use std::net::TcpStream;
use std::sync::mpsc;

use tungstenite::client::{IntoClientRequest, connect_with_config};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message as WsMessage, WebSocket};

use chromiumoxide_types::{
    CallId, CdpJsonEventMessage, Command, Message as CdpMessage, MethodCall,
};

use crate::error::{CdpError, Result};

/// A blocking WebSocket connection speaking the Chrome DevTools Protocol.
///
/// Correlates outgoing [`MethodCall`]s with their [`chromiumoxide_types::Response`]
/// by [`CallId`]. Any server-pushed CDP events observed while a `send` is
/// in flight are forwarded to the event channel returned by [`Connection::connect`].
///
/// Single-threaded, single-in-flight: this API assumes at most one `send` is
/// executing at a time.
#[derive(Debug)]
pub struct Connection {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    next_id: usize,
    event_tx: mpsc::Sender<CdpJsonEventMessage>,
}

impl Connection {
    /// Blocking WebSocket connect. Returns the connection together with the
    /// receiver end of a channel that receives every unsolicited CDP event
    /// observed while [`Connection::send`] is reading frames.
    pub fn connect(
        url: impl IntoClientRequest,
    ) -> Result<(Self, mpsc::Receiver<CdpJsonEventMessage>)> {
        let config = WebSocketConfig::default()
            .max_message_size(None)
            .max_frame_size(None);
        let (ws, _resp) = connect_with_config(url, Some(config), 3)?;

        let (event_tx, event_rx) = mpsc::channel();
        Ok((
            Self {
                ws,
                next_id: 0,
                event_tx,
            },
            event_rx,
        ))
    }

    fn next_call_id(&mut self) -> CallId {
        let id = CallId::new(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    /// Send a command and block until its matching response arrives.
    ///
    /// CDP events read while waiting are forwarded through the event channel
    /// (silently dropped if the receiver has been dropped).
    pub fn send<T: Command>(
        &mut self,
        cmd: T,
        session_id: Option<String>,
    ) -> Result<T::Response> {
        let call_id = self.next_call_id();
        let call = MethodCall {
            id: call_id,
            method: cmd.identifier(),
            session_id,
            params: serde_json::to_value(&cmd)?,
        };
        let payload = serde_json::to_string(&call)?;
        self.ws.send(WsMessage::text(payload))?;

        loop {
            match self.ws.read()? {
                WsMessage::Text(text) => {
                    let parsed: CdpMessage<CdpJsonEventMessage> =
                        serde_json::from_str(text.as_str()).map_err(|e| {
                            CdpError::InvalidMessage(text.as_str().to_string(), e)
                        })?;
                    match parsed {
                        CdpMessage::Response(resp) => {
                            if resp.id != call_id {
                                return Err(CdpError::ResponseIdMismatch {
                                    expected: call_id,
                                    got: resp.id,
                                });
                            }
                            if let Some(err) = resp.error {
                                return Err(err.into());
                            }
                            let result =
                                resp.result.unwrap_or(serde_json::Value::Null);
                            return Ok(T::response_from_value(result)?);
                        }
                        CdpMessage::Event(ev) => {
                            let _ = self.event_tx.send(ev);
                        }
                    }
                }
                WsMessage::Ping(payload) => {
                    self.ws.send(WsMessage::Pong(payload))?;
                }
                WsMessage::Pong(_) => {}
                WsMessage::Close(_) => return Err(CdpError::ConnectionClosed),
                other @ (WsMessage::Binary(_) | WsMessage::Frame(_)) => {
                    return Err(CdpError::UnexpectedWsMessage(other));
                }
            }
        }
    }

    /// Close the WebSocket cleanly.
    pub fn close(mut self) -> Result<()> {
        self.ws.close(None)?;
        Ok(())
    }
}
