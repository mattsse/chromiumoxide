use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::time::{Duration, Instant};

use fnv::FnvHashMap;
use futures::channel::mpsc::Receiver;
use futures::channel::oneshot::Sender as OneshotSender;
use futures::stream::{Fuse, Stream, StreamExt};
use futures::task::{Context, Poll};

use crate::listeners::{EventListenerRequest, EventListeners};
use chromiumoxide_cdp::cdp::browser_protocol::browser::*;
use chromiumoxide_cdp::cdp::browser_protocol::page::{
    AddScriptToEvaluateOnNewDocumentParams, NavigateReturns, ScriptIdentifier,
};
use chromiumoxide_cdp::cdp::browser_protocol::target::*;
use chromiumoxide_cdp::cdp::events::CdpEvent;
use chromiumoxide_cdp::cdp::events::CdpEventMessage;
use chromiumoxide_types::{CallId, Error as ProtocolError, Message, Method, Response};
use chromiumoxide_types::{MethodId, Request as CdpRequest};
#[cfg(test)]
pub(crate) use page::PageHandle;
pub(crate) use page::PageInner;

use crate::cmd::{CommandMessage, to_command_response};
use crate::conn::Connection;
use crate::error::{CdpError, Result};
use crate::handler::browser::BrowserContext;
use crate::handler::frame::{FrameNavigationRequest, FrameWaitError, HTTP_RESPONSE_CODE_FAILURE};
use crate::handler::frame::{NavigationError, NavigationId, NavigationOk, PreloadId};
use crate::handler::job::PeriodicJob;
use crate::handler::session::Session;
use crate::handler::target::{FanOutAckBatch, TargetEvent};
use crate::handler::target::{Target, TargetConfig};
use crate::handler::viewport::Viewport;
use crate::page::Page;

/// Standard timeout in MS
pub const REQUEST_TIMEOUT: u64 = 30_000;

/// Chrome uses this structured protocol error for commands addressed to a
/// child session that has already detached. Detection is shared by ACK and
/// navigation paths even though their dispositions differ.
pub(crate) fn is_session_gone_error(error: &ProtocolError) -> bool {
    error.code == -32001
}

pub mod browser;
pub mod commandfuture;
pub mod domworld;
pub mod emulation;
pub mod frame;
pub mod http;
pub mod httpfuture;
mod job;
pub mod network;
mod page;
mod session;
pub mod target;
pub mod target_message_future;
pub mod viewport;

/// The handler that monitors the state of the chromium browser and drives all
/// the requests and events.
#[must_use = "streams do nothing unless polled"]
#[derive(Debug)]
pub struct Handler {
    /// Commands that are being processed and awaiting a response from the
    /// chromium instance together with the timestamp when the request
    /// started.
    pending_commands: FnvHashMap<CallId, (PendingRequest, MethodId, Instant)>,
    /// Response-confirmed multi-session operations. A group owns the only
    /// caller sender; individual pending calls only carry its stable id.
    pending_ack_groups: FnvHashMap<AckGroupId, AckGroup>,
    /// Connection to the browser instance
    from_browser: Fuse<Receiver<HandlerMessage>>,
    default_browser_context: BrowserContext,
    browser_contexts: HashSet<BrowserContext>,
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
    /// Monotonic id source for response-confirmed fan-out groups.
    next_ack_group_id: u64,
    /// How this handler will configure targets etc,
    config: HandlerConfig,
    /// All registered event subscriptions
    event_listeners: EventListeners,
    /// Keeps track is the browser is closing
    closing: bool,
}

