use std::collections::VecDeque;
use std::io::{PipeReader, PipeWriter, Write};
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::ready;

use async_tungstenite::tungstenite::Message as WsMessage;
use async_tungstenite::{WebSocketStream, tungstenite::protocol::WebSocketConfig};
use futures::stream::Stream;
use futures::task::{Context, Poll};
use futures::{SinkExt, StreamExt};
use tokio_util::io::ReaderStream;

use async_tungstenite::tokio::ConnectStream;
use chromiumoxide_cdp::cdp::browser_protocol::target::SessionId;
use chromiumoxide_types::{CallId, EventMessage, Message, MethodCall, MethodId};

use crate::error::CdpError;
use crate::error::Result;

#[derive(Debug)]
enum ConnectionTransport<T: EventMessage> {
    Websocket(WebSocketStream<ConnectStream>),
    Pipes {
        writer: PipeWriter,
        reader: ReaderStream<tokio::net::unix::pipe::Receiver>,
        read_buf: Vec<u8>,
        _phantom: PhantomData<T>,
    },
}

use futures::Sink;

impl<T: EventMessage + Unpin> Sink<MethodCall> for ConnectionTransport<T> {
    type Error = CdpError;

    fn poll_ready(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        match self.get_mut() {
            ConnectionTransport::Websocket(ws) => Pin::new(ws).poll_ready(cx).map_err(CdpError::Ws),
            ConnectionTransport::Pipes { .. } => Poll::Ready(Ok(())),
        }
    }

    fn start_send(self: Pin<&mut Self>, item: MethodCall) -> std::result::Result<(), Self::Error> {
        match self.get_mut() {
            ConnectionTransport::Websocket(ws) => {
                let msg = serde_json::to_string(&item)?;
                ws.start_send_unpin(msg.into()).map_err(CdpError::Ws)
            }
            ConnectionTransport::Pipes { writer: write, .. } => {
                let msg = serde_json::to_string(&item)?;
                write.write_all(msg.as_bytes())?;
                // Nul byte needed to indicate end of message
                write.write_all(&[0])?;
                Ok(())
            }
        }
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        match self.get_mut() {
            ConnectionTransport::Websocket(ws) => Pin::new(ws).poll_flush(cx).map_err(CdpError::Ws),
            ConnectionTransport::Pipes { writer, .. } => {
                writer.flush()?;
                Poll::Ready(Ok(()))
            }
        }
    }

    fn poll_close(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        match self.get_mut() {
            ConnectionTransport::Websocket(ws) => Pin::new(ws).poll_close(cx).map_err(CdpError::Ws),
            ConnectionTransport::Pipes { .. } => Poll::Ready(Ok(())),
        }
    }
}

impl<T: EventMessage + Unpin> Stream for ConnectionTransport<T> {
    type Item = Result<Message<T>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.get_mut() {
            ConnectionTransport::Websocket(ws) => {
                match ready!(ws.poll_next_unpin(cx)) {
                    Some(Ok(WsMessage::Text(text))) => {
                        let ready = match serde_json::from_str::<Message<T>>(&text) {
                            Ok(msg) => {
                                tracing::trace!("Received {:?}", msg);
                                Ok(msg)
                            }
                            Err(err) => {
                                let msg = text.as_str().to_string();
                                tracing::debug!(target: "chromiumoxide::conn::raw_ws::parse_errors", msg, "Failed to parse raw WS message {}", err);
                                Err(CdpError::InvalidMessage(msg, err))
                            }
                        };
                        Poll::Ready(Some(ready))
                    }
                    Some(Ok(WsMessage::Close(_))) => Poll::Ready(None),
                    // ignore ping and pong
                    Some(Ok(WsMessage::Ping(_))) | Some(Ok(WsMessage::Pong(_))) => {
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                    Some(Ok(msg)) => Poll::Ready(Some(Err(CdpError::UnexpectedWsMessage(msg)))),
                    Some(Err(err)) => Poll::Ready(Some(Err(CdpError::Ws(err)))),
                    None => {
                        // ws connection closed
                        Poll::Ready(None)
                    }
                }
            }
            ConnectionTransport::Pipes {
                reader, read_buf, ..
            } => {
                /// Attempt to get the next message from the buffer
                /// Returns None if there is no null byte found, indicating
                /// that there is no complete message.
                /// If there is a potential complete message, this function will try to
                /// deserialize it, returning the result of deserialization.
                fn try_get_message<T>(
                    read_buf: &mut Vec<u8>,
                ) -> Option<Result<Message<T>, CdpError>>
                where
                    T: EventMessage + Unpin,
                {
                    if let Some(index) = read_buf.iter().position(|x| *x == 0) {
                        let mut msg_buf = read_buf.split_off(index + 1);
                        std::mem::swap(&mut msg_buf, read_buf);

                        msg_buf.truncate(msg_buf.len() - 1);
                        let ready = match serde_json::from_slice::<Message<T>>(&msg_buf) {
                            Ok(msg) => {
                                tracing::trace!("Received {:?}", msg);
                                Ok(msg)
                            }
                            Err(err) => {
                                let msg = String::from_utf8_lossy(&msg_buf).into_owned();
                                tracing::debug!(target: "chromiumoxide::conn::pipes::parse_errors", msg, "Failed to parse raw pipe message {}", err);
                                Err(CdpError::InvalidMessage(msg, err))
                            }
                        };
                        return Some(ready);
                    }
                    None
                }

                if let Some(message_result) = try_get_message(read_buf) {
                    return Poll::Ready(Some(message_result));
                }

                loop {
                    match ready!(reader.poll_next_unpin(cx)) {
                        Some(Ok(bytes)) => {
                            read_buf.extend_from_slice(&bytes);
                            if let Some(message_result) = try_get_message(read_buf) {
                                return Poll::Ready(Some(message_result));
                            }
                        }
                        Some(Err(err)) => return Poll::Ready(Some(Err(CdpError::Io(err)))),
                        None => return Poll::Ready(None),
                    }
                }
            }
        }
    }
}

