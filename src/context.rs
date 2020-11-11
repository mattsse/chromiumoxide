use std::pin::Pin;

use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::channel::oneshot::Sender as OneshotSender;
use futures::stream::{Fuse, Stream};
use futures::task::{Context, Poll};
use futures::StreamExt;

use chromeoxid_types::{CallId, CdpEvent, Event, Message, Response};

use crate::browser::{BrowserMessage, CommandMessage};
use crate::conn::Connection;
use crate::error::CdpError;
use std::collections::HashMap;

pub struct CdpFuture<T: Event = CdpEvent> {
    pending_commands: HashMap<CallId, OneshotSender<Response>>,
    from_tabs: Vec<Fuse<Receiver<CommandMessage>>>,
    from_browser: Fuse<Receiver<BrowserMessage>>,
    conn: Connection<T>,
}

impl<T: Event> CdpFuture<T> {
    pub(crate) fn new(conn: Connection<T>, rx: Receiver<BrowserMessage>) -> Self {
        Self {
            pending_commands: Default::default(),
            from_tabs: vec![],
            from_browser: rx.fuse(),
            conn,
        }
    }

    fn respond(&mut self, resp: Response) {
        if let Some(ret) = self.pending_commands.remove(&resp.id) {
            ret.send(resp).ok();
        }
    }

    fn submit_command(&mut self, msg: CommandMessage) -> Result<(), CdpError> {
        let call_id = self.conn.submit_command(msg.method, msg.params)?;
        self.pending_commands.insert(call_id, msg.sender);
        Ok(())
    }
}

impl<T: Event + Unpin> Stream for CdpFuture<T> {
    type Item = Result<T, CdpError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let pin = self.get_mut();

        loop {
            if let Poll::Ready(Some(ev)) = Pin::new(&mut pin.conn).poll_next(cx) {
                match ev {
                    Ok(Message::Response(resp)) => pin.respond(resp),
                    Ok(Message::Event(ev)) => return Poll::Ready(Some(Ok(ev))),
                    Err(err) => return Poll::Ready(Some(Err(err))),
                }
            }

            // temporary pinning of the browser receiver should be safe as we are pinning
            // through the already pinned self. with the receivers we can also
            // safely ignore exhaustion as those are fused.
            while let Poll::Ready(Some(msg)) = Pin::new(&mut pin.from_browser).poll_next(cx) {
                match msg {
                    BrowserMessage::Command(cmd) => {
                        pin.submit_command(cmd);
                    }
                    BrowserMessage::RegisterTab(recv) => {
                        pin.from_tabs.push(recv.fuse());
                    }
                }
            }

            for n in (0..pin.from_tabs.len()).rev() {
                let mut tab = pin.from_tabs.swap_remove(n);
                while let Poll::Ready(Some(msg)) = Pin::new(&mut tab).poll_next(cx) {
                    pin.submit_command(msg);
                }
                pin.from_tabs.push(tab);
            }
        }
    }
}