impl Handler {
    /// Create a new `Handler` that drives the connection and listens for
    /// messages on the receiver `rx`.
    pub(crate) fn new(
        mut conn: Connection<CdpEventMessage>,
        rx: Receiver<HandlerMessage>,
        config: HandlerConfig,
    ) -> Self {
        let discover = SetDiscoverTargetsParams::new(true);
        let _ = conn.submit_command(
            discover.identifier(),
            None,
            serde_json::to_value(discover).unwrap(),
        );

        let browser_contexts = config
            .context_ids
            .iter()
            .map(|id| BrowserContext::from(id.clone()))
            .collect();

        Self {
            pending_commands: Default::default(),
            pending_ack_groups: Default::default(),
            from_browser: rx.fuse(),
            default_browser_context: Default::default(),
            browser_contexts,
            target_ids: Default::default(),
            targets: Default::default(),
            navigations: Default::default(),
            sessions: Default::default(),
            conn,
            evict_command_timeout: PeriodicJob::new(config.request_timeout),
            next_navigation_id: 0,
            next_ack_group_id: 0,
            config,
            event_listeners: Default::default(),
            closing: false,
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

    /// The default Browser context
    pub fn default_browser_context(&self) -> &BrowserContext {
        &self.default_browser_context
    }

    /// Iterator over all currently available browser contexts
    pub fn browser_contexts(&self) -> impl Iterator<Item = &BrowserContext> + '_ {
        self.browser_contexts.iter()
    }

    fn next_ack_group_id(&mut self) -> AckGroupId {
        let id = AckGroupId(self.next_ack_group_id);
        self.next_ack_group_id = self.next_ack_group_id.wrapping_add(1);
        id
    }

    fn classify_fan_out_member_outcome(
        is_main: bool,
        error: Option<ProtocolError>,
    ) -> MemberOutcome {
        match error {
            None => MemberOutcome::Complete,
            Some(error) if !is_main && is_session_gone_error(&error) => MemberOutcome::Complete,
            Some(error) => MemberOutcome::Err(CdpError::Chrome(error)),
        }
    }

    /// Consume one removed pending member. Removing the pending call before
    /// this method makes duplicate detach/response/timeout paths idempotent.
    fn complete_ack_member(&mut self, group_id: AckGroupId, outcome: MemberOutcome) {
        match outcome {
            MemberOutcome::Err(error) => self.fail_ack_group(group_id, error),
            MemberOutcome::Complete => {
                let complete = match self.pending_ack_groups.get_mut(&group_id) {
                    Some(group) if group.remaining > 0 => {
                        group.remaining -= 1;
                        group.remaining == 0
                    }
                    Some(_) | None => false,
                };
                if complete {
                    if let Some(group) = self.pending_ack_groups.remove(&group_id) {
                        let _ = group.tx.send(Ok(()));
                    }
                }
            }
        }
    }

    /// Fail a fan-out immediately and purge every still-pending member so late
    /// responses become harmless unknown call ids.
    fn fail_ack_group(&mut self, group_id: AckGroupId, error: CdpError) {
        let Some(group) = self.pending_ack_groups.remove(&group_id) else {
            return;
        };
        self.pending_commands.retain(|_, (pending, _, _)| {
            !matches!(
                pending,
                PendingRequest::FanOutAckMember {
                    group_id: pending_group,
                    ..
                } if *pending_group == group_id
            )
        });
        let _ = group.tx.send(Err(error));
    }

    fn fail_ack_groups_for_target(&mut self, target_id: &TargetId) {
        let group_ids = self
            .pending_ack_groups
            .iter()
            .filter(|(_, group)| &group.target_id == target_id)
            .map(|(group_id, _)| *group_id)
            .collect::<Vec<_>>();
        for group_id in group_ids {
            self.fail_ack_group(group_id, CdpError::NoResponse);
        }
    }

    fn fail_all_ack_groups(&mut self) {
        let group_ids = self.pending_ack_groups.keys().copied().collect::<Vec<_>>();
        for group_id in group_ids {
            self.fail_ack_group(group_id, CdpError::NoResponse);
        }
    }

    fn fail_navigation_response(
        &mut self,
        id: NavigationId,
        error: CdpError,
        watcher_error: String,
    ) {
        if let Some(NavigationRequest::Navigate(navigation)) = self.navigations.remove(&id) {
            if let Some(target) = self.targets.get_mut(&navigation.target_id) {
                target.on_navigation_failed(id, watcher_error);
            }
            let _ = navigation.tx.send(Err(error));
        }
    }

    /// received a response to a navigation request like `Page.navigate`
    fn on_navigation_response(&mut self, id: NavigationId, mut resp: Response) {
        if let Some(error) = resp.error.take() {
            if is_session_gone_error(&error) {
                tracing::debug!(navigation_id = ?id, "navigation session disappeared before response");
            }
            let watcher_error = error.message.clone();
            self.fail_navigation_response(id, CdpError::Chrome(error), watcher_error);
            return;
        }

        let navigate_result = match resp.result.as_ref() {
            Some(result) => {
                serde_json::from_value::<NavigateReturns>(result.clone()).map_err(CdpError::from)
            }
            None => Err(CdpError::NoResponse),
        };
        let navigate_result = match navigate_result {
            Ok(result) => result,
            Err(error) => {
                let watcher_error = error.to_string();
                self.fail_navigation_response(id, error, watcher_error);
                return;
            }
        };
        if let Some(error_text) = navigate_result
            .error_text
            .filter(|error_text| !error_text.is_empty())
        {
            if error_text != HTTP_RESPONSE_CODE_FAILURE {
                self.fail_navigation_response(
                    id,
                    CdpError::ChromeMessage(error_text.clone()),
                    error_text,
                );
                return;
            }
        }

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
                self.remove_pending_navigation_call(*err.navigation_id());
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

    fn remove_pending_navigation_call(&mut self, navigation_id: NavigationId) {
        let call_ids = self
            .pending_commands
            .iter()
            .filter_map(|(call_id, (pending, _, _))| {
                matches!(pending, PendingRequest::Navigate(id) if *id == navigation_id)
                    .then_some(*call_id)
            })
            .collect::<Vec<_>>();
        for call_id in call_ids {
            self.pending_commands.remove(&call_id);
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
                PendingRequest::GetTargets(tx) => {
                    match to_command_response::<GetTargetsParams>(resp, method) {
                        Ok(resp) => {
                            let targets: Vec<TargetInfo> = resp.result.target_infos;
                            let results = targets.clone();
                            for target_info in targets {
                                let target_id = target_info.target_id.clone();
                                let event: EventTargetCreated = EventTargetCreated { target_info };
                                self.on_target_created(event);
                                let attach = AttachToTargetParams::new(target_id);
                                let _ = self.conn.submit_command(
                                    attach.identifier(),
                                    None,
                                    serde_json::to_value(attach).unwrap(),
                                );
                            }

                            let _ = tx.send(Ok(results)).ok();
                        }
                        Err(err) => {
                            let _ = tx.send(Err(err)).ok();
                        }
                    }
                }
                PendingRequest::Navigate(id) => {
                    let detached = self
                        .navigations
                        .get(&id)
                        .map(|navigation| match navigation {
                            NavigationRequest::Navigate(navigation) => {
                                navigation.session_id.clone()
                            }
                        })
                        .is_some_and(|session_id| self.session_is_unavailable(&session_id));
                    if detached {
                        if let Some(NavigationRequest::Navigate(navigation)) =
                            self.navigations.remove(&id)
                        {
                            let _ = navigation.tx.send(Err(CdpError::FrameNotReady));
                        }
                    } else {
                        self.on_navigation_response(id, resp);
                    }
                }
                PendingRequest::ExternalCommand { session_id, tx } => {
                    if session_id
                        .as_ref()
                        .is_some_and(|session_id| self.session_is_unavailable(session_id))
                    {
                        let _ = tx.send(Err(CdpError::FrameNotReady));
                    } else {
                        let _ = tx.send(Ok(resp));
                    }
                }
                PendingRequest::InternalCommand(target_id, session_id) => {
                    let detached = session_id
                        .as_ref()
                        .is_some_and(|session_id| self.session_is_unavailable(session_id));
                    if !detached {
                        if let Some(target) = self.targets.get_mut(&target_id) {
                            target.on_response_in_session(resp, method.as_ref(), session_id);
                        }
                    }
                }
                PendingRequest::PreloadAddScript {
                    target_id,
                    session_id,
                    preload_key,
                } => {
                    match to_command_response::<AddScriptToEvaluateOnNewDocumentParams>(
                        resp, method,
                    ) {
                        Ok(response) => {
                            if !self.session_is_unavailable(&session_id) {
                                if let Some(target) = self.targets.get_mut(&target_id) {
                                    target.frame_manager_mut().set_preload_id(
                                        preload_key,
                                        session_id,
                                        response.result.identifier,
                                    );
                                }
                            }
                        }
                        Err(error) => {
                            tracing::debug!(
                                target_id = %target_id.as_ref(),
                                session_id = %session_id.as_ref(),
                                %error,
                                "child preload script registration failed"
                            );
                        }
                    }
                }
                PendingRequest::AddPreloadScriptMain {
                    target_id,
                    params,
                    tx,
                } => {
                    match to_command_response::<AddScriptToEvaluateOnNewDocumentParams>(
                        resp, method,
                    ) {
                        Ok(response) => {
                            let main_id = response.result.identifier;
                            if let Some(target) = self.targets.get_mut(&target_id) {
                                let preload_key = target
                                    .frame_manager_mut()
                                    .add_preload_script(params, main_id.clone());
                                target.enqueue_preload_fan_out(preload_key);
                                let _ = tx.send(Ok(main_id));
                            } else {
                                let _ = tx.send(Err(CdpError::NoResponse));
                            }
                        }
                        Err(error) => {
                            let _ = tx.send(Err(error));
                        }
                    }
                }
                PendingRequest::FanOutAckMember {
                    group_id,
                    session_id,
                } => {
                    let is_main = self
                        .pending_ack_groups
                        .get(&group_id)
                        .is_some_and(|group| group.main_session_id == session_id);
                    let outcome = Self::classify_fan_out_member_outcome(is_main, resp.error);
                    self.complete_ack_member(group_id, outcome);
                }
                PendingRequest::CloseBrowser(tx) => {
                    self.closing = true;
                    let _ = tx.send(Ok(CloseReturns {})).ok();
                }
            }
        }
    }

    fn session_is_unavailable(&self, session_id: &SessionId) -> bool {
        let Some(session) = self.sessions.get(session_id) else {
            return true;
        };
        self.targets
            .get(session.target_id())
            .is_none_or(|target| target.is_session_draining(session_id))
    }

    /// Submit a command initiated via channel
    pub(crate) fn submit_external_command(
        &mut self,
        msg: CommandMessage,
        now: Instant,
    ) -> Result<()> {
        let session_id = msg.session_id.clone();
        let call_id = self
            .conn
            .submit_command(msg.method.clone(), msg.session_id, msg.params)?;
        self.pending_commands.insert(
            call_id,
            (
                PendingRequest::ExternalCommand {
                    session_id,
                    tx: msg.sender,
                },
                msg.method,
                now,
            ),
        );
        Ok(())
    }

    pub(crate) fn submit_internal_command(
        &mut self,
        target_id: TargetId,
        req: CdpRequest,
        now: Instant,
    ) -> Result<()> {
        let session_id = req
            .session_id
            .as_ref()
            .map(|session_id| SessionId::new(session_id.clone()));
        let call_id = self.conn.submit_command(
            req.method.clone(),
            req.session_id.map(Into::into),
            req.params,
        )?;
        self.pending_commands.insert(
            call_id,
            (
                PendingRequest::InternalCommand(target_id, session_id),
                req.method,
                now,
            ),
        );
        Ok(())
    }

    /// Submit an already-serialized request and attach its lifecycle owner.
    /// `Connection::submit_command` only allocates an id and queues a value, so
    /// after serialization this operation has no recoverable failure branch.
    fn submit_command_with_pending(
        &mut self,
        req: CdpRequest,
        pending: PendingRequest,
        now: Instant,
    ) -> CallId {
        let CdpRequest {
            method,
            session_id,
            params,
        } = req;
        let call_id = self
            .conn
            .submit_command(method.clone(), session_id.map(SessionId::new), params)
            .expect("queuing an already-serialized CDP request is infallible");
        self.pending_commands
            .insert(call_id, (pending, method, now));
        call_id
    }

    /// Submit both disjoint halves of one queued fan-out item exactly once.
    /// The target queue position establishes ordering; this method only turns
    /// the already-ordered batch into call ids and an acknowledgement group.
    fn submit_fan_out_ack_batch(
        &mut self,
        target_id: &TargetId,
        batch: FanOutAckBatch,
        now: Instant,
    ) {
        let FanOutAckBatch {
            ack_reqs,
            send_only_reqs,
            ack_tx,
            main_session_id,
        } = batch;

        for request in send_only_reqs {
            let session_id = request
                .session_id
                .as_ref()
                .map(|session_id| SessionId::new(session_id.clone()));
            self.submit_command_with_pending(
                request,
                PendingRequest::InternalCommand(target_id.clone(), session_id),
                now,
            );
        }

        if ack_reqs.is_empty() {
            let _ = ack_tx.send(Ok(()));
            return;
        }

        let group_id = self.next_ack_group_id();
        self.pending_ack_groups.insert(
            group_id,
            AckGroup {
                target_id: target_id.clone(),
                main_session_id,
                remaining: ack_reqs.len(),
                tx: ack_tx,
            },
        );
        for request in ack_reqs {
            let session_id = SessionId::new(
                request
                    .session_id
                    .as_ref()
                    .expect("fan-out request carries a session")
                    .clone(),
            );
            self.submit_command_with_pending(
                request,
                PendingRequest::FanOutAckMember {
                    group_id,
                    session_id,
                },
                now,
            );
        }
    }

    fn submit_fetch_targets(&mut self, tx: OneshotSender<Result<Vec<TargetInfo>>>, now: Instant) {
        let msg = GetTargetsParams { filter: None };
        let method = msg.identifier();
        let call_id = self
            .conn
            .submit_command(method.clone(), None, serde_json::to_value(msg).unwrap())
            .unwrap();

        self.pending_commands
            .insert(call_id, (PendingRequest::GetTargets(tx), method, now));
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

    fn submit_navigation_for_target(
        &mut self,
        target: &Target,
        id: NavigationId,
        req: CdpRequest,
        now: Instant,
    ) {
        let draining_session = req
            .session_id
            .as_ref()
            .map(|session_id| SessionId::new(session_id.clone()))
            .filter(|session_id| target.is_session_draining(session_id));
        if draining_session.is_some() {
            // The target is temporarily outside the registry while it is
            // polled, so this gate must use the local target. A missing sender
            // means earlier draining cleanup already settled the navigation.
            if let Some(NavigationRequest::Navigate(navigation)) = self.navigations.remove(&id) {
                let _ = navigation.tx.send(Err(CdpError::FrameNotReady));
            }
            return;
        }

        self.submit_navigation(id, req, now);
    }

    fn submit_close(&mut self, tx: OneshotSender<Result<CloseReturns>>, now: Instant) {
        let close_msg = CloseParams::default();
        let method = close_msg.identifier();

        let call_id = self
            .conn
            .submit_command(
                method.clone(),
                None,
                serde_json::to_value(close_msg).unwrap(),
            )
            .unwrap();

        self.pending_commands
            .insert(call_id, (PendingRequest::CloseBrowser(tx), method, now));
    }

    /// Process a message received by the target's page via channel
    fn on_target_message(&mut self, target: &mut Target, msg: CommandMessage, now: Instant) {
        if let Some(session_id) = msg.session_id.as_ref() {
            if !target.frame_session_ready(session_id) {
                let _ = msg.sender.send(Err(CdpError::FrameNotReady));
                return;
            }
        }
        if msg.is_navigation() {
            let Some(session_id) = msg
                .session_id
                .clone()
                .or_else(|| target.session_id().cloned())
            else {
                let _ = msg.sender.send(Err(CdpError::FrameNotReady));
                return;
            };
            let (req, tx) = msg.split();
            let id = self.next_navigation_id();
            target.goto_in_session(session_id.clone(), FrameNavigationRequest::new(id, req));
            self.navigations.insert(
                id,
                NavigationRequest::Navigate(NavigationInProgress::new(
                    target.target_id().clone(),
                    session_id,
                    tx,
                )),
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

    /// Settle every submitted operation owned by one child session. Main
    /// session teardown is target-wide and deliberately takes a different path.
    fn fail_pending_for_session(&mut self, target: &mut Target, session_id: &SessionId) {
        if target.session_id() == Some(session_id) {
            return;
        }

        let call_ids = self
            .pending_commands
            .iter()
            .filter_map(|(call_id, (pending, _, _))| {
                let matches = match pending {
                    PendingRequest::ExternalCommand {
                        session_id: Some(pending_session),
                        ..
                    } => pending_session == session_id,
                    PendingRequest::ExternalCommand {
                        session_id: None, ..
                    } => false,
                    PendingRequest::InternalCommand(_, Some(pending_session)) => {
                        pending_session == session_id
                    }
                    PendingRequest::InternalCommand(_, None) => false,
                    PendingRequest::FanOutAckMember {
                        session_id: pending_session,
                        ..
                    } => pending_session == session_id,
                    PendingRequest::PreloadAddScript {
                        session_id: pending_session,
                        ..
                    } => pending_session == session_id,
                    PendingRequest::AddPreloadScriptMain { .. } => false,
                    PendingRequest::Navigate(navigation_id) => self
                        .navigations
                        .get(navigation_id)
                        .is_some_and(|navigation| match navigation {
                            NavigationRequest::Navigate(navigation) => {
                                &navigation.session_id == session_id
                            }
                        }),
                    PendingRequest::CreateTarget(_)
                    | PendingRequest::GetTargets(_)
                    | PendingRequest::CloseBrowser(_) => false,
                };
                matches.then_some(*call_id)
            })
            .collect::<Vec<_>>();

        for call_id in call_ids {
            let Some((pending, _, _)) = self.pending_commands.remove(&call_id) else {
                continue;
            };
            match pending {
                PendingRequest::ExternalCommand { tx, .. } => {
                    let _ = tx.send(Err(CdpError::FrameNotReady));
                }
                PendingRequest::InternalCommand(_, _) => {}
                PendingRequest::PreloadAddScript { .. } => {}
                PendingRequest::AddPreloadScriptMain { .. } => {}
                PendingRequest::FanOutAckMember { group_id, .. } => {
                    self.complete_ack_member(group_id, MemberOutcome::Complete);
                }
                PendingRequest::Navigate(navigation_id) => {
                    if let Some(NavigationRequest::Navigate(navigation)) =
                        self.navigations.remove(&navigation_id)
                    {
                        let _ = navigation.tx.send(Err(CdpError::FrameNotReady));
                    }
                }
                PendingRequest::CreateTarget(_)
                | PendingRequest::GetTargets(_)
                | PendingRequest::CloseBrowser(_) => {}
            }
        }

        let navigation_ids = self
            .navigations
            .iter()
            .filter_map(|(navigation_id, navigation)| match navigation {
                NavigationRequest::Navigate(navigation) if &navigation.session_id == session_id => {
                    Some(*navigation_id)
                }
                NavigationRequest::Navigate(_) => None,
            })
            .collect::<Vec<_>>();
        for navigation_id in navigation_ids {
            if let Some(NavigationRequest::Navigate(navigation)) =
                self.navigations.remove(&navigation_id)
            {
                let _ = navigation.tx.send(Err(CdpError::FrameNotReady));
            }
        }

        target.fail_queued_events(session_id);
    }

    /// Settle all state owned by a target, including child sessions and
    /// navigation entries whose command response already arrived.
    fn fail_pending_for_target(&mut self, target_id: &TargetId, main_session: Option<&SessionId>) {
        let mut owned_sessions = self
            .sessions
            .values()
            .filter(|session| session.target_id() == target_id)
            .map(|session| session.session_id().clone())
            .collect::<HashSet<_>>();
        if let Some(main_session) = main_session {
            owned_sessions.insert(main_session.clone());
        }

        let navigation_ids = self
            .navigations
            .iter()
            .filter_map(|(navigation_id, navigation)| match navigation {
                NavigationRequest::Navigate(navigation) if &navigation.target_id == target_id => {
                    Some(*navigation_id)
                }
                NavigationRequest::Navigate(_) => None,
            })
            .collect::<HashSet<_>>();

        let call_ids = self
            .pending_commands
            .iter()
            .filter_map(|(call_id, (pending, _, _))| {
                let matches = match pending {
                    PendingRequest::InternalCommand(pending_target, _) => {
                        pending_target == target_id
                    }
                    PendingRequest::ExternalCommand {
                        session_id: Some(session_id),
                        ..
                    } => owned_sessions.contains(session_id),
                    PendingRequest::ExternalCommand {
                        session_id: None, ..
                    } => false,
                    PendingRequest::FanOutAckMember { group_id, .. } => self
                        .pending_ack_groups
                        .get(group_id)
                        .is_some_and(|group| &group.target_id == target_id),
                    PendingRequest::PreloadAddScript {
                        target_id: pending_target,
                        ..
                    }
                    | PendingRequest::AddPreloadScriptMain {
                        target_id: pending_target,
                        ..
                    } => pending_target == target_id,
                    PendingRequest::Navigate(navigation_id) => {
                        navigation_ids.contains(navigation_id)
                    }
                    PendingRequest::CreateTarget(_)
                    | PendingRequest::GetTargets(_)
                    | PendingRequest::CloseBrowser(_) => false,
                };
                matches.then_some(*call_id)
            })
            .collect::<Vec<_>>();

        for navigation_id in &navigation_ids {
            if let Some(NavigationRequest::Navigate(navigation)) =
                self.navigations.remove(navigation_id)
            {
                let _ = navigation.tx.send(Err(CdpError::NoResponse));
            }
        }

        for call_id in call_ids {
            let Some((pending, _, _)) = self.pending_commands.remove(&call_id) else {
                continue;
            };
            match pending {
                PendingRequest::ExternalCommand { tx, .. } => {
                    let _ = tx.send(Err(CdpError::NoResponse));
                }
                PendingRequest::Navigate(_) | PendingRequest::InternalCommand(_, _) => {}
                PendingRequest::PreloadAddScript { .. } => {}
                PendingRequest::AddPreloadScriptMain { tx, .. } => {
                    let _ = tx.send(Err(CdpError::NoResponse));
                }
                PendingRequest::FanOutAckMember { group_id, .. } => {
                    self.fail_ack_group(group_id, CdpError::NoResponse);
                }
                PendingRequest::CreateTarget(_)
                | PendingRequest::GetTargets(_)
                | PendingRequest::CloseBrowser(_) => {}
            }
        }

        self.sessions
            .retain(|_, session| session.target_id() != target_id);
    }

    /// Explicitly settle owned senders before the handler stream terminates.
    /// This avoids exposing channel cancellation when the transport already
    /// knows no response can arrive.
    fn fail_all_pending_on_connection_close(&mut self) {
        for target in self.targets.values_mut() {
            target.fail_all_queued_events();
            target.settle_whole_target_teardown();
            // On connection close the Handler stops polling, so FrameManager
            // waiters would otherwise hang until their own deadline (which is
            // never advanced again). Settle them with a typed error now. The
            // Handler-owned goto senders are settled by the pending drain below.
            target
                .frame_manager_mut()
                .fail_all_navigation_state(FrameWaitError::FrameSwappedOrDetached);
        }
        self.fail_all_ack_groups();

        for (_, (pending, _, _)) in std::mem::take(&mut self.pending_commands) {
            match pending {
                PendingRequest::CreateTarget(tx) => {
                    let _ = tx.send(Err(CdpError::NoResponse));
                }
                PendingRequest::GetTargets(tx) => {
                    let _ = tx.send(Err(CdpError::NoResponse));
                }
                PendingRequest::Navigate(navigation_id) => {
                    if let Some(NavigationRequest::Navigate(navigation)) =
                        self.navigations.remove(&navigation_id)
                    {
                        let _ = navigation.tx.send(Err(CdpError::NoResponse));
                    }
                }
                PendingRequest::ExternalCommand { tx, .. } => {
                    let _ = tx.send(Err(CdpError::NoResponse));
                }
                PendingRequest::InternalCommand(_, _)
                | PendingRequest::FanOutAckMember { .. }
                | PendingRequest::PreloadAddScript { .. } => {}
                PendingRequest::AddPreloadScriptMain { tx, .. } => {
                    let _ = tx.send(Err(CdpError::NoResponse));
                }
                PendingRequest::CloseBrowser(tx) => {
                    let _ = tx.send(Err(CdpError::NoResponse));
                }
            }
        }

        for (_, navigation) in std::mem::take(&mut self.navigations) {
            match navigation {
                NavigationRequest::Navigate(navigation) => {
                    let _ = navigation.tx.send(Err(CdpError::NoResponse));
                }
            }
        }
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
        match url::Url::parse(&params.url) {
            Ok(_) => {
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
            Err(err) => {
                let _ = tx.send(Err(err.into())).ok();
            }
        }
    }

    /// Process an incoming event read from the websocket
    fn on_event(&mut self, event: CdpEventMessage) {
        if let Some(ref session_id) = event.session_id {
            if let Some(session) = self.sessions.get(session_id.as_str()) {
                if let Some(target) = self.targets.get_mut(session.target_id()) {
                    return target.on_event(event);
                }
            }
        }
        let CdpEventMessage { params, method, .. } = event;
        match params.clone() {
            CdpEvent::TargetTargetCreated(ev) => self.on_target_created(*ev),
            CdpEvent::TargetAttachedToTarget(ev) => self.on_attached_to_target(ev),
            CdpEvent::TargetTargetDestroyed(ev) => self.on_target_destroyed(ev),
            CdpEvent::TargetDetachedFromTarget(ev) => self.on_detached_from_target(ev),
            _ => {}
        }
        chromiumoxide_cdp::consume_event!(match params {
            |ev| self.event_listeners.start_send(ev),
            |json| { let _ = self.event_listeners.try_send_custom(&method, json);}
        });
    }

    /// Fired when a new target was created on the chromium instance
    ///
    /// Creates a new `Target` instance and keeps track of it
    fn on_target_created(&mut self, event: EventTargetCreated) {
        let browser_ctx = event
            .target_info
            .browser_context_id
            .clone()
            .map(BrowserContext::from)
            .filter(|id| self.browser_contexts.contains(id))
            .unwrap_or_else(|| self.default_browser_context.clone());
        let target = Target::new(
            event.target_info,
            TargetConfig {
                ignore_https_errors: self.config.ignore_https_errors,
                request_timeout: self.config.request_timeout,
                viewport: self.config.viewport.clone(),
                request_intercept: self.config.request_intercept,
                cache_enabled: self.config.cache_enabled,
            },
            browser_ctx,
        );
        self.target_ids.push(target.target_id().clone());
        self.targets.insert(target.target_id().clone(), target);
    }

    /// A new session is attached to a target
    fn on_attached_to_target(&mut self, event: Box<EventAttachedToTarget>) {
        let session = Session::new(event.session_id.clone(), event.target_info.target_id);
        if let Some(target) = self.targets.get_mut(session.target_id()) {
            target.set_session_id(session.session_id().clone())
        }
        self.sessions.insert(event.session_id, session);
    }

    /// The session was detached from target.
    /// Can be issued multiple times per target if multiple session have been
    /// attached to it.
    fn on_detached_from_target(&mut self, event: EventDetachedFromTarget) {
        let Some(session) = self.sessions.get(&event.session_id).cloned() else {
            return;
        };
        let target_id = session.target_id().clone();
        let is_main = self
            .targets
            .get(&target_id)
            .is_some_and(|target| target.session_id() == Some(&event.session_id));

        if is_main {
            if let Some(mut target) = self.targets.remove(&target_id) {
                let main_session = target.session_id().cloned();
                target.fail_all_queued_events();
                target.settle_whole_target_teardown();
                // Main-session detach drops the whole target; settle FrameManager
                // waiters before it (and its FrameManager) is gone. Handler-owned
                // goto senders are settled by fail_pending_for_target below.
                target
                    .frame_manager_mut()
                    .fail_all_navigation_state(FrameWaitError::FrameSwappedOrDetached);
                self.fail_ack_groups_for_target(&target_id);
                self.fail_pending_for_target(&target_id, main_session.as_ref());
                target.session_id_mut().take();
            }
            self.target_ids.retain(|candidate| candidate != &target_id);
        } else {
            tracing::warn!(
                target_id = ?target_id,
                session_id = ?event.session_id,
                "child-session detach reached the handler path; the auto-attach envelope invariant did not hold and cleanup here is incomplete"
            );
            self.sessions.remove(&event.session_id);
            if let Some(mut target) = self.targets.remove(&target_id) {
                self.fail_pending_for_session(&mut target, &event.session_id);
                target.clear_draining(&event.session_id);
                self.targets.insert(target_id, target);
            }
        }
    }

    /// Fired when the target was destroyed in the browser
    fn on_target_destroyed(&mut self, event: EventTargetDestroyed) {
        if let Some(mut target) = self.targets.remove(&event.target_id) {
            let main_session = target.session_id().cloned();
            target.fail_all_queued_events();
            target.settle_whole_target_teardown();
            // Settle FrameManager-registered wait_for_navigation waiters before
            // the target (and its FrameManager) is dropped, so callers observe a
            // typed error rather than a channel cancellation. The Handler-owned
            // goto senders are settled by fail_pending_for_target below.
            target
                .frame_manager_mut()
                .fail_all_navigation_state(FrameWaitError::FrameSwappedOrDetached);
            self.fail_ack_groups_for_target(&event.target_id);
            self.fail_pending_for_target(&event.target_id, main_session.as_ref());
            target.session_id_mut().take();
        }
        self.target_ids
            .retain(|target_id| target_id != &event.target_id);
    }

    /// House keeping of commands
    ///
    /// Remove all commands where `now` > `timestamp of command starting point +
    /// request timeout` and notify the senders that their request timed out.
    fn evict_timed_out_commands(&mut self, now: Instant) {
        let timed_out = self
            .pending_commands
            .iter()
            .filter(|(_, (_, _, timestamp))| now > (*timestamp + self.config.request_timeout))
            .map(|(k, _)| *k)
            .collect::<Vec<_>>();
        for call in timed_out {
            if let Some((req, _, _)) = self.pending_commands.remove(&call) {
                match req {
                    PendingRequest::CreateTarget(tx) => {
                        let _ = tx.send(Err(CdpError::Timeout));
                    }
                    PendingRequest::GetTargets(tx) => {
                        let _ = tx.send(Err(CdpError::Timeout));
                    }
                    PendingRequest::Navigate(nav) => {
                        if let Some(nav) = self.navigations.remove(&nav) {
                            match nav {
                                NavigationRequest::Navigate(nav) => {
                                    let _ = nav.tx.send(Err(CdpError::Timeout));
                                }
                            }
                        }
                    }
                    PendingRequest::ExternalCommand { tx, .. } => {
                        let _ = tx.send(Err(CdpError::Timeout));
                    }
                    PendingRequest::InternalCommand(_, _) => {}
                    PendingRequest::PreloadAddScript { .. } => {}
                    PendingRequest::AddPreloadScriptMain { tx, .. } => {
                        let _ = tx.send(Err(CdpError::Timeout));
                    }
                    PendingRequest::FanOutAckMember { group_id, .. } => {
                        self.fail_ack_group(group_id, CdpError::Timeout);
                    }
                    PendingRequest::CloseBrowser(tx) => {
                        let _ = tx.send(Err(CdpError::Timeout));
                    }
                }
            }
        }
    }

    pub fn event_listeners_mut(&mut self) -> &mut EventListeners {
        &mut self.event_listeners
    }
}

impl Stream for Handler {
    type Item = Result<()>;

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
                        pin.submit_external_command(cmd, now)?;
                    }
                    HandlerMessage::FetchTargets(tx) => {
                        pin.submit_fetch_targets(tx, now);
                    }
                    HandlerMessage::CloseBrowser(tx) => {
                        pin.submit_close(tx, now);
                    }
                    HandlerMessage::CreatePage(params, tx) => {
                        pin.create_page(params, tx);
                    }
                    HandlerMessage::GetPages(tx) => {
                        let mut pages = Vec::new();
                        let mut exposed_target_ids = Vec::new();
                        for (target_id, target) in &mut pin.targets {
                            if !target.is_page() {
                                continue;
                            }
                            if let Some(page) = target.get_or_create_page() {
                                pages.push(Page::from(page.clone()));
                                exposed_target_ids.push(target_id.clone());
                            }
                        }
                        if tx.send(pages).is_ok() {
                            for target_id in exposed_target_ids {
                                if let Some(target) = pin.targets.get_mut(&target_id) {
                                    target.mark_page_exposed_after_successful_send(true);
                                }
                            }
                        }
                    }
                    HandlerMessage::InsertContext(ctx) => {
                        pin.browser_contexts.insert(ctx);
                    }
                    HandlerMessage::DisposeContext(ctx) => {
                        pin.browser_contexts.remove(&ctx);
                    }
                    HandlerMessage::GetPage(target_id, tx) => {
                        if let Some(target) = pin.targets.get_mut(&target_id) {
                            let page = target
                                .get_or_create_page()
                                .map(|page| Page::from(page.clone()));
                            let has_page = page.is_some();
                            let delivered = tx.send(page).is_ok();
                            target.mark_page_exposed_after_successful_send(has_page && delivered);
                        } else {
                            let _ = tx.send(None);
                        }
                    }
                    HandlerMessage::AddEventListener(req) => {
                        pin.event_listeners.add_listener(req);
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
                            TargetEvent::Command(msg) => {
                                pin.on_target_message(&mut target, msg, now);
                            }
                            TargetEvent::NavigationRequest(id, req) => {
                                pin.submit_navigation_for_target(&target, id, req, now);
                            }
                            TargetEvent::NavigationResult(res) => {
                                pin.on_navigation_lifecycle_completed(res)
                            }
                            TargetEvent::RegisterChildSession(session_id) => {
                                let session =
                                    Session::new(session_id.clone(), target.target_id().clone());
                                pin.sessions.insert(session_id, session);
                            }
                            TargetEvent::UnregisterChildSession(session_id) => {
                                pin.sessions.remove(&session_id);
                                pin.fail_pending_for_session(&mut target, &session_id);
                                target.fail_queued_events(&session_id);
                                target.clear_draining(&session_id);
                            }
                            TargetEvent::SessionDraining(session_id) => {
                                pin.fail_pending_for_session(&mut target, &session_id);
                                target.fail_queued_events(&session_id);
                            }
                            TargetEvent::FrameNavigate {
                                session_id,
                                frame_id,
                                req,
                                tx,
                            } => {
                                if !target.frame_ready(&frame_id, &session_id) {
                                    let _ = tx.send(Err(CdpError::FrameNotReady));
                                } else {
                                    let navigation_id = pin.next_navigation_id();
                                    target.goto_frame(
                                        session_id.clone(),
                                        frame_id,
                                        FrameNavigationRequest::new(navigation_id, req),
                                    );
                                    pin.navigations.insert(
                                        navigation_id,
                                        NavigationRequest::Navigate(NavigationInProgress::new(
                                            target.target_id().clone(),
                                            session_id,
                                            tx,
                                        )),
                                    );
                                }
                            }
                            TargetEvent::FrameWaitForNavigation {
                                session_id,
                                frame_id,
                                tx,
                            } => {
                                if !target.frame_ready(&frame_id, &session_id) {
                                    let _ = tx.send(Err(
                                        crate::handler::frame::FrameWaitError::FrameSwappedOrDetached,
                                    ));
                                } else {
                                    target
                                        .frame_manager_mut()
                                        .wait_for_navigation(session_id, frame_id, tx);
                                }
                            }
                            TargetEvent::QueuePreloadScript {
                                request,
                                preload_key,
                            } => {
                                let session_id = SessionId::new(
                                    request
                                        .session_id
                                        .as_ref()
                                        .expect("preload request carries a child session")
                                        .clone(),
                                );
                                pin.submit_command_with_pending(
                                    request,
                                    PendingRequest::PreloadAddScript {
                                        target_id: target.target_id().clone(),
                                        session_id,
                                        preload_key,
                                    },
                                    now,
                                );
                            }
                            TargetEvent::AddPreloadScript { params, tx } => {
                                if let Some(main_session_id) = target.session_id().cloned() {
                                    let request = CdpRequest {
                                        method: params.identifier(),
                                        session_id: Some(main_session_id.into()),
                                        params: serde_json::to_value(params.clone())
                                            .expect("preload command should serialize"),
                                    };
                                    pin.submit_command_with_pending(
                                        request,
                                        PendingRequest::AddPreloadScriptMain {
                                            target_id: target.target_id().clone(),
                                            params,
                                            tx,
                                        },
                                        now,
                                    );
                                } else {
                                    let _ = tx.send(Err(CdpError::FrameNotReady));
                                }
                            }
                            TargetEvent::FanOutAckBatch(batch) => {
                                pin.submit_fan_out_ack_batch(target.target_id(), batch, now);
                            }
                        }
                    }

                    // poll the target's event listeners
                    target.event_listeners_mut().poll(cx);
                    // poll the handler's event listeners
                    pin.event_listeners_mut().poll(cx);

                    pin.targets.insert(id, target);
                    pin.target_ids.push(target_id);
                }
            }

            let mut done = true;

            loop {
                match Pin::new(&mut pin.conn).poll_next(cx) {
                    Poll::Pending => break,
                    Poll::Ready(None) => {
                        pin.fail_all_pending_on_connection_close();
                        return Poll::Ready(None);
                    }
                    Poll::Ready(Some(ev)) => {
                        done = false;
                        match ev {
                            Ok(Message::Response(resp)) => {
                                pin.on_response(resp);
                                if pin.closing {
                                    pin.fail_all_pending_on_connection_close();
                                    return Poll::Ready(None);
                                }
                            }
                            Ok(Message::Event(ev)) => {
                                pin.on_event(ev);
                            }
                            Err(err @ CdpError::InvalidMessage(_, _)) => {
                                if pin.config.ignore_invalid_messages {
                                    tracing::warn!("WS Invalid message: {}", err);
                                } else {
                                    return Poll::Ready(Some(Err(err)));
                                }
                            }
                            Err(err) => {
                                tracing::error!("WS Connection error: {:?}", err);
                                pin.fail_all_pending_on_connection_close();
                                return Poll::Ready(Some(Err(err)));
                            }
                        }
                    }
                }
            }

            if pin.evict_command_timeout.poll_ready(cx) {
                // evict all commands that timed out
                pin.evict_timed_out_commands(now);
            }

            if done {
                // no events/responses were read from the websocket
                return Poll::Pending;
            }
        }
    }
}

