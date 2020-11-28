use std::borrow::Cow;
use std::collections::HashMap;
use std::pin::Pin;
use std::time::Instant;

use fnv::FnvHashMap;
use futures::channel::mpsc::Receiver;
use futures::channel::oneshot::Sender as OneshotSender;
use futures::stream::{Fuse, Stream};
use futures::task::{Context, Poll};
use futures::StreamExt;

use chromeoxid_types::{CallId, CdpJsonEventMessage, Event, Message, Response};

use crate::{
    browser::{BrowserMessage, CommandMessage},
    cdp::{
        browser_protocol::{browser::*, fetch::*, network::*, page::*, target::*},
        events::CdpEventMessage,
        js_protocol::runtime::*,
    },
    conn::Connection,
    error::CdpError,
    handler::{browser::BrowserContext, job::PeriodicJob, session::Session, target::Target},
    page::Page,
};

/// Navigation timeout in MS
pub const NAVIGATION_TIMEOUT: u64 = 30000;

// TODO move logic inside, this is essentially the `Browser` class from
// puppeteer
pub struct Handler2 {
    /// Commands that are being processed await a response from the chromium
    /// instance
    pending_commands: FnvHashMap<CallId, (OneshotSender<Response>, Instant)>,
    from_tabs: Vec<Fuse<Receiver<CommandMessage>>>,
    from_browser: Fuse<Receiver<HandlerMessage>>,
    // default_ctx: BrowserContext,
    contexts: FnvHashMap<BrowserContextId, BrowserContext>,
    targets: FnvHashMap<TargetId, Target>,
    sessions: FnvHashMap<SessionId, Session>,
    /// The websocket connection to the chromium instance
    conn: Connection<CdpEventMessage>,
    evict_command_timeout: PeriodicJob,
}

// Design: commands get called via channels from the client side, incoming
// events alter the state of the tracked objects. Incoming requests can be
// delayed until the incoming events have changed the state of the sending
// object for example navigating: the response to a navigation request comes
// before the event that the browser finished navigating. Requires an additional
// FIFO buffer for responses. `Browser` si the main entry point on the user
// side, it creates new tabs etc.
impl Handler2 {
    // pub(crate) fn new(conn: Connection<T>, rx: Receiver<BrowserMessage>) -> Self
    // {     todo!()
    // }

    fn on_response(&mut self, resp: Response) {
        if let Some((ret, _)) = self.pending_commands.remove(&resp.id) {
            ret.send(resp).ok();
        }
    }

    pub(crate) fn submit_command(&mut self, msg: CommandMessage) -> Result<(), CdpError> {
        let call_id = self
            .conn
            .submit_command(msg.method, msg.session_id, msg.params)?;
        self.pending_commands
            .insert(call_id, (msg.sender, Instant::now()));
        Ok(())
    }

    // Is `sessionfactory` that sends `Target.attachToTarget`
    /// Creates a new session attached to the target.
    fn create_session(&mut self, id: TargetId) {
        // attach to target flatten
    }

    /// Create a new page
    fn create_page(&mut self, params: CreateTargetParams, tx: OneshotSender<Page>) {
        // 1. Target.createTarget
        // 2. initialize target
        // 3. create session
        // 4. initialize page
    }

    fn on_event(&mut self, event: CdpEventMessage) {}

    fn on_target_created(&mut self, event: EventTargetCreated) {
        // TODO create new Target instance, store with target id
        // TODO initialize Target
        // create new session for this target
        // TODO initialize target
    }

    fn on_attached_to_target(&mut self, event: EventAttachedToTarget) {
        // create new session for event.target_id
        // frame manager on_frame_moved
    }

    /// The session was detached from target.
    /// Can be issued multiple times per target if multiple session have been
    /// attached to it.
    fn on_detached_from_target(&mut self, event: EventDetachedFromTarget) {
        // remove the session
    }

    fn on_target_destroyed(&mut self, event: EventTargetDestroyed) {
        // remove the target from store
    }

    // network manager events

    fn on_fetch_request_paused(&mut self, event: EventRequestPaused) {}

    fn on_fetch_auth_required(&mut self, event: EventAuthRequired) {}
    fn on_request_will_be_sent(&mut self, event: EventRequestWillBeSent) {}
    fn on_request_served_from_cache(&mut self, event: EventRequestServedFromCache) {}
    fn on_response_received(&mut self, event: EventResponseReceived) {}
    fn on_network_loading_finished(&mut self, event: EventLoadingFinished) {}
    fn on_network_loading_failed(&mut self, event: EventLoadingFailed) {}
}

impl Stream for Handler2 {
    type Item = Result<CdpEventMessage, CdpError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let pin = self.get_mut();

        // temporary pinning of the browser receiver should be safe as we are pinning
        // through the already pinned self. with the receivers we can also
        // safely ignore exhaustion as those are fused.
        // while let Poll::Ready(Some(msg)) = Pin::new(&mut
        // pin.from_browser).poll_next(cx) {     match msg {
        //         BrowserMessage::Command(cmd) => {
        //             pin.submit_command(cmd).unwrap();
        //         }
        //         BrowserMessage::RegisterTab(recv) => {
        //             pin.from_tabs.push(recv.fuse());
        //         }
        //     }
        // }
        //
        // 'outer: for n in (0..pin.from_tabs.len()).rev() {
        //     let mut tab = pin.from_tabs.swap_remove(n);
        //     loop {
        //         match Pin::new(&mut tab).poll_next(cx) {
        //             Poll::Ready(Some(msg)) => {
        //                 // TODO handle err
        //                 pin.submit_command(msg).unwrap();
        //             }
        //             Poll::Ready(None) => {
        //                 // channel is done, skip
        //                 continue 'outer;
        //             }
        //             Poll::Pending => break,
        //         }
        //     }
        //     pin.from_tabs.push(tab);
        // }
        //
        // while let Poll::Ready(Some(ev)) = Pin::new(&mut pin.conn).poll_next(cx) {
        //     match ev {
        //         Ok(Message::Response(resp)) => pin.on_response(resp),
        //         Ok(Message::Event(ev)) => {
        //             pin.on_event(ev);
        //         }
        //         Err(err) => return Poll::Ready(Some(Err(err))),
        //     }
        // }

        Poll::Pending
    }
}

/// Events used internally to communicate with the handler, which are executed
/// in the background
pub(crate) enum HandlerMessage {
    CreatePage(CreateTargetParams, OneshotSender<Result<Page, CdpError>>),
    GetPages(OneshotSender<Vec<Page>>),
    Command(CommandMessage),
    Subscribe,
}
