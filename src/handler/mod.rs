use std::collections::HashMap;
use std::pin::Pin;
use std::time::Instant;

use fnv::FnvHashMap;
use futures::channel::mpsc::Receiver;
use futures::channel::oneshot::Sender as OneshotSender;
use futures::stream::{Fuse, Stream, StreamExt};
use futures::task::{Context, Poll};

use chromiumoxide_cdp::cdp::browser_protocol::browser::*;
use chromiumoxide_cdp::cdp::browser_protocol::target::*;
use chromiumoxide_cdp::cdp::events::CdpEvent;
use chromiumoxide_cdp::cdp::events::CdpEventMessage;
use chromiumoxide_types::{CallId, Message, Method, Response};
use chromiumoxide_types::{MethodId, Request as CdpRequest};
pub(crate) use page::PageInner;

use crate::cmd::{to_command_response, CommandMessage};
use crate::conn::Connection;
use crate::error::Result;
use crate::handler::browser::BrowserContext;
use crate::handler::frame::FrameNavigationRequest;
use crate::handler::frame::{NavigationError, NavigationId, NavigationOk};
use crate::handler::job::PeriodicJob;
use crate::handler::session::Session;
use crate::handler::target::Target;
use crate::handler::target::TargetEvent;
use crate::page::Page;

/// Standard timeout in MS
pub const REQUEST_TIMEOUT: u64 = 30_000;

mod browser;
pub mod emulation;
pub mod frame;
mod job;
pub mod network;
mod page;
mod session;
pub mod target;
mod viewport;

/// The handler that monitors the state of the chromium browser and drives all
/// the requests and events.
#[must_use = "streams do nothing unless polled"]
#[derive(Debug)]
pub struct Handler {
    /// Commands that are being processed await a response from the chromium
    /// instance
    pending_commands: FnvHashMap<CallId, (PendingRequest, MethodId, Instant)>,
    /// Connection to the browser instance
    from_browser: Fuse<Receiver<HandlerMessage>>,
    // default_ctx: BrowserContext,
    contexts: HashMap<BrowserContextId, BrowserContext>,
    /// Used to loop over all targets in a consistent manner
    target_ids: Vec<TargetId>,
    /// The created and attached targets
    targets: HashMap<TargetId, Target>,
    /// Currently queued in navigations for targets
    navigations: FnvHashMap<NavigationId, NavigationRequest>,
    /// Keeps track of all the current active sessions
    ///
    /// There can be multiple sessions per target.
    sessions: HashMap<SessionId, Session>,
    /// The websocket connection to the chromium instance
    conn: Connection<CdpEventMessage>,
    /// Evicts timed out requests periodically
    evict_command_timeout: PeriodicJob,
    /// The internal identifier for a specific navigation
    next_navigation_id: usize,
}

impl Handler {
    /// Create a new `Handler` that drives the connection and listens for
    /// messages on the receiver `rx`.
    pub(crate) fn new(mut conn: Connection<CdpEventMessage>, rx: Receiver<HandlerMessage>) -> Self {
        let discover = SetDiscoverTargetsParams::new(true);
        let _ = conn.submit_command(
            discover.identifier(),
            None,
            serde_json::to_value(discover).unwrap(),
        );

        Self {
            pending_commands: Default::default(),
            from_browser: rx.fuse(),
            contexts: Default::default(),
            target_ids: Default::default(),
            targets: Default::default(),
            navigations: Default::default(),
            sessions: Default::default(),
            conn,
            evict_command_timeout: Default::default(),
            next_navigation_id: 0,
        }
    }

    /// Return the target with the matching `target_id`
    pub fn get_target(&self, target_id: &TargetId) -> Option<&Target> {
        self.targets.get(target_id)
    }