/// How to configure the handler
#[derive(Debug, Clone)]
pub struct HandlerConfig {
    /// Whether the `NetworkManager`s should ignore https errors
    pub ignore_https_errors: bool,
    /// Whether to ignore invalid messages
    pub ignore_invalid_messages: bool,
    /// Window and device settings
    pub viewport: Option<Viewport>,
    /// Context ids to set from the get go
    pub context_ids: Vec<BrowserContextId>,
    /// default request timeout to use
    pub request_timeout: Duration,
    /// Whether to enable request interception
    pub request_intercept: bool,
    /// Whether to enable cache
    pub cache_enabled: bool,
}

impl Default for HandlerConfig {
    fn default() -> Self {
        Self {
            ignore_https_errors: true,
            ignore_invalid_messages: true,
            viewport: Default::default(),
            context_ids: Vec::new(),
            request_timeout: Duration::from_millis(REQUEST_TIMEOUT),
            request_intercept: false,
            cache_enabled: true,
        }
    }
}

/// Wraps the sender half of the channel who requested a navigation
#[derive(Debug)]
pub struct NavigationInProgress<T> {
    /// Target that owns this navigation. Needed to settle lifecycle-only state
    /// when the whole target disappears.
    target_id: TargetId,
    /// Session selected once at registration and used for detach cleanup.
    session_id: SessionId,
    /// Marker to indicate whether a navigation lifecycle has completed
    navigated: bool,
    /// The response of the issued navigation request
    response: Option<Response>,
    /// Sender who initiated the navigation request
    tx: OneshotSender<T>,
}