/// Exchanges the messages with the websocket
#[must_use = "streams do nothing unless polled"]
#[derive(Debug)]
pub struct Connection<T: EventMessage> {
    /// Queue of commands to send.
    pending_commands: VecDeque<MethodCall>,
    /// The connection transport of the chromium instance
    connection_transport: ConnectionTransport<T>,
    /// The identifier for a specific command
    next_id: usize,
    needs_flush: bool,
    /// The message that is currently being proceessed
    pending_flush: Option<MethodCall>,
}

impl<T: EventMessage + Unpin> Connection<T> {
    pub async fn connect(debug_ws_url: impl AsRef<str>) -> Result<Self> {
        let config = WebSocketConfig::default()
            .max_message_size(None)
            .max_frame_size(None);

        let (ws, _) = async_tungstenite::tokio::connect_async_with_config(
            debug_ws_url.as_ref(),
            Some(config),
        )
        .await?;

        Ok(Self {
            pending_commands: Default::default(),
            connection_transport: ConnectionTransport::Websocket(ws),
            next_id: 0,
            needs_flush: false,
            pending_flush: None,
        })
    }

    pub async fn connect_with_pipes(writer: PipeWriter, reader: PipeReader) -> Result<Self> {
        Ok(Self {
            pending_commands: Default::default(),
            connection_transport: ConnectionTransport::Pipes {
                writer,
                reader: ReaderStream::new(
                    tokio::net::unix::pipe::Receiver::from_owned_fd(reader.into()).unwrap(),
                ),
                read_buf: vec![],
                _phantom: Default::default(),
            },
            next_id: 0,
            needs_flush: false,
            pending_flush: None,
        })
    }
}

impl<T: EventMessage + Unpin> Connection<T> {
    fn next_call_id(&mut self) -> CallId {
        let id = CallId::new(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    /// Queue in the command to send over the socket and return the id for this
    /// command
    pub fn submit_command(
        &mut self,
        method: MethodId,
        session_id: Option<SessionId>,
        params: serde_json::Value,
    ) -> serde_json::Result<CallId> {
        let id = self.next_call_id();
        let call = MethodCall {
            id,
            method,
            session_id: session_id.map(Into::into),
            params,
        };
        self.pending_commands.push_back(call);
        Ok(id)
    }

    /// flush any processed message and start sending the next over the conn
    /// sink
    fn start_send_next(&mut self, cx: &mut Context<'_>) -> Result<()> {
        if self.needs_flush {
            if let Poll::Ready(Ok(())) = self.connection_transport.poll_flush_unpin(cx) {
                self.needs_flush = false;
            }
        }
        if self.pending_flush.is_none() && !self.needs_flush {
            if let Some(cmd) = self.pending_commands.pop_front() {
                tracing::trace!("Sending {:?}", cmd);
                self.connection_transport.start_send_unpin(cmd.clone())?;
                self.pending_flush = Some(cmd);
            }
        }
        Ok(())
    }
}

impl<T: EventMessage + Unpin> Stream for Connection<T> {
    type Item = Result<Message<T>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let pin = self.get_mut();

        loop {
            // queue in the next message if not currently flushing
            if let Err(err) = pin.start_send_next(cx) {
                return Poll::Ready(Some(Err(err)));
            }

            // send the message
            if let Some(call) = pin.pending_flush.take() {
                if pin.connection_transport.poll_ready_unpin(cx).is_ready() {
                    pin.needs_flush = true;
                    // try another flush
                    continue;
                } else {
                    pin.pending_flush = Some(call);
                }
            }

            break;
        }

        pin.connection_transport.poll_next_unpin(cx)
    }
}