    /// Iterator over all currently attached targets
    pub fn targets(&self) -> impl Iterator<Item = &Target> + '_ {
        self.targets.values()
    }

    /// received a response to a navigation request like `Page.navigate`
    fn on_navigation_response(&mut self, id: NavigationId, resp: Response) {
        if let Some(nav) = self.navigations.remove(&id) {
            match nav {
                NavigationRequest::Navigate(mut nav) => {
                    if nav.navigated {
                        let _ = nav.tx.send(Ok(resp));
                    } else {
                        nav.set_response(resp);
                        self.navigations
                            .insert(id, NavigationRequest::Navigate(nav));
                    }
                }
            }
        }
    }

    /// A navigation has finished.
    fn on_navigation_lifecycle_completed(&mut self, res: Result<NavigationOk, NavigationError>) {
        match res {
            Ok(ok) => {
                let id = *ok.navigation_id();
                if let Some(nav) = self.navigations.remove(&id) {
                    match nav {
                        NavigationRequest::Navigate(mut nav) => {
                            if let Some(resp) = nav.response.take() {
                                let _ = nav.tx.send(Ok(resp));
                            } else {
                                nav.set_navigated();
                                self.navigations
                                    .insert(id, NavigationRequest::Navigate(nav));
                            }
                        }
                    }
                }
            }
            Err(err) => {
                if let Some(nav) = self.navigations.remove(err.navigation_id()) {
                    match nav {
                        NavigationRequest::Navigate(nav) => {
                            let _ = nav.tx.send(Err(err.into()));
                        }
                    }
                }
            }
        }
    }

    /// Received a response to a request.
    fn on_response(&mut self, resp: Response) {
        if let Some((req, method, _)) = self.pending_commands.remove(&resp.id) {
            match req {
                PendingRequest::CreateTarget(tx) => {
                    match to_command_response::<CreateTargetParams>(resp, method) {
                        Ok(resp) => {
                            if let Some(target) = self.targets.get_mut(&resp.target_id) {
                                // move the sender to the target that sends its page once
                                // initialized
                                target.set_initiator(tx);
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
                    let _ = tx.send(Ok(resp)).ok();
                }
                PendingRequest::InternalCommand(target_id) => {
                    if let Some(target) = self.targets.get_mut(&target_id) {
                        target.on_response(resp, method.as_ref());
                    }
                }
            }
        }
    }

    /// Submit a command initiated via channel
    pub(crate) fn submit_external_command(
        &mut self,
        msg: CommandMessage,
        now: Instant,
    ) -> Result<()> {
        let call_id = self
            .conn
            .submit_command(msg.method.clone(), msg.session_id, msg.params)?;
        self.pending_commands.insert(
            call_id,
            (PendingRequest::ExternalCommand(msg.sender), msg.method, now),
        );
        Ok(())
    }

    pub(crate) fn submit_internal_command(
        &mut self,
        target_id: TargetId,
        req: CdpRequest,
        now: Instant,
    ) -> Result<()> {
        let call_id = self.conn.submit_command(
            req.method.clone(),
            req.session_id.map(Into::into),
            req.params,
        )?;
        self.pending_commands.insert(
            call_id,
            (PendingRequest::InternalCommand(target_id), req.method, now),
        );
        Ok(())
    }

    /// Send the Request over to the server and store its identifier to handle
    /// the response once received.
    fn submit_navigation(&mut self, id: NavigationId, req: CdpRequest, now: Instant) {
        let call_id = self
            .conn
            .submit_command(
                req.method.clone(),
                req.session_id.map(Into::into),
                req.params,
            )
            .unwrap();

        self.pending_commands
            .insert(call_id, (PendingRequest::Navigate(id), req.method, now));
    }

    /// Process a message received by the target's page via channel
    fn on_target_message(&mut self, target: &mut Target, msg: CommandMessage, now: Instant) {
        // if let some
        if msg.is_navigation() {
            let (req, tx) = msg.split();
            let id = self.next_navigation_id();
            target.goto(FrameNavigationRequest::new(id, req));
            self.navigations.insert(
                id,
                NavigationRequest::Navigate(NavigationInProgress::new(tx)),
            );
        } else {
            let _ = self.submit_external_command(msg, now);
        }
    }

    /// An identifier for queued `NavigationRequest`s.
    fn next_navigation_id(&mut self) -> NavigationId {
        let id = NavigationId(self.next_navigation_id);
        self.next_navigation_id = self.next_navigation_id.wrapping_add(1);
        id
    }

    /// Create a new page and send it to the receiver when ready
    ///
    /// First a `CreateTargetParams` is send to the server, this will trigger
    /// `EventTargetCreated` which results in a new `Target` being created.
    /// Once the response to the request is received the initialization process
    /// of the target kicks in. This triggers a queue of initialization requests
    /// of the `Target`, once those are all processed and the `url` fo the
    /// `CreateTargetParams` has finished loading (The `Target`'s `Page` is
    /// ready and idle), the `Target` sends its newly created `Page` as response
    /// to the initiator (`tx`) of the `CreateTargetParams` request.
    fn create_page(&mut self, params: CreateTargetParams, tx: OneshotSender<Result<Page>>) {
        let method = params.identifier();
        match serde_json::to_value(params) {
            Ok(params) => match self.conn.submit_command(method.clone(), None, params) {
                Ok(call_id) => {
                    self.pending_commands.insert(
                        call_id,
                        (PendingRequest::CreateTarget(tx), method, Instant::now()),
                    );
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

    /// Process an incoming event read from the websocket
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
            CdpEvent::TargetDetachedFromTarget(ev) => self.on_detached_from_target(ev),
            _ => {}
        }
    }

    /// Fired when a new target was created on the chromium instance
    ///
    /// Creates a new `Target` instance and keeps track of it
    fn on_target_created(&mut self, event: EventTargetCreated) {
        let target = Target::new(event.target_info);
        self.target_ids.push(target.target_id().clone());
        self.targets.insert(target.target_id().clone(), target);
    }

    /// A new session is attached to a target
    fn on_attached_to_target(&mut self, event: EventAttachedToTarget) {
        let session = Session::new(
            event.session_id.clone(),
            event.target_info.r#type,
            event.target_info.target_id,
        );
        if let Some(target) = self.targets.get_mut(session.target_id()) {
            target.set_session_id(session.session_id().clone())
        }
        self.sessions.insert(event.session_id, session);
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

    /// Fired when the target was destroyed in the browser
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
    type Item = Result<CdpEventMessage>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let pin = self.get_mut();

        loop {
            let now = Instant::now();
            // temporary pinning of the browser receiver should be safe as we are pinning
            // through the already pinned self. with the receivers we can also
            // safely ignore exhaustion as those are fused.
            while let Poll::Ready(Some(msg)) = Pin::new(&mut pin.from_browser).poll_next(cx) {
                match msg {
                    HandlerMessage::Command(cmd) => {
                        pin.submit_external_command(cmd, now).unwrap();
                    }
                    HandlerMessage::CreatePage(params, tx) => {
                        pin.create_page(params, tx);
                    }
                    HandlerMessage::GetPages(tx) => {
                        let pages: Vec<_> = pin
                            .targets
                            .values_mut()
                            .filter_map(|target| target.get_or_create_page())
                            .map(|page| Page::from(page.clone()))
                            .collect();
                        let _ = tx.send(pages);
                    }
                    HandlerMessage::Subscribe => {
                        // TODO implement subscriptions
                    }
                }
            }

            for n in (0..pin.target_ids.len()).rev() {
                let target_id = pin.target_ids.swap_remove(n);
                if let Some((id, mut target)) = pin.targets.remove_entry(&target_id) {
                    while let Some(event) = target.poll(cx, now) {
                        match event {
                            TargetEvent::Request(req) => {
                                let _ = pin.submit_internal_command(
                                    target.target_id().clone(),
                                    req,
                                    now,
                                );
                            }
                            TargetEvent::RequestTimeout(_) => {
                                continue;
                            }
                            TargetEvent::Command(msg) => {
                                pin.on_target_message(&mut target, msg, now);
                            }
                            TargetEvent::NavigationRequest(id, req) => {
                                pin.submit_navigation(id, req, now);
                            }
                            TargetEvent::NavigationResult(res) => {
                                pin.on_navigation_lifecycle_completed(res)
                            }
                        }
                    }

                    pin.targets.insert(id, target);
                    pin.target_ids.push(target_id);
                }
            }

            let mut done = true;

            while let Poll::Ready(Some(ev)) = Pin::new(&mut pin.conn).poll_next(cx) {
                match ev {
                    Ok(Message::Response(resp)) => pin.on_response(resp),
                    Ok(Message::Event(ev)) => {
                        pin.on_event(ev);
                    }
                    Err(err) => return Poll::Ready(Some(Err(err))),
                }
                done = false;
            }

            if pin.evict_command_timeout.is_ready(cx) {
                // TODO evict all commands that timed out
            }

            if done {
                // no events/responses were read from the websocket
                return Poll::Pending;
            }
        }
    }
}

/// Wraps the sender half of the channel who requested a navigation
#[derive(Debug)]
pub struct NavigationInProgress<T> {
    /// Marker to indicate whether a navigation lifecycle has completed
    navigated: bool,
    /// The response of the issued navigation request
    response: Option<Response>,
    /// Sender who initiated the navigation request
    tx: OneshotSender<T>,
}

impl<T> NavigationInProgress<T> {
    fn new(tx: OneshotSender<T>) -> Self {
        Self {
            navigated: false,
            response: None,
            tx,
        }
    }

    /// The response to the cdp request has arrived
    fn set_response(&mut self, resp: Response) {
        self.response = Some(resp);
    }

    /// The navigation process has finished, the page finished loading.
    fn set_navigated(&mut self) {
        self.navigated = true;
    }
}

/// Request type for navigation
#[derive(Debug)]
enum NavigationRequest {
    /// Represents a simple `NavigateParams` ("Page.navigate")
    Navigate(NavigationInProgress<Result<Response>>),
    // TODO are there more?
}

/// Different kind of submitted request submitted from the  `Handler` to the
/// `Connection` and being waited on for the response.
#[derive(Debug)]
enum PendingRequest {
    /// A Request to create a new `Target` that results in the creation of a
    /// `Page` that represents a browser page.
    CreateTarget(OneshotSender<Result<Page>>),
    /// A Request to navigate a specific `Target`.
    ///
    /// Navigation requests are not automatically completed once the response to
    /// the raw cdp navigation request (like `NavigateParams`) arrives, but only
    /// after the `Target` notifies the `Handler` that the `Page` has finished
    /// loading, which comes after the response.
    Navigate(NavigationId),
    /// A common request received via a channel (`Page`).
    ExternalCommand(OneshotSender<Result<Response>>),
    /// Requests that are initiated directly from a `Target` (all the
    /// initialization commands).
    InternalCommand(TargetId),
}

/// Events used internally to communicate with the handler, which are executed
/// in the background
// TODO rename to BrowserMessage
#[derive(Debug)]
pub(crate) enum HandlerMessage {
    CreatePage(CreateTargetParams, OneshotSender<Result<Page>>),
    GetPages(OneshotSender<Vec<Page>>),
    Command(CommandMessage),
    #[allow(unused)] // allow until implemented
    Subscribe,
}