impl<T> NavigationInProgress<T> {
    fn new(target_id: TargetId, session_id: SessionId, tx: OneshotSender<T>) -> Self {
        Self {
            target_id,
            session_id,
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
    /// A Request to fetch old `Target`s created before connection
    GetTargets(OneshotSender<Result<Vec<TargetInfo>>>),
    /// A Request to navigate a specific `Target`.
    ///
    /// Navigation requests are not automatically completed once the response to
    /// the raw cdp navigation request (like `NavigateParams`) arrives, but only
    /// after the `Target` notifies the `Handler` that the `Page` has finished
    /// loading, which comes after the response.
    Navigate(NavigationId),
    /// A common request received via a channel (`Page`).
    ExternalCommand {
        /// `None` is the browser/root transport path; page and frame commands
        /// always retain their real session identity here.
        session_id: Option<SessionId>,
        tx: OneshotSender<Result<Response>>,
    },
    /// Requests that are initiated directly from a `Target` (all the
    /// initialization commands).
    InternalCommand(TargetId, Option<SessionId>),
    /// One response-confirmed command in a multi-session fan-out operation.
    FanOutAckMember {
        group_id: AckGroupId,
        session_id: SessionId,
    },
    /// Add one tracked preload to a child session, whether sourced from its
    /// initialization snapshot or a later existing-session fan-out.
    PreloadAddScript {
        target_id: TargetId,
        session_id: SessionId,
        preload_key: PreloadId,
    },
    /// Add a public preload to the main session while retaining its complete
    /// parameters until the returned identifier can be tracked.
    AddPreloadScriptMain {
        target_id: TargetId,
        params: AddScriptToEvaluateOnNewDocumentParams,
        tx: OneshotSender<Result<ScriptIdentifier>>,
    },
    // A Request to close the browser.
    CloseBrowser(OneshotSender<Result<CloseReturns>>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct AckGroupId(u64);

#[derive(Debug)]
struct AckGroup {
    target_id: TargetId,
    /// Stored at creation so a detached child remains distinguishable from
    /// the main session when a racing response is classified.
    main_session_id: SessionId,
    /// Counts CDP command responses, not sessions. A session may contribute
    /// more than one member while preserving one caller acknowledgement.
    remaining: usize,
    tx: OneshotSender<Result<()>>,
}

#[derive(Debug)]
enum MemberOutcome {
    Complete,
    Err(CdpError),
}

/// Events used internally to communicate with the handler, which are executed
/// in the background
// TODO rename to BrowserMessage
#[derive(Debug)]
pub(crate) enum HandlerMessage {
    CreatePage(CreateTargetParams, OneshotSender<Result<Page>>),
    FetchTargets(OneshotSender<Result<Vec<TargetInfo>>>),
    InsertContext(BrowserContext),
    DisposeContext(BrowserContext),
    GetPages(OneshotSender<Vec<Page>>),
    Command(CommandMessage),
    GetPage(TargetId, OneshotSender<Option<Page>>),
    AddEventListener(EventListenerRequest),
    CloseBrowser(OneshotSender<Result<CloseReturns>>),
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_tungstenite::tungstenite::Message as WsMessage;
    use futures::channel::{mpsc, oneshot};
    use futures::future::BoxFuture;
    use futures::task::noop_waker_ref;
    use futures::{SinkExt, StreamExt};
    use serde_json::json;
    use tokio::net::TcpListener;

    use chromiumoxide_cdp::cdp::browser_protocol::page::FrameId;
    use chromiumoxide_cdp::cdp::js_protocol::runtime::RunIfWaitingForDebuggerParams;

    use super::*;

    #[derive(Clone)]
    struct SharedLogWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedLogWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("test log buffer lock")
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn session(id: &str) -> SessionId {
        SessionId::new(id)
    }

    fn target_id(id: &str) -> TargetId {
        TargetId::new(id)
    }

    fn target_info(id: &str) -> TargetInfo {
        TargetInfo::builder()
            .target_id(target_id(id))
            .r#type("page")
            .title(id)
            .url("about:blank")
            .attached(true)
            .can_access_opener(false)
            .build()
            .expect("target fixture has all mandatory fields")
    }

    fn frame_navigated_event(
        session_id: &SessionId,
        frame_id: &str,
        parent_id: Option<&str>,
        url: &str,
    ) -> CdpEventMessage {
        let mut frame = json!({
            "id": frame_id,
            "loaderId": format!("loader-{frame_id}"),
            "url": url,
            "domainAndRegistry": "example",
            "securityOrigin": "https://example.test",
            "mimeType": "text/html",
            "secureContextType": "Secure",
            "crossOriginIsolatedContextType": "NotIsolated",
            "gatedAPIFeatures": []
        });
        if let Some(parent_id) = parent_id {
            frame["parentId"] = json!(parent_id);
        }
        let event = serde_json::from_value(json!({
            "frame": frame,
            "type": "Navigation"
        }))
        .expect("frameNavigated fixture is valid");
        CdpEventMessage {
            method: "Page.frameNavigated".into(),
            session_id: Some(session_id.as_ref().to_owned()),
            params: CdpEvent::PageFrameNavigated(Box::new(event)),
        }
    }

    async fn test_handler() -> (Handler, oneshot::Sender<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local websocket listener");
        let address = listener.local_addr().expect("listener address");
        let (close_tx, close_rx) = oneshot::channel();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept websocket client");
            let mut websocket = async_tungstenite::tokio::accept_async(stream)
                .await
                .expect("complete websocket handshake");
            let _ = close_rx.await;
            let _ = websocket.close(None).await;
        });

        let connection = Connection::connect(format!("ws://{address}"))
            .await
            .expect("connect test handler");
        let (_tx, rx) = mpsc::channel(1);
        (
            Handler::new(connection, rx, HandlerConfig::default()),
            close_tx,
        )
    }

    async fn test_handler_with_sender()
    -> (Handler, mpsc::Sender<HandlerMessage>, oneshot::Sender<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local websocket listener");
        let address = listener.local_addr().expect("listener address");
        let (close_tx, close_rx) = oneshot::channel();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept websocket client");
            let mut websocket = async_tungstenite::tokio::accept_async(stream)
                .await
                .expect("complete websocket handshake");
            let _ = close_rx.await;
            let _ = websocket.close(None).await;
        });

        let connection = Connection::connect(format!("ws://{address}"))
            .await
            .expect("connect test handler");
        let (tx, rx) = mpsc::channel(4);
        (
            Handler::new(connection, rx, HandlerConfig::default()),
            tx,
            close_tx,
        )
    }

