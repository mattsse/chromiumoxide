use std::borrow::Cow;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use fnv::FnvHashMap;
use futures::channel::mpsc::Receiver;
use futures::channel::oneshot::Sender as OneshotSender;
use futures::stream::{Fuse, Stream};
use futures::task::{Context, Poll};
use futures::StreamExt;
use serde_json::{Error, Value};
use smallvec::alloc::borrow::Borrow;
use smallvec::alloc::collections::{BTreeMap, VecDeque};

use chromiumoxid_types::{
    CallId, CdpJsonEventMessage, Command, CommandResponse, Event, Message, Method, Response,
};

use crate::handler::frame::{NavigationError, NavigationId, NavigationOk};
use crate::page::PageInner;
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

/// Standard timeout in MS
pub const REQUEST_TIMEOUT: u64 = 30000;

mod browser;
mod cmd;
mod emulation;
mod frame;
mod job;
mod network;
mod page;
mod session;
mod target;
mod viewport;

// puppeteer
pub struct Handler {
    /// Commands that are being processed await a response from the chromium
    /// instance
    pending_commands: FnvHashMap<CallId, (PendingRequest, Instant)>,
    from_browser: Fuse<Receiver<HandlerMessage>>,
    // default_ctx: BrowserContext,
    contexts: HashMap<BrowserContextId, BrowserContext>,
    pages: Vec<(Fuse<Receiver<HandlerMessage>>, Arc<PageInner>)>,
    /// The created and attached targets
    targets: HashMap<TargetId, Target>,
    navigations: FnvHashMap<NavigationId, NavigationRequest>,
    /// Keeps track of all the current active sessions
    ///
    /// There can be multiple sessions per target.
    sessions: HashMap<SessionId, Session>,
    /// The websocket connection to the chromium instance
    conn: Connection<CdpEventMessage>,
    evict_command_timeout: PeriodicJob,
    /// The internal identifier for a specific navigation
    next_navigation_id: usize,
}

impl Handler {
    pub(crate) fn new(mut conn: Connection<CdpEventMessage>, rx: Receiver<HandlerMessage>) -> Self {
        let discover = SetDiscoverTargetsParams::new(true);
        conn.submit_command(
            discover.identifier(),
            None,
            serde_json::to_value(discover).unwrap(),
        );

        Self {
            pending_commands: Default::default(),
            from_browser: rx.fuse(),
            contexts: Default::default(),
            pages: Default::default(),
            targets: Default::default(),
            navigations: Default::default(),
            sessions: Default::default(),
            conn,
            evict_command_timeout: Default::default(),
            next_navigation_id: 0,
        }
    }

    fn on_navigation_response(&mut self, id: NavigationId, resp: Response) {
        if let Some(nav) = self.navigations.get_mut(&id) {
            nav.set_response(resp);
        }
    }

    fn on_navigation_lifecycle_completed(&mut self, event: Result<NavigationOk, NavigationError>) {}

    fn on_response(&mut self, resp: Response) {
        if let Some((req, _)) = self.pending_commands.remove(&resp.id) {
            match req {
                PendingRequest::CreateTarget(tx) => {
                    match to_command_response::<CreateTargetParams>(resp) {
                        Ok(resp) => {
                            if let Some(target) = self.targets.get_mut(&resp.target_id) {

                                // TODO submit navigation request to the target
                                // and store navid -> tx in navigations
                            } else {
                                // TODO can this even happen?
                                panic!("Created target not present")
                            }
                        }
                        Err(err) => {
                            let _ = tx.send(Err(err)).ok();
                        }
                    }
                }
                PendingRequest::Navigate(id) => {
                    self.on_navigation_response(id, resp);
                }
                PendingRequest::ExternalCommand(tx) => {
                    let _ = tx.send(resp).ok();
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

    fn next_navigation_id(&mut self) -> NavigationId {
        let id = NavigationId(self.next_navigation_id);
        self.next_navigation_id = self.next_navigation_id.wrapping_add(1);
        id
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
                        .insert(call_id, (PendingRequest::CreateTarget(tx), Instant::now()));
                }
                Err(err) => {
                    let _ = tx.send(Err(err.into())).ok();
                }
            },
            Err(err) => {
                let _ = tx.send(Err(err.into())).ok();
            }
        }
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

impl Stream for Handler {
    type Item = Result<CdpEventMessage, CdpError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let pin = self.get_mut();

        // temporary pinning of the browser receiver should be safe as we are pinning
        // through the already pinned self. with the receivers we can also
        // safely ignore exhaustion as those are fused.
        while let Poll::Ready(Some(msg)) = Pin::new(&mut pin.from_browser).poll_next(cx) {
            match msg {
                HandlerMessage::Command(cmd) => {
                    pin.submit_command(cmd).unwrap();
                }
                _ => {}
            }
        }

        'outer: for n in (0..pin.pages.len()).rev() {
            let (mut tab, inner) = pin.pages.swap_remove(n);
            loop {
                match Pin::new(&mut tab).poll_next(cx) {
                    Poll::Ready(Some(msg)) => {
                        // TODO handle err
                        // pin.submit_command(msg).unwrap();
                    }
                    Poll::Ready(None) => {
                        // channel is done, skip
                        continue 'outer;
                    }
                    Poll::Pending => break,
                }
            }
            pin.pages.push((tab, inner));
        }

        while let Poll::Ready(Some(ev)) = Pin::new(&mut pin.conn).poll_next(cx) {
            match ev {
                Ok(Message::Response(resp)) => pin.on_response(resp),
                Ok(Message::Event(ev)) => {
                    pin.on_event(ev);
                }
                Err(err) => return Poll::Ready(Some(Err(err))),
            }
        }

        Poll::Pending
    }
}

#[derive(Debug)]
pub struct NavigationInProgress<T> {
    response: Option<Response>,
    tx: OneshotSender<T>,
}

impl<T> NavigationInProgress<T> {
    fn set_response(&mut self, resp: Response) {
        self.response = Some(resp);
    }
}

#[derive(Debug)]
enum NavigationRequest {
    CreatePage(NavigationInProgress<Result<Page, CdpError>>),
    Goto(NavigationInProgress<Response>),
}

impl NavigationRequest {
    fn set_response(&mut self, response: Response) {
        match self {
            NavigationRequest::CreatePage(nav) => nav.set_response(response),
            NavigationRequest::Goto(nav) => nav.set_response(response),
        }
    }
}

#[derive(Debug)]
enum PendingRequest {
    CreateTarget(OneshotSender<Result<Page, CdpError>>),
    Navigate(NavigationId),
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
