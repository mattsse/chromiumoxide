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
use serde_json::{Error, Value};
use smallvec::alloc::borrow::Borrow;

use chromiumoxid_types::{
    CallId, CdpJsonEventMessage, Command, CommandResponse, Event, Message, Method, Response,
};

use crate::{
    browser::{BrowserMessage, CommandMessage},
    cdp::{
        browser_protocol::{browser::*, fetch::*, network::*, page::*, target::*},
        events::CdpEvent,
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
    pending_commands: FnvHashMap<CallId, (PendingRequest, Instant)>,
    from_tabs: Vec<Fuse<Receiver<CommandMessage>>>,
    from_browser: Fuse<Receiver<HandlerMessage>>,
    // default_ctx: BrowserContext,
    contexts: FnvHashMap<BrowserContextId, BrowserContext>,
    /// The created and attached targets
    targets: FnvHashMap<TargetId, Target>,
    /// Keeps track of all the current active sessions
    ///
    /// There can be multiple sessions per target.
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
        if let Some((req, _)) = self.pending_commands.remove(&resp.id) {
            match req {
                PendingRequest::CreatePage(tx) => {
                    match to_command_response::<CreateTargetParams>(resp) {
                        Ok(resp) => {
                            if let Some(target) = self.targets.get_mut(&resp.target_id) {
                                target.set_initiator(tx);
                            } else {
                                // TODO can this even happen?
                                panic!("Created target not present")
                            }
                        }
                        Err(err) => {
                            tx.send(Err(err));
                        }
                    }
                }
                PendingRequest::ExternalCommand(tx) => {
                    tx.send(resp);
                }
                PendingRequest::InternalCommand(target_id) => {
                    if let Some(target) = self.targets.get_mut(&target_id) {
                        target.on_response(resp);
                    }
                }
            }
        }
    }

    pub(crate) fn submit_command(&mut self, msg: CommandMessage) -> Result<(), CdpError> {
        let call_id = self
            .conn
            .submit_command(msg.method, msg.session_id, msg.params)?;
        self.pending_commands.insert(
            call_id,
            (PendingRequest::ExternalCommand(msg.sender), Instant::now()),
        );
        Ok(())
    }

    // Is `sessionfactory` that sends `Target.attachToTarget`
    /// Creates a new session attached to the target.
    fn create_session(&mut self, id: TargetId) {
        // attach to target flatten
    }

    /// Create a new page and send it to the receiver
    fn create_page(
        &mut self,
        params: CreateTargetParams,
        tx: OneshotSender<Result<Page, CdpError>>,
    ) {
        let method = params.identifier();
        match serde_json::to_value(params) {
            Ok(params) => match self.conn.submit_command(method, None, params) {
                Ok(call_id) => {
                    self.pending_commands
                        .insert(call_id, (PendingRequest::CreatePage(tx), Instant::now()));
                }
                Err(err) => {
                    tx.send(Err(err.into()));
                }
            },
            Err(err) => {
                tx.send(Err(err.into()));
            }
        }
        // 1. Target.createTarget
        // 2. initialize target
        // 3. create session
        // 4. initialize page
    }

    fn on_event(&mut self, event: CdpEventMessage) {
        if let Some(ref session_id) = event.session_id {
            if let Some(session) = self.sessions.get(session_id) {
                if let Some(target) = self.targets.get_mut(session.target_id()) {
                    return target.on_event(event);
                }
            }
        }
        match event.params {
            CdpEvent::TargetTargetCreated(ev) => self.on_target_created(ev),
            CdpEvent::TargetAttachedToTarget(ev) => self.on_attached_to_target(ev),
            CdpEvent::TargetTargetDestroyed(ev) => self.on_target_destroyed(ev),
            _ => {}
        }
    }

    fn on_target_created(&mut self, event: EventTargetCreated) {
        let target = Target::new(event.target_info);
        self.targets.insert(target.target_id().clone(), target);
    }

    fn on_attached_to_target(&mut self, event: EventAttachedToTarget) {
        let session = Session::new(
            event.session_id,
            event.target_info.r#type,
            event.target_info.target_id,
        );
        if let Some(target) = self.targets.get_mut(session.target_id()) {
            target.set_session_id(session.session_id().clone())
        }
    }

    /// The session was detached from target.
    /// Can be issued multiple times per target if multiple session have been
    /// attached to it.
    fn on_detached_from_target(&mut self, event: EventDetachedFromTarget) {
        // remove the session
        if let Some(session) = self.sessions.remove(&event.session_id) {
            if let Some(target) = self.targets.get_mut(session.target_id()) {
                target.session_id().take();
            }
        }
    }

    fn on_target_destroyed(&mut self, event: EventTargetDestroyed) {
        if let Some(target) = self.targets.remove(&event.target_id) {
            // TODO shutdown?
            if let Some(session) = target.session_id() {
                self.sessions.remove(session);
            }
        }
    }
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

enum PendingRequest {
    CreatePage(OneshotSender<Result<Page, CdpError>>),
    ExternalCommand(OneshotSender<Response>),
    InternalCommand(TargetId),
}

/// Events used internally to communicate with the handler, which are executed
/// in the background
// TODO rename to BrowserMessage
pub(crate) enum HandlerMessage {
    CreatePage(CreateTargetParams, OneshotSender<Result<Page, CdpError>>),
    GetPages(OneshotSender<Vec<Page>>),
    Command(CommandMessage),
    Subscribe,
}

pub(crate) fn to_command_response<T: Command>(
    resp: Response,
) -> Result<CommandResponse<T::Response>, CdpError> {
    if let Some(res) = resp.result {
        let result = serde_json::from_value(res)?;
        Ok(CommandResponse {
            id: resp.id,
            result,
            method: resp.method,
        })
    } else if let Some(err) = resp.error {
        Err(err.into())
    } else {
        Err(CdpError::NoResponse)
    }
}