    fn poll_handler_once(handler: &mut Handler) {
        let mut cx = Context::from_waker(noop_waker_ref());
        let _ = Pin::new(handler).poll_next(&mut cx);
    }

    fn poll_future_once<F: std::future::Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
        let mut cx = Context::from_waker(noop_waker_ref());
        future.poll(&mut cx)
    }

    async fn test_handler_with_message(message: WsMessage) -> (Handler, oneshot::Sender<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local websocket listener");
        let address = listener.local_addr().expect("listener address");
        let (release_tx, release_rx) = oneshot::channel();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept websocket client");
            let mut websocket = async_tungstenite::tokio::accept_async(stream)
                .await
                .expect("complete websocket handshake");
            websocket.send(message).await.expect("send test message");
            let _ = release_rx.await;
        });

        let connection = Connection::connect(format!("ws://{address}"))
            .await
            .expect("connect test handler");
        let (_tx, rx) = mpsc::channel(1);
        (
            Handler::new(connection, rx, HandlerConfig::default()),
            release_tx,
        )
    }

    #[tokio::test]
    async fn child_session_event_route_reaches_parent_target_and_rejects_draining_work() {
        let (mut handler, close_tx) = test_handler().await;
        let (target_id, main_session) = install_target(&mut handler, "target");
        handler.on_event(frame_navigated_event(
            &main_session,
            "main-frame",
            None,
            "https://main.example/",
        ));

        let child_session = session("child");
        handler.sessions.insert(
            child_session.clone(),
            Session::new(child_session.clone(), target_id.clone()),
        );
        handler
            .targets
            .get_mut(&target_id)
            .expect("parent target remains registered")
            .frame_manager_mut()
            .on_frame_attached_in_session(
                chromiumoxide_cdp::cdp::browser_protocol::page::FrameId::new("child-frame"),
                Some(chromiumoxide_cdp::cdp::browser_protocol::page::FrameId::new("main-frame")),
                child_session.clone(),
            );
        handler.on_event(frame_navigated_event(
            &child_session,
            "child-frame",
            Some("main-frame"),
            "https://child.example/first",
        ));

        let child_frame_id =
            chromiumoxide_cdp::cdp::browser_protocol::page::FrameId::new("child-frame");
        let target = handler
            .targets
            .get(&target_id)
            .expect("parent target remains registered");
        let child_frame = target
            .frame_manager()
            .frame(&child_frame_id)
            .expect("child event reached the parent target");
        assert_eq!(child_frame.session_id(), Some(&child_session));
        assert_eq!(child_frame.url(), Some("https://child.example/first"));

        handler
            .targets
            .get_mut(&target_id)
            .expect("parent target remains registered")
            .enter_draining_session(child_session.clone());
        handler.on_event(frame_navigated_event(
            &child_session,
            "child-frame",
            Some("main-frame"),
            "https://child.example/late",
        ));
        assert_eq!(
            handler
                .targets
                .get(&target_id)
                .and_then(|target| target.frame_manager().frame(&child_frame_id))
                .and_then(|frame| frame.url()),
            Some("https://child.example/first")
        );

        let _ = close_tx.send(());
    }

    #[tokio::test]
    async fn captured_session_command_rejects_a_stale_session_before_wire_submit() {
        let (mut handler, close_tx) = test_handler().await;
        let (target_id, _) = install_target(&mut handler, "target");
        let mut target = handler
            .targets
            .remove(&target_id)
            .expect("target remains registered");
        let (tx, rx) = oneshot::channel();
        let command = CommandMessage::with_session(
            RunIfWaitingForDebuggerParams::default(),
            tx,
            Some(session("detached-child")),
        )
        .expect("command serializes");

        handler.on_target_message(&mut target, command, Instant::now());

        assert!(matches!(
            rx.await.expect("stale command sender resolves"),
            Err(CdpError::FrameNotReady)
        ));
        assert!(handler.pending_commands.is_empty());
        handler.targets.insert(target_id, target);
        let _ = close_tx.send(());
    }

    #[tokio::test]
    async fn get_pages_marks_only_successfully_delivered_pages_as_exposed() {
        let (mut handler, mut sender, close_tx) = test_handler_with_sender().await;
        let (target_id, _) = install_target(&mut handler, "target");
        let (tx, rx) = oneshot::channel();
        sender
            .send(HandlerMessage::GetPages(tx))
            .await
            .expect("handler channel remains live");
        poll_handler_once(&mut handler);

        assert_eq!(rx.await.expect("page list is delivered").len(), 1);
        assert!(
            handler
                .targets
                .get(&target_id)
                .expect("target remains registered")
                .page_exposed()
        );
        let _ = close_tx.send(());

        let (mut handler, mut sender, close_tx) = test_handler_with_sender().await;
        let (target_id, _) = install_target(&mut handler, "canceled");
        let (tx, rx) = oneshot::channel();
        drop(rx);
        sender
            .send(HandlerMessage::GetPages(tx))
            .await
            .expect("handler channel remains live");
        poll_handler_once(&mut handler);
        assert!(
            !handler
                .targets
                .get(&target_id)
                .expect("target remains registered")
                .page_exposed()
        );
        let _ = close_tx.send(());
    }

    #[tokio::test]
    async fn get_page_marks_exposure_after_successful_delivery() {
        let (mut handler, mut sender, close_tx) = test_handler_with_sender().await;
        let (target_id, _) = install_target(&mut handler, "target");
        let (tx, rx) = oneshot::channel();
        sender
            .send(HandlerMessage::GetPage(target_id.clone(), tx))
            .await
            .expect("handler channel remains live");
        poll_handler_once(&mut handler);

        assert!(rx.await.expect("page response is delivered").is_some());
        assert!(
            handler
                .targets
                .get(&target_id)
                .expect("target remains registered")
                .page_exposed()
        );
        let _ = close_tx.send(());
    }

    fn add_ack_group(
        handler: &mut Handler,
        target_id: TargetId,
        main_session_id: SessionId,
        remaining: usize,
    ) -> (AckGroupId, oneshot::Receiver<Result<()>>) {
        let (tx, rx) = oneshot::channel();
        let group_id = handler.next_ack_group_id();
        handler.pending_ack_groups.insert(
            group_id,
            AckGroup {
                target_id,
                main_session_id,
                remaining,
                tx,
            },
        );
        (group_id, rx)
    }

    fn add_ack_member(
        handler: &mut Handler,
        call_id: usize,
        group_id: AckGroupId,
        session_id: SessionId,
    ) {
        handler.pending_commands.insert(
            CallId::new(call_id),
            (
                PendingRequest::FanOutAckMember {
                    group_id,
                    session_id,
                },
                "Fetch.enable".into(),
                Instant::now(),
            ),
        );
    }

    fn response(call_id: usize, error: Option<ProtocolError>) -> Response {
        Response {
            id: CallId::new(call_id),
            result: error.is_none().then(|| json!({})),
            error,
        }
    }

    fn preload_response(call_id: usize, identifier: &str) -> Response {
        Response {
            id: CallId::new(call_id),
            result: Some(json!({ "identifier": identifier })),
            error: None,
        }
    }

    fn navigation_response(call_id: usize, error_text: Option<&str>) -> Response {
        let mut result = json!({ "frameId": "main-frame" });
        if let Some(error_text) = error_text {
            result["errorText"] = json!(error_text);
        }
        Response {
            id: CallId::new(call_id),
            result: Some(result),
            error: None,
        }
    }

    fn protocol_error(code: i64) -> ProtocolError {
        ProtocolError {
            code,
            message: format!("protocol error {code}"),
        }
    }

    fn session_request(method: &str, session_id: &SessionId) -> CdpRequest {
        CdpRequest::with_session(method.to_owned().into(), json!({}), session_id.as_ref())
    }

    fn install_target(handler: &mut Handler, id: &str) -> (TargetId, SessionId) {
        let target_id = target_id(id);
        let main_session = session(&format!("{id}-main"));
        let mut target = Target::new(
            target_info(id),
            TargetConfig::default(),
            BrowserContext::default(),
        );
        target.set_session_id(main_session.clone());
        handler.sessions.insert(
            main_session.clone(),
            Session::new(main_session.clone(), target_id.clone()),
        );
        handler.target_ids.push(target_id.clone());
        handler.targets.insert(target_id.clone(), target);
        (target_id, main_session)
    }

    fn install_page_teardown_waiters(
        handler: &mut Handler,
        target_id: &TargetId,
    ) -> (
        BoxFuture<'static, Result<crate::ArcHttpRequest>>,
        oneshot::Receiver<Result<Page>>,
    ) {
        let target = handler
            .targets
            .get_mut(target_id)
            .expect("target remains registered");
        let page = target
            .get_or_create_page()
            .expect("attached target creates a page handle")
            .clone();
        let mut wait = page.wait_for_navigation();
        assert!(poll_future_once(wait.as_mut()).is_pending());

        let (initiator_tx, initiator_rx) = oneshot::channel();
        target.set_initiator(initiator_tx);
        (wait, initiator_rx)
    }

    #[tokio::test]
    async fn preload_main_failures_do_not_allocate_and_success_fans_out_before_completion() {
        let (mut handler, _close) = test_handler().await;
        let (target_id, main_session) = install_target(&mut handler, "preload-target");
        handler.on_event(frame_navigated_event(
            &main_session,
            "main-frame",
            None,
            "https://main.example/",
        ));

        let child_session = session("preload-child");
        handler.sessions.insert(
            child_session.clone(),
            Session::new(child_session.clone(), target_id.clone()),
        );
        handler
            .targets
            .get_mut(&target_id)
            .expect("target remains registered")
            .frame_manager_mut()
            .on_frame_attached_in_session(
                FrameId::new("child-frame"),
                Some(FrameId::new("main-frame")),
                child_session.clone(),
            );

        let failed_params = AddScriptToEvaluateOnNewDocumentParams::new("globalThis.failed = true");
        let (failed_tx, failed_rx) = oneshot::channel();
        handler.pending_commands.insert(
            CallId::new(200),
            (
                PendingRequest::AddPreloadScriptMain {
                    target_id: target_id.clone(),
                    params: failed_params,
                    tx: failed_tx,
                },
                AddScriptToEvaluateOnNewDocumentParams::IDENTIFIER.into(),
                Instant::now(),
            ),
        );
        handler.on_response(response(200, Some(protocol_error(-32000))));
        assert!(matches!(
            failed_rx.await.expect("failed preload sender resolves"),
            Err(CdpError::Chrome(error)) if error.code == -32000
        ));
        assert!(
            handler.targets[&target_id]
                .frame_manager()
                .preload_snapshot()
                .is_empty()
        );

        let timed_out_params =
            AddScriptToEvaluateOnNewDocumentParams::new("globalThis.timedOut = true");
        let (timed_out_tx, timed_out_rx) = oneshot::channel();
        let now = Instant::now();
        handler.pending_commands.insert(
            CallId::new(201),
            (
                PendingRequest::AddPreloadScriptMain {
                    target_id: target_id.clone(),
                    params: timed_out_params,
                    tx: timed_out_tx,
                },
                AddScriptToEvaluateOnNewDocumentParams::IDENTIFIER.into(),
                now - handler.config.request_timeout - Duration::from_millis(1),
            ),
        );
        handler.evict_timed_out_commands(now);
        assert!(matches!(
            timed_out_rx
                .await
                .expect("timed-out preload sender resolves"),
            Err(CdpError::Timeout)
        ));
        assert!(
            handler.targets[&target_id]
                .frame_manager()
                .preload_snapshot()
                .is_empty()
        );

        let first_params = AddScriptToEvaluateOnNewDocumentParams::new("globalThis.first = true");
        let (first_tx, first_rx) = oneshot::channel();
        handler.pending_commands.insert(
            CallId::new(202),
            (
                PendingRequest::AddPreloadScriptMain {
                    target_id: target_id.clone(),
                    params: first_params.clone(),
                    tx: first_tx,
                },
                AddScriptToEvaluateOnNewDocumentParams::IDENTIFIER.into(),
                Instant::now(),
            ),
        );
        handler.on_response(preload_response(202, "main-script-0"));

        let target = &handler.targets[&target_id];
        let tracked = target
            .frame_manager()
            .preload_script(0)
            .expect("first successful preload uses the first stable key");
        assert_eq!(tracked.id, 0);
        assert_eq!(tracked.params, first_params);
        assert_eq!(tracked.main_id.as_ref(), "main-script-0");
        assert!(matches!(
            target.queued_events().front(),
            Some(TargetEvent::QueuePreloadScript {
                request,
                preload_key: 0,
            }) if request.session_id.as_deref() == Some(child_session.as_ref())
        ));
        assert_eq!(
            first_rx
                .await
                .expect("successful preload sender resolves")
                .expect("main preload succeeds")
                .as_ref(),
            "main-script-0"
        );

        let second_params = AddScriptToEvaluateOnNewDocumentParams::new("globalThis.second = true");
        let (second_tx, second_rx) = oneshot::channel();
        handler.pending_commands.insert(
            CallId::new(203),
            (
                PendingRequest::AddPreloadScriptMain {
                    target_id: target_id.clone(),
                    params: second_params,
                    tx: second_tx,
                },
                AddScriptToEvaluateOnNewDocumentParams::IDENTIFIER.into(),
                Instant::now(),
            ),
        );
        handler.on_response(preload_response(203, "main-script-1"));
        assert_eq!(
            second_rx
                .await
                .expect("second preload sender resolves")
                .expect("second main preload succeeds")
                .as_ref(),
            "main-script-1"
        );
        assert_eq!(
            handler.targets[&target_id]
                .frame_manager()
                .preload_snapshot()
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[tokio::test]
    async fn preload_child_response_tracks_only_a_live_registered_session() {
        let (mut handler, _close) = test_handler().await;
        let (target_id, main_session) = install_target(&mut handler, "preload-target");
        handler.on_event(frame_navigated_event(
            &main_session,
            "main-frame",
            None,
            "https://main.example/",
        ));
        let preload_key = handler
            .targets
            .get_mut(&target_id)
            .expect("target remains registered")
            .frame_manager_mut()
            .add_preload_script(
                AddScriptToEvaluateOnNewDocumentParams::new("globalThis.preloaded = true"),
                ScriptIdentifier::new("main-script"),
            );

        let live_child = session("live-child");
        handler.sessions.insert(
            live_child.clone(),
            Session::new(live_child.clone(), target_id.clone()),
        );
        handler
            .targets
            .get_mut(&target_id)
            .expect("target remains registered")
            .frame_manager_mut()
            .on_frame_attached_in_session(
                FrameId::new("live-frame"),
                Some(FrameId::new("main-frame")),
                live_child.clone(),
            );
        handler.pending_commands.insert(
            CallId::new(210),
            (
                PendingRequest::PreloadAddScript {
                    target_id: target_id.clone(),
                    session_id: live_child.clone(),
                    preload_key,
                },
                AddScriptToEvaluateOnNewDocumentParams::IDENTIFIER.into(),
                Instant::now(),
            ),
        );
        handler.on_response(preload_response(210, "live-child-script"));
        assert_eq!(
            handler.targets[&target_id]
                .frame_manager()
                .preload_script(preload_key)
                .and_then(|preload| preload.per_session_ids.get(&live_child))
                .map(|id| id.as_ref()),
            Some("live-child-script")
        );

        handler
            .targets
            .get_mut(&target_id)
            .expect("target remains registered")
            .frame_manager_mut()
            .on_detached_from_target(&live_child, &main_session);
        handler.sessions.remove(&live_child);
        assert!(
            !handler.targets[&target_id]
                .frame_manager()
                .preload_script(preload_key)
                .expect("tracked preload remains live")
                .per_session_ids
                .contains_key(&live_child)
        );

        let detached_child = session("detached-child");
        handler.sessions.insert(
            detached_child.clone(),
            Session::new(detached_child.clone(), target_id.clone()),
        );
        handler
            .targets
            .get_mut(&target_id)
            .expect("target remains registered")
            .frame_manager_mut()
            .on_frame_attached_in_session(
                FrameId::new("detached-frame"),
                Some(FrameId::new("main-frame")),
                detached_child.clone(),
            );
        handler.pending_commands.insert(
            CallId::new(211),
            (
                PendingRequest::PreloadAddScript {
                    target_id: target_id.clone(),
                    session_id: detached_child.clone(),
                    preload_key,
                },
                AddScriptToEvaluateOnNewDocumentParams::IDENTIFIER.into(),
                Instant::now(),
            ),
        );
        let mut target = handler
            .targets
            .remove(&target_id)
            .expect("target remains registered");
        handler.fail_pending_for_session(&mut target, &detached_child);
        target
            .frame_manager_mut()
            .on_detached_from_target(&detached_child, &main_session);
        handler.targets.insert(target_id.clone(), target);
        handler.sessions.remove(&detached_child);
        assert!(!handler.pending_commands.contains_key(&CallId::new(211)));
        handler.on_response(preload_response(211, "late-child-script"));
        assert!(
            !handler.targets[&target_id]
                .frame_manager()
                .preload_script(preload_key)
                .expect("tracked preload remains live")
                .per_session_ids
                .contains_key(&detached_child)
        );

        let draining_child = session("draining-child");
        handler.sessions.insert(
            draining_child.clone(),
            Session::new(draining_child.clone(), target_id.clone()),
        );
        let target = handler
            .targets
            .get_mut(&target_id)
            .expect("target remains registered");
        target.frame_manager_mut().on_frame_attached_in_session(
            FrameId::new("draining-frame"),
            Some(FrameId::new("main-frame")),
            draining_child.clone(),
        );
        target.enter_draining_session(draining_child.clone());
        handler.pending_commands.insert(
            CallId::new(212),
            (
                PendingRequest::PreloadAddScript {
                    target_id: target_id.clone(),
                    session_id: draining_child.clone(),
                    preload_key,
                },
                AddScriptToEvaluateOnNewDocumentParams::IDENTIFIER.into(),
                Instant::now(),
            ),
        );
        handler.on_response(preload_response(212, "draining-child-script"));
        assert!(
            !handler.targets[&target_id]
                .frame_manager()
                .preload_script(preload_key)
                .expect("tracked preload remains live")
                .per_session_ids
                .contains_key(&draining_child)
        );
    }

    #[tokio::test]
    async fn preload_isolated_world_response_does_not_create_a_tracked_script() {
        let (mut handler, _close) = test_handler().await;
        let (target_id, main_session) = install_target(&mut handler, "preload-target");
        handler.pending_commands.insert(
            CallId::new(220),
            (
                PendingRequest::InternalCommand(target_id.clone(), Some(main_session)),
                AddScriptToEvaluateOnNewDocumentParams::IDENTIFIER.into(),
                Instant::now(),
            ),
        );

        handler.on_response(preload_response(220, "utility-world-script"));

        assert!(
            handler.targets[&target_id]
                .frame_manager()
                .preload_snapshot()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn preload_main_lifecycle_failures_settle_the_user_sender() {
        let (mut handler, _close) = test_handler().await;
        let (target_id, main_session) = install_target(&mut handler, "preload-target");
        let params = AddScriptToEvaluateOnNewDocumentParams::new("globalThis.preloaded = true");

        let (target_tx, target_rx) = oneshot::channel();
        handler.pending_commands.insert(
            CallId::new(230),
            (
                PendingRequest::AddPreloadScriptMain {
                    target_id: target_id.clone(),
                    params: params.clone(),
                    tx: target_tx,
                },
                AddScriptToEvaluateOnNewDocumentParams::IDENTIFIER.into(),
                Instant::now(),
            ),
        );
        handler.fail_pending_for_target(&target_id, Some(&main_session));
        assert!(matches!(
            target_rx.await.expect("target teardown sender resolves"),
            Err(CdpError::NoResponse)
        ));

        let (connection_tx, connection_rx) = oneshot::channel();
        handler.pending_commands.insert(
            CallId::new(231),
            (
                PendingRequest::AddPreloadScriptMain {
                    target_id,
                    params,
                    tx: connection_tx,
                },
                AddScriptToEvaluateOnNewDocumentParams::IDENTIFIER.into(),
                Instant::now(),
            ),
        );
        handler.fail_all_pending_on_connection_close();
        assert!(matches!(
            connection_rx
                .await
                .expect("connection teardown sender resolves"),
            Err(CdpError::NoResponse)
        ));
    }

    fn install_frame_navigation_watcher(
        handler: &mut Handler,
        target_id: &TargetId,
        session_id: &SessionId,
        navigation_id: NavigationId,
    ) -> oneshot::Receiver<std::result::Result<(), crate::handler::frame::FrameWaitError>> {
        handler.on_event(frame_navigated_event(
            session_id,
            "main-frame",
            None,
            "https://main.example/",
        ));
        let frame_id = FrameId::new("main-frame");
        let (wait_tx, wait_rx) = oneshot::channel();
        let target = handler
            .targets
            .get_mut(target_id)
            .expect("target remains registered");
        target.frame_manager_mut().wait_for_navigation(
            session_id.clone(),
            frame_id.clone(),
            wait_tx,
        );
        target.frame_manager_mut().navigate_frame_in_session(
            session_id.clone(),
            frame_id,
            FrameNavigationRequest::new(
                navigation_id,
                CdpRequest::with_session(
                    "Page.navigate".into(),
                    json!({ "url": "https://next.example/" }),
                    session_id.as_ref(),
                ),
            ),
        );
        assert!(matches!(
            target.frame_manager_mut().poll(Instant::now()),
            Some(crate::handler::frame::FrameEvent::NavigationRequest(id, _))
                if id == navigation_id
        ));
        wait_rx
    }

    #[tokio::test]
    async fn navigation_response_without_error_text_waits_for_lifecycle() {
        let (mut handler, _close) = test_handler().await;
        let (target_id, session_id) = install_target(&mut handler, "target");
        let navigation_id = NavigationId(100);
        let (tx, mut rx) = oneshot::channel();
        handler.navigations.insert(
            navigation_id,
            NavigationRequest::Navigate(NavigationInProgress::new(target_id, session_id, tx)),
        );

        handler.on_navigation_response(navigation_id, navigation_response(100, None));
        assert!(matches!(rx.try_recv(), Ok(None)));
        handler.on_navigation_lifecycle_completed(Ok(NavigationOk::NewDocumentNavigation(
            navigation_id,
        )));

        let response = rx
            .await
            .expect("navigation sender remains live")
            .expect("normal navigation succeeds");
        assert!(response.error.is_none());
    }

    #[tokio::test]
    async fn navigation_http_response_failure_waits_for_lifecycle_and_succeeds() {
        let (mut handler, _close) = test_handler().await;
        let (target_id, session_id) = install_target(&mut handler, "target");
        let navigation_id = NavigationId(101);
        let (tx, mut rx) = oneshot::channel();
        handler.navigations.insert(
            navigation_id,
            NavigationRequest::Navigate(NavigationInProgress::new(target_id, session_id, tx)),
        );

        handler.on_navigation_response(
            navigation_id,
            navigation_response(101, Some(HTTP_RESPONSE_CODE_FAILURE)),
        );
        assert!(matches!(rx.try_recv(), Ok(None)));
        handler.on_navigation_lifecycle_completed(Ok(NavigationOk::NewDocumentNavigation(
            navigation_id,
        )));

        let response = rx
            .await
            .expect("navigation sender remains live")
            .expect("HTTP error page navigation succeeds");
        let result = serde_json::from_value::<NavigateReturns>(
            response.result.expect("navigate response has a result"),
        )
        .expect("navigate result is valid");
        assert_eq!(
            result.error_text.as_deref(),
            Some(HTTP_RESPONSE_CODE_FAILURE)
        );
    }

    #[tokio::test]
    async fn navigation_non_http_error_text_fails_immediately_and_cancels_waiter() {
        let (mut handler, _close) = test_handler().await;
        let (target_id, session_id) = install_target(&mut handler, "target");
        let navigation_id = NavigationId(102);
        let wait_rx =
            install_frame_navigation_watcher(&mut handler, &target_id, &session_id, navigation_id);
        let (tx, rx) = oneshot::channel();
        handler.navigations.insert(
            navigation_id,
            NavigationRequest::Navigate(NavigationInProgress::new(
                target_id.clone(),
                session_id,
                tx,
            )),
        );

        handler.on_navigation_response(
            navigation_id,
            navigation_response(102, Some("net::ERR_ABORTED")),
        );

        assert!(matches!(
            rx.await.expect("navigation sender remains live"),
            Err(CdpError::ChromeMessage(message)) if message == "net::ERR_ABORTED"
        ));
        assert_eq!(
            wait_rx.await.expect("waiter sender remains live"),
            Err(crate::handler::frame::FrameWaitError::NavigationFailed(
                "net::ERR_ABORTED".to_owned()
            ))
        );
        assert!(
            handler
                .targets
                .get_mut(&target_id)
                .expect("target remains registered")
                .frame_manager_mut()
                .poll(Instant::now() + Duration::from_secs(60))
                .is_none()
        );
    }

    #[tokio::test]
    async fn navigation_protocol_error_fails_immediately_and_cancels_waiter() {
        let (mut handler, _close) = test_handler().await;
        let (target_id, session_id) = install_target(&mut handler, "target");
        let navigation_id = NavigationId(103);
        let wait_rx =
            install_frame_navigation_watcher(&mut handler, &target_id, &session_id, navigation_id);
        let (tx, rx) = oneshot::channel();
        handler.navigations.insert(
            navigation_id,
            NavigationRequest::Navigate(NavigationInProgress::new(target_id, session_id, tx)),
        );

        handler.on_navigation_response(
            navigation_id,
            Response {
                id: CallId::new(103),
                result: None,
                error: Some(protocol_error(-32001)),
            },
        );

        assert!(matches!(
            rx.await.expect("navigation sender remains live"),
            Err(CdpError::Chrome(error)) if error.code == -32001
        ));
        assert_eq!(
            wait_rx.await.expect("waiter sender remains live"),
            Err(crate::handler::frame::FrameWaitError::NavigationFailed(
                "protocol error -32001".to_owned()
            ))
        );
    }

    #[tokio::test]
    async fn navigation_lifecycle_failure_removes_pending_call_before_late_response() {
        let (mut handler, _close) = test_handler().await;
        let (target_id, session_id) = install_target(&mut handler, "target");
        let navigation_id = NavigationId(104);
        let (tx, rx) = oneshot::channel();
        handler.navigations.insert(
            navigation_id,
            NavigationRequest::Navigate(NavigationInProgress::new(target_id, session_id, tx)),
        );
        handler.pending_commands.insert(
            CallId::new(104),
            (
                PendingRequest::Navigate(navigation_id),
                "Page.navigate".into(),
                Instant::now(),
            ),
        );

        handler.on_navigation_lifecycle_completed(Err(NavigationError::FrameNotFound {
            id: navigation_id,
            frame: FrameId::new("main-frame"),
        }));

        assert!(matches!(
            rx.await.expect("navigation sender remains live"),
            Err(CdpError::FrameNotFound(frame)) if frame == FrameId::new("main-frame")
        ));
        assert!(!handler.pending_commands.contains_key(&CallId::new(104)));
        handler.on_response(navigation_response(104, None));
        assert!(!handler.navigations.contains_key(&navigation_id));
    }

    #[tokio::test]
    async fn ack_fan_out_batch_submits_each_disjoint_request_once() {
        let (mut handler, _close) = test_handler().await;
        let main = session("main");
        let child = session("child");
        let warming_child = session("warming-child");
        let (ack_tx, ack_rx) = oneshot::channel();
        handler.submit_fan_out_ack_batch(
            &target_id("target"),
            FanOutAckBatch {
                ack_reqs: vec![
                    session_request("Network.setCacheDisabled", &main),
                    session_request("Fetch.enable", &main),
                    session_request("Network.setCacheDisabled", &child),
                    session_request("Fetch.enable", &child),
                ],
                send_only_reqs: vec![
                    session_request("Network.setCacheDisabled", &warming_child),
                    session_request("Fetch.enable", &warming_child),
                ],
                ack_tx,
                main_session_id: main,
            },
            Instant::now(),
        );

        assert_eq!(handler.pending_commands.len(), 6);
        let group_id = *handler
            .pending_ack_groups
            .keys()
            .next()
            .expect("one ack group was registered");
        assert_eq!(handler.pending_ack_groups[&group_id].remaining, 4);
        assert_eq!(
            handler
                .pending_commands
                .values()
                .filter(|(pending, _, _)| matches!(pending, PendingRequest::FanOutAckMember { .. }))
                .count(),
            4
        );
        assert_eq!(
            handler
                .pending_commands
                .values()
                .filter(|(pending, _, _)| matches!(pending, PendingRequest::InternalCommand(_, _)))
                .count(),
            2
        );

        let member_calls = handler
            .pending_commands
            .iter()
            .filter_map(|(call_id, (pending, _, _))| {
                matches!(pending, PendingRequest::FanOutAckMember { .. }).then_some(*call_id)
            })
            .collect::<Vec<_>>();
        for call_id in member_calls {
            handler.on_response(Response {
                id: call_id,
                result: Some(json!({})),
                error: None,
            });
        }
        assert!(ack_rx.await.expect("ack sender remains live").is_ok());
    }

    #[tokio::test]
    async fn ack_empty_fan_out_batch_resolves_without_registering_a_group() {
        let (mut handler, _close) = test_handler().await;
        let (ack_tx, ack_rx) = oneshot::channel();
        handler.submit_fan_out_ack_batch(
            &target_id("target"),
            FanOutAckBatch {
                ack_reqs: Vec::new(),
                send_only_reqs: Vec::new(),
                ack_tx,
                main_session_id: session("main"),
            },
            Instant::now(),
        );

        assert!(handler.pending_commands.is_empty());
        assert!(handler.pending_ack_groups.is_empty());
        assert!(ack_rx.await.expect("empty batch ack remains live").is_ok());
    }

    #[tokio::test]
    async fn ack_two_commands_per_session_complete_only_after_all_responses() {
        let (mut handler, _close) = test_handler().await;
        let main = session("main");
        let child = session("child");
        let (group_id, rx) = add_ack_group(&mut handler, target_id("target"), main.clone(), 4);
        add_ack_member(&mut handler, 10, group_id, main.clone());
        add_ack_member(&mut handler, 11, group_id, main);
        add_ack_member(&mut handler, 12, group_id, child.clone());
        add_ack_member(&mut handler, 13, group_id, child);

        for call_id in [10, 11, 12] {
            handler.on_response(response(call_id, None));
        }
        assert_eq!(
            handler
                .pending_ack_groups
                .get(&group_id)
                .map(|group| group.remaining),
            Some(1)
        );
        handler.on_response(response(13, None));

        assert!(!handler.pending_ack_groups.contains_key(&group_id));
        assert!(rx.await.expect("ack sender remains live").is_ok());
    }

    #[tokio::test]
    async fn ack_child_session_gone_is_benign_after_registry_removal() {
        let (mut handler, _close) = test_handler().await;
        let main = session("main");
        let child = session("child");
        let (group_id, rx) = add_ack_group(&mut handler, target_id("target"), main, 1);
        add_ack_member(&mut handler, 20, group_id, child);

        handler.on_response(response(20, Some(protocol_error(-32001))));

        assert!(rx.await.expect("ack sender remains live").is_ok());
    }

    #[tokio::test]
    async fn ack_real_child_error_fails_fast_and_purges_stragglers() {
        let (mut handler, _close) = test_handler().await;
        let main = session("main");
        let child = session("child");
        let (group_id, rx) = add_ack_group(&mut handler, target_id("target"), main, 2);
        add_ack_member(&mut handler, 30, group_id, child.clone());
        add_ack_member(&mut handler, 31, group_id, child);

        handler.on_response(response(30, Some(protocol_error(-32000))));

        assert!(!handler.pending_ack_groups.contains_key(&group_id));
        assert!(!handler.pending_commands.contains_key(&CallId::new(31)));
        assert!(matches!(
            rx.await.expect("ack sender remains live"),
            Err(CdpError::Chrome(error)) if error.code == -32000
        ));
        handler.on_response(response(31, None));
    }

    #[tokio::test]
    async fn ack_main_session_gone_error_is_not_ignorable() {
        let (mut handler, _close) = test_handler().await;
        let main = session("main");
        let (group_id, rx) = add_ack_group(&mut handler, target_id("target"), main.clone(), 1);
        add_ack_member(&mut handler, 40, group_id, main);

        handler.on_response(response(40, Some(protocol_error(-32001))));

        assert!(matches!(
            rx.await.expect("ack sender remains live"),
            Err(CdpError::Chrome(error)) if error.code == -32001
        ));
    }

    #[tokio::test]
    async fn ack_timeout_fails_group_and_removes_other_members() {
        let (mut handler, _close) = test_handler().await;
        let main = session("main");
        let (group_id, rx) = add_ack_group(&mut handler, target_id("target"), main.clone(), 2);
        add_ack_member(&mut handler, 50, group_id, main.clone());
        add_ack_member(&mut handler, 51, group_id, main);
        for (_, _, submitted_at) in handler.pending_commands.values_mut() {
            *submitted_at = Instant::now() - Duration::from_secs(60);
        }

        handler.evict_timed_out_commands(Instant::now());

        assert!(!handler.pending_commands.contains_key(&CallId::new(51)));
        assert!(matches!(
            rx.await.expect("ack sender remains live"),
            Err(CdpError::Timeout)
        ));
    }

    #[tokio::test]
    async fn ack_child_detach_completes_each_member_without_failing_main() {
        let (mut handler, _close) = test_handler().await;
        let main = session("main");
        let child = session("child");
        let mut target = Target::new(
            target_info("target"),
            TargetConfig::default(),
            BrowserContext::default(),
        );
        target.set_session_id(main.clone());
        let (group_id, rx) = add_ack_group(&mut handler, target_id("target"), main.clone(), 3);
        add_ack_member(&mut handler, 60, group_id, child.clone());
        add_ack_member(&mut handler, 61, group_id, child.clone());
        add_ack_member(&mut handler, 62, group_id, main);

        handler.fail_pending_for_session(&mut target, &child);

        assert_eq!(
            handler
                .pending_ack_groups
                .get(&group_id)
                .map(|group| group.remaining),
            Some(1)
        );
        handler.on_response(response(62, None));
        assert!(rx.await.expect("ack sender remains live").is_ok());
    }

    #[tokio::test]
    async fn ack_target_scoped_failure_keeps_concurrent_group_isolated() {
        let (mut handler, _close) = test_handler().await;
        let main = session("main");
        let target_a = target_id("a");
        let target_b = target_id("b");
        let (group_a, rx_a) = add_ack_group(&mut handler, target_a.clone(), main.clone(), 1);
        let (group_b, rx_b) = add_ack_group(&mut handler, target_b, main.clone(), 1);
        add_ack_member(&mut handler, 70, group_a, main.clone());
        add_ack_member(&mut handler, 71, group_b, main);

        handler.fail_ack_groups_for_target(&target_a);

        assert!(!handler.pending_ack_groups.contains_key(&group_a));
        assert!(handler.pending_ack_groups.contains_key(&group_b));
        assert!(matches!(
            rx_a.await.expect("ack sender remains live"),
            Err(CdpError::NoResponse)
        ));
        handler.on_response(response(71, None));
        assert!(rx_b.await.expect("ack sender remains live").is_ok());
    }

    #[tokio::test]
    async fn draining_child_settles_pending_command_and_navigation() {
        let (mut handler, _close) = test_handler().await;
        let main = session("main");
        let child = session("child");
        let mut target = Target::new(
            target_info("target"),
            TargetConfig::default(),
            BrowserContext::default(),
        );
        target.set_session_id(main);
        let (command_tx, command_rx) = oneshot::channel();
        handler.pending_commands.insert(
            CallId::new(80),
            (
                PendingRequest::ExternalCommand {
                    session_id: Some(child.clone()),
                    tx: command_tx,
                },
                "Runtime.evaluate".into(),
                Instant::now(),
            ),
        );
        let navigation_id = NavigationId(80);
        let (navigation_tx, navigation_rx) = oneshot::channel();
        handler.navigations.insert(
            navigation_id,
            NavigationRequest::Navigate(NavigationInProgress::new(
                target_id("target"),
                child.clone(),
                navigation_tx,
            )),
        );
        handler.pending_commands.insert(
            CallId::new(81),
            (
                PendingRequest::Navigate(navigation_id),
                "Page.navigate".into(),
                Instant::now(),
            ),
        );

        handler.fail_pending_for_session(&mut target, &child);

        assert!(matches!(
            command_rx.await.expect("command sender remains live"),
            Err(CdpError::FrameNotReady)
        ));
        assert!(matches!(
            navigation_rx.await.expect("navigation sender remains live"),
            Err(CdpError::FrameNotReady)
        ));
        assert!(handler.pending_commands.is_empty());
        assert!(handler.navigations.is_empty());
    }

    #[tokio::test]
    async fn draining_navigation_gate_blocks_only_session_scoped_requests() {
        let (mut handler, _close) = test_handler().await;
        let main = session("main");
        let child = session("child");
        let target_id = target_id("target");
        let mut target = Target::new(
            target_info("target"),
            TargetConfig::default(),
            BrowserContext::default(),
        );
        target.set_session_id(main.clone());
        target.enter_draining_session(child.clone());

        let blocked_id = NavigationId(82);
        let (blocked_tx, blocked_rx) = oneshot::channel();
        handler.navigations.insert(
            blocked_id,
            NavigationRequest::Navigate(NavigationInProgress::new(
                target_id.clone(),
                child.clone(),
                blocked_tx,
            )),
        );
        handler.submit_navigation_for_target(
            &target,
            blocked_id,
            CdpRequest::with_session(
                "Page.navigate".into(),
                json!({ "url": "https://blocked.example/" }),
                child.as_ref(),
            ),
            Instant::now(),
        );

        assert!(matches!(
            blocked_rx
                .await
                .expect("blocked navigation sender resolves"),
            Err(CdpError::FrameNotReady)
        ));
        assert!(!handler.navigations.contains_key(&blocked_id));
        assert!(handler.pending_commands.is_empty());

        let sessionless_id = NavigationId(83);
        let (sessionless_tx, mut sessionless_rx) = oneshot::channel();
        handler.navigations.insert(
            sessionless_id,
            NavigationRequest::Navigate(NavigationInProgress::new(target_id, main, sessionless_tx)),
        );
        handler.submit_navigation_for_target(
            &target,
            sessionless_id,
            CdpRequest::new(
                "Page.navigate".into(),
                json!({ "url": "https://main.example/" }),
            ),
            Instant::now(),
        );

        assert_eq!(handler.pending_commands.len(), 1);
        assert!(matches!(sessionless_rx.try_recv(), Ok(None)));
        handler.fail_all_pending_on_connection_close();
        assert!(matches!(
            sessionless_rx
                .await
                .expect("submitted navigation sender resolves during cleanup"),
            Err(CdpError::NoResponse)
        ));
    }

    #[tokio::test]
    async fn target_teardown_settles_navigation_before_response() {
        let (mut handler, _close) = test_handler().await;
        let (target_id, main_session) = install_target(&mut handler, "target");
        let navigation_id = NavigationId(90);
        let (tx, rx) = oneshot::channel();
        handler.navigations.insert(
            navigation_id,
            NavigationRequest::Navigate(NavigationInProgress::new(
                target_id.clone(),
                main_session,
                tx,
            )),
        );
        handler.pending_commands.insert(
            CallId::new(90),
            (
                PendingRequest::Navigate(navigation_id),
                "Page.navigate".into(),
                Instant::now(),
            ),
        );

        handler.on_target_destroyed(EventTargetDestroyed {
            target_id: target_id.clone(),
        });

        assert!(matches!(
            rx.await.expect("navigation sender remains live"),
            Err(CdpError::NoResponse)
        ));
        assert!(!handler.targets.contains_key(&target_id));
        assert!(handler.pending_commands.is_empty());
        assert!(handler.navigations.is_empty());
        assert!(handler.sessions.is_empty());
        handler.on_response(response(90, None));
    }

    #[tokio::test]
    async fn target_teardown_settles_navigation_after_response_before_lifecycle() {
        let (mut handler, _close) = test_handler().await;
        let (target_id, main_session) = install_target(&mut handler, "target");
        let navigation_id = NavigationId(91);
        let (tx, rx) = oneshot::channel();
        handler.navigations.insert(
            navigation_id,
            NavigationRequest::Navigate(NavigationInProgress::new(
                target_id.clone(),
                main_session,
                tx,
            )),
        );
        handler.pending_commands.insert(
            CallId::new(91),
            (
                PendingRequest::Navigate(navigation_id),
                "Page.navigate".into(),
                Instant::now(),
            ),
        );
        handler.on_response(navigation_response(91, None));
        assert!(handler.navigations.contains_key(&navigation_id));

        handler.on_target_destroyed(EventTargetDestroyed {
            target_id: target_id.clone(),
        });

        assert!(matches!(
            rx.await.expect("navigation sender remains live"),
            Err(CdpError::NoResponse)
        ));
        assert!(handler.navigations.is_empty());
    }

    #[tokio::test]
    async fn target_destroy_settles_registered_wait_for_navigation() {
        let (mut handler, _close) = test_handler().await;
        let (target_id, main_session) = install_target(&mut handler, "target");
        let navigation_id = NavigationId(120);
        let wait_rx = install_frame_navigation_watcher(
            &mut handler,
            &target_id,
            &main_session,
            navigation_id,
        );
        let (page_wait, initiator_rx) = install_page_teardown_waiters(&mut handler, &target_id);

        handler.on_target_destroyed(EventTargetDestroyed {
            target_id: target_id.clone(),
        });

        // The FrameManager-registered waiter must observe a typed error rather
        // than an opaque channel cancellation (which is what dropping the
        // FrameManager alone would produce).
        assert!(matches!(
            wait_rx
                .await
                .expect("wait sender is settled, not cancelled"),
            Err(crate::handler::frame::FrameWaitError::FrameSwappedOrDetached)
        ));
        assert!(matches!(page_wait.await, Err(CdpError::FrameNotReady)));
        assert!(matches!(
            initiator_rx.await.expect("initiator sender is settled"),
            Err(CdpError::NoResponse)
        ));
        assert!(!handler.targets.contains_key(&target_id));
    }

    #[tokio::test]
    async fn main_session_detach_settles_registered_wait_for_navigation() {
        let (mut handler, _close) = test_handler().await;
        let (target_id, main_session) = install_target(&mut handler, "target");
        let navigation_id = NavigationId(121);
        let wait_rx = install_frame_navigation_watcher(
            &mut handler,
            &target_id,
            &main_session,
            navigation_id,
        );
        let (page_wait, initiator_rx) = install_page_teardown_waiters(&mut handler, &target_id);

        handler.on_detached_from_target(EventDetachedFromTarget {
            session_id: main_session,
        });

        assert!(matches!(
            wait_rx
                .await
                .expect("wait sender is settled, not cancelled"),
            Err(crate::handler::frame::FrameWaitError::FrameSwappedOrDetached)
        ));
        assert!(matches!(page_wait.await, Err(CdpError::FrameNotReady)));
        assert!(matches!(
            initiator_rx.await.expect("initiator sender is settled"),
            Err(CdpError::NoResponse)
        ));
        assert!(!handler.targets.contains_key(&target_id));
    }

    #[tokio::test]
    async fn connection_close_settles_registered_wait_for_navigation() {
        let (mut handler, close) = test_handler().await;
        let (target_id, main_session) = install_target(&mut handler, "target");
        let navigation_id = NavigationId(122);
        let wait_rx = install_frame_navigation_watcher(
            &mut handler,
            &target_id,
            &main_session,
            navigation_id,
        );
        let (page_wait, initiator_rx) = install_page_teardown_waiters(&mut handler, &target_id);

        close.send(()).expect("close test websocket");
        let next = tokio::time::timeout(Duration::from_secs(2), handler.next())
            .await
            .expect("handler observes websocket EOF");
        assert!(next.is_none());

        // On connection close the Handler stops polling, so without explicit
        // settlement this waiter would hang forever. It must resolve to a typed
        // error instead.
        assert!(matches!(
            wait_rx
                .await
                .expect("wait sender is settled, not cancelled"),
            Err(crate::handler::frame::FrameWaitError::FrameSwappedOrDetached)
        ));
        assert!(matches!(page_wait.await, Err(CdpError::FrameNotReady)));
        assert!(matches!(
            initiator_rx.await.expect("initiator sender is settled"),
            Err(CdpError::NoResponse)
        ));
    }

    #[tokio::test]
    async fn child_session_detach_keeps_the_target_initiator_alive() {
        let (mut handler, _close) = test_handler().await;
        let (target_id, _) = install_target(&mut handler, "target");
        let child_session = session("target-child");
        handler.sessions.insert(
            child_session.clone(),
            Session::new(child_session.clone(), target_id.clone()),
        );
        let (initiator_tx, mut initiator_rx) = oneshot::channel();
        handler
            .targets
            .get_mut(&target_id)
            .expect("target remains registered")
            .set_initiator(initiator_tx);

        let logs = Arc::new(Mutex::new(Vec::new()));
        let writer_logs = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .with_writer(move || SharedLogWriter(writer_logs.clone()))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            handler.on_detached_from_target(EventDetachedFromTarget {
                session_id: child_session,
            });
        });

        assert!(matches!(initiator_rx.try_recv(), Ok(None)));
        assert!(handler.targets.contains_key(&target_id));
        let logs = String::from_utf8(logs.lock().expect("test log buffer lock").clone())
            .expect("warning output is utf-8");
        assert!(logs.contains("auto-attach envelope invariant did not hold"));

        handler.on_target_destroyed(EventTargetDestroyed { target_id });
        assert!(matches!(
            initiator_rx
                .await
                .expect("whole-target teardown settles initiator"),
            Err(CdpError::NoResponse)
        ));
    }

    #[tokio::test]
    async fn target_teardown_covers_live_child_session_commands() {
        let (mut handler, _close) = test_handler().await;
        let (target_id, _) = install_target(&mut handler, "target");
        let child = session("target-child");
        handler.sessions.insert(
            child.clone(),
            Session::new(child.clone(), target_id.clone()),
        );
        let (tx, rx) = oneshot::channel();
        handler.pending_commands.insert(
            CallId::new(92),
            (
                PendingRequest::ExternalCommand {
                    session_id: Some(child),
                    tx,
                },
                "Runtime.evaluate".into(),
                Instant::now(),
            ),
        );

        handler.on_target_destroyed(EventTargetDestroyed { target_id });

        assert!(matches!(
            rx.await.expect("command sender remains live"),
            Err(CdpError::NoResponse)
        ));
        assert!(handler.sessions.is_empty());
    }

    #[tokio::test]
    async fn ack_connection_eof_drains_group_and_browser_root_command() {
        let (mut handler, close) = test_handler().await;
        let (group_id, ack_rx) =
            add_ack_group(&mut handler, target_id("target"), session("main"), 1);
        add_ack_member(&mut handler, 100, group_id, session("main"));
        let (command_tx, command_rx) = oneshot::channel();
        handler.pending_commands.insert(
            CallId::new(101),
            (
                PendingRequest::ExternalCommand {
                    session_id: None,
                    tx: command_tx,
                },
                "Browser.getVersion".into(),
                Instant::now(),
            ),
        );

        close.send(()).expect("close test websocket");
        let next = tokio::time::timeout(Duration::from_secs(2), handler.next())
            .await
            .expect("handler observes websocket EOF");
        assert!(next.is_none());
        assert!(matches!(
            ack_rx.await.expect("ack sender remains live"),
            Err(CdpError::NoResponse)
        ));
        assert!(matches!(
            command_rx.await.expect("command sender remains live"),
            Err(CdpError::NoResponse)
        ));
    }

    #[tokio::test]
    async fn ack_connection_error_drains_groups_before_returning_error() {
        let (mut handler, _release) =
            test_handler_with_message(WsMessage::Binary(vec![1, 2, 3].into())).await;
        let (_group_id, ack_rx) =
            add_ack_group(&mut handler, target_id("target"), session("main"), 1);

        let next = tokio::time::timeout(Duration::from_secs(2), handler.next())
            .await
            .expect("handler observes websocket error");
        assert!(matches!(next, Some(Err(CdpError::UnexpectedWsMessage(_)))));
        assert!(matches!(
            ack_rx.await.expect("ack sender remains live"),
            Err(CdpError::NoResponse)
        ));
    }

    #[tokio::test]
    async fn ack_closing_completion_drains_other_groups() {
        let close_call_id = 120;
        let response = format!(r#"{{"id":{close_call_id},"result":{{}}}}"#);
        let (mut handler, _release) =
            test_handler_with_message(WsMessage::Text(response.into())).await;
        let (_group_id, ack_rx) =
            add_ack_group(&mut handler, target_id("target"), session("main"), 1);
        let (close_tx, close_rx) = oneshot::channel();
        handler.pending_commands.insert(
            CallId::new(close_call_id),
            (
                PendingRequest::CloseBrowser(close_tx),
                "Browser.close".into(),
                Instant::now(),
            ),
        );

        let next = tokio::time::timeout(Duration::from_secs(2), handler.next())
            .await
            .expect("handler observes closing response");
        assert!(next.is_none());
        assert!(close_rx.await.expect("close sender remains live").is_ok());
        assert!(matches!(
            ack_rx.await.expect("ack sender remains live"),
            Err(CdpError::NoResponse)
        ));
    }

    #[tokio::test]
    async fn ack_browser_root_response_remains_targetless_and_successful() {
        let (mut handler, _close) = test_handler().await;
        let (tx, rx) = oneshot::channel();
        handler.pending_commands.insert(
            CallId::new(110),
            (
                PendingRequest::ExternalCommand {
                    session_id: None,
                    tx,
                },
                "Browser.getVersion".into(),
                Instant::now(),
            ),
        );

        handler.on_response(response(110, None));

        assert!(rx.await.expect("root sender remains live").is_ok());
    }
}
