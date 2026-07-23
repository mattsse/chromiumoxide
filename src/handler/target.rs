use std::collections::{HashMap, HashSet, VecDeque};
use std::mem;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use chromiumoxide_cdp::cdp::browser_protocol::target::DetachFromTargetParams;
use futures::channel::mpsc::{Receiver, TryRecvError, UnboundedSender};
use futures::channel::oneshot::Sender;
use futures::stream::Stream;
use futures::task::{Context, Poll};

use chromiumoxide_cdp::cdp::CdpEventMessage;
use chromiumoxide_cdp::cdp::browser_protocol::fetch::{
    self, ContinueRequestParams, RequestPattern,
};
#[allow(deprecated)]
use chromiumoxide_cdp::cdp::browser_protocol::network::{
    EmulateNetworkConditionsParams, Headers, SetCacheDisabledParams, SetExtraHttpHeadersParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::page::{
    AddScriptToEvaluateOnNewDocumentParams, FrameId, GetFrameTreeParams, ScriptIdentifier,
};
use chromiumoxide_cdp::cdp::browser_protocol::{
    browser::BrowserContextId,
    log as cdplog, network as cdp_network, performance,
    security::SetIgnoreCertificateErrorsParams,
    target::{
        AttachToTargetParams, EventDetachedFromTarget, SessionId, SetAutoAttachParams, TargetId,
        TargetInfo,
    },
};
use chromiumoxide_cdp::cdp::events::CdpEvent;
use chromiumoxide_types::{Command, Method, MethodId, Request, Response};

use crate::auth::Credentials;
use crate::cdp::browser_protocol::target::CloseTargetParams;
use crate::cmd::CommandChain;
use crate::cmd::CommandMessage;
use crate::error::{CdpError, Result};
use crate::handler::browser::BrowserContext;
use crate::handler::domworld::DOMWorldKind;
use crate::handler::emulation::EmulationManager;
use crate::handler::frame::{
    FrameEvent, FrameManager, FrameWaitError, NavigationError, NavigationId, NavigationOk,
    PreloadId,
};
use crate::handler::frame::{FrameNavigationRequest, UTILITY_WORLD_NAME};
use crate::handler::network::{NetworkCommand, NetworkEvent, NetworkManager, PauseDisposition};
use crate::handler::page::PageHandle;
use crate::handler::viewport::Viewport;
use crate::handler::{PageInner, REQUEST_TIMEOUT};
use crate::listeners::{EventListenerRequest, EventListeners};
use crate::{
    ArcHttpRequest,
    page::{Page, PausedRequest},
};
use chromiumoxide_cdp::cdp::js_protocol::runtime::{
    ExecutionContextId, RunIfWaitingForDebuggerParams,
};
use std::time::Duration;

const PAGE_MESSAGE_POLL_BUDGET: usize = 32;

fn drain_closed_receiver<T>(receiver: &mut Receiver<T>, mut settle: impl FnMut(T)) {
    loop {
        match receiver.try_recv() {
            Ok(message) => settle(message),
            Err(TryRecvError::Empty) => {
                // SinkExt::send performs the state increment and queue push in
                // one synchronous poll, so a single-thread executor cannot
                // interleave teardown here. A producer on another OS thread can
                // be preempted inside that short critical section, with no hard
                // wall-clock bound before it resumes. Receiver::drop relies on
                // the same yield-and-drain behavior; teardown accepts that
                // dependency-level risk so every message accepted before close
                // is drained and settled. Sends starting after close fail at
                // the channel boundary instead of entering this receiver.
                std::thread::yield_now();
            }
            Err(TryRecvError::Closed) => break,
        }
    }
}

fn poll_receiver_batch<T, S>(receiver: &mut S, cx: &mut Context<'_>) -> (Vec<T>, bool)
where
    S: Stream<Item = T> + Unpin,
{
    let mut messages = Vec::new();
    for _ in 0..PAGE_MESSAGE_POLL_BUDGET {
        match Pin::new(&mut *receiver).poll_next(cx) {
            Poll::Ready(Some(message)) => messages.push(message),
            Poll::Ready(None) | Poll::Pending => break,
        }
    }

    let budget_exhausted = messages.len() == PAGE_MESSAGE_POLL_BUDGET;
    if budget_exhausted {
        // The receiver may still be ready. Schedule another poll instead of
        // letting a hot page monopolize the handler task.
        cx.waker().wake_by_ref();
    }
    (messages, budget_exhausted)
}

macro_rules! advance_state {
    ($s:ident, $cx:ident, $now:ident, $cmds: ident, $next_state:expr ) => {{
        if let Poll::Ready(poll) = $cmds.poll($now) {
            return match poll {
                None => {
                    $s.init_state = $next_state;
                    $s.poll($cx, $now)
                }
                Some(Ok((method, params))) => Some(TargetEvent::Request(Request {
                    method,
                    session_id: $s.session_id.clone().map(Into::into),
                    params,
                })),
                Some(Err(_)) => Some($s.on_initialization_failed()),
            };
        } else {
            return None;
        }
    }};
}

#[derive(Debug)]
pub struct Target {
    /// Info about this target as returned from the chromium instance
    info: TargetInfo,
    /// The type of this target
    r#type: TargetType,
    /// Configs for this target
    config: TargetConfig,
    /// The context this target is running in
    browser_context: BrowserContext,
    /// The frame manager that maintains the state of all frames and handles
    /// navigations of frames
    frame_manager: FrameManager,
    /// Handles all the https
    network_manager: NetworkManager,
    emulation_manager: EmulationManager,
    /// The identifier of the session this target is attached to
    session_id: Option<SessionId>,
    /// The handle of the browser page of this target
    page: Option<PageHandle>,
    /// Drives this target towards initialization
    init_state: TargetInit,
    /// Initialization state for every paused OOP child session.
    iframe_init_states: HashMap<SessionId, IframeInitState>,
    /// Main-target initialization reuses `InitializingFrame` for the utility
    /// script command, so this prevents an explicit failure from turning the
    /// next state poll into an accidental retry loop.
    main_isolated_world_attempted: bool,
    /// Currently queued events to report to the `Handler`
    queued_events: VecDeque<TargetEvent>,
    /// Sessions that are no longer allowed to produce ordinary work. The
    /// tombstone remains until the handler removes the session route, closing
    /// the window where late events could otherwise re-enter this target.
    draining_sessions: HashSet<SessionId>,
    /// All registered event subscriptions
    event_listeners: EventListeners,
    /// At most one live consumer owns managed paused-request responses. A
    /// closed consumer is replaceable on the next registration attempt.
    paused_request_sink: Option<PausedRequestSink>,
    /// Whether a Page clone has been successfully handed to user code. Before
    /// that point a main-session pause must not wait for an unavailable
    /// responder during target bootstrap.
    page_exposed: bool,
    /// Senders that need to be notified once the main frame has loaded
    wait_for_frame_navigation: Vec<Sender<ArcHttpRequest>>,
    /// Page-facing navigation waiters retain their typed error channel after
    /// they have been promoted out of the page message receiver.
    wait_for_navigation_results: Vec<Sender<Result<ArcHttpRequest>>>,
    /// The sender who requested the page.
    initiator: Option<Sender<Result<Page>>>,
}

impl Target {
    /// Create a new target instance with `TargetInfo` after a
    /// `CreateTargetParams` request.
    pub fn new(info: TargetInfo, config: TargetConfig, browser_context: BrowserContext) -> Self {
        let ty = TargetType::new(&info.r#type);
        let request_timeout = config.request_timeout;
        let mut network_manager = NetworkManager::new(config.ignore_https_errors, request_timeout);

        network_manager.set_cache_enabled(config.cache_enabled);
        network_manager.set_request_interception(config.request_intercept);

        Self {
            info,
            r#type: ty,
            config,
            frame_manager: FrameManager::new(request_timeout),
            network_manager,
            emulation_manager: EmulationManager::new(request_timeout),
            session_id: None,
            page: None,
            init_state: TargetInit::AttachToTarget,
            iframe_init_states: Default::default(),
            main_isolated_world_attempted: false,
            wait_for_frame_navigation: Default::default(),
            wait_for_navigation_results: Default::default(),
            queued_events: Default::default(),
            draining_sessions: Default::default(),
            event_listeners: Default::default(),
            paused_request_sink: None,
            page_exposed: false,
            initiator: None,
            browser_context,
        }
    }

    pub fn set_session_id(&mut self, id: SessionId) {
        self.frame_manager.set_main_session_id(id.clone());
        self.session_id = Some(id)
    }

    pub fn session_id(&self) -> Option<&SessionId> {
        self.session_id.as_ref()
    }

    pub fn browser_context(&self) -> &BrowserContext {
        &self.browser_context
    }

    pub fn session_id_mut(&mut self) -> &mut Option<SessionId> {
        &mut self.session_id
    }

    /// The identifier for this target
    pub fn target_id(&self) -> &TargetId {
        &self.info.target_id
    }

    /// The type of this target
    pub fn r#type(&self) -> &TargetType {
        &self.r#type
    }

    /// Whether this target is already initialized
    pub fn is_initialized(&self) -> bool {
        matches!(self.init_state, TargetInit::Initialized)
    }

    /// Navigate a frame
    pub fn goto(&mut self, req: FrameNavigationRequest) {
        self.frame_manager.goto(req)
    }

    pub(crate) fn goto_in_session(&mut self, session_id: SessionId, req: FrameNavigationRequest) {
        let Some(frame_id) = self
            .frame_manager
            .main_frame()
            .map(|frame| frame.id().clone())
        else {
            return;
        };
        self.frame_manager
            .navigate_frame_in_session(session_id, frame_id, req);
    }

    pub(crate) fn goto_frame(
        &mut self,
        session_id: SessionId,
        frame_id: FrameId,
        req: FrameNavigationRequest,
    ) {
        self.frame_manager
            .navigate_frame_in_session(session_id, frame_id, req);
    }

    pub(crate) fn on_navigation_failed(&mut self, navigation_id: NavigationId, error_text: String) {
        self.frame_manager
            .fail_navigation_by_nav_id(navigation_id, error_text);
    }

    /// Whether commands may still be submitted to this session. A child must
    /// be registered, fully initialized, and outside the draining window.
    pub(crate) fn frame_session_ready(&self, session_id: &SessionId) -> bool {
        if self.is_session_draining(session_id) {
            return false;
        }
        if self.session_id() == Some(session_id) {
            return true;
        }
        if !self.frame_manager.is_child_session(session_id) {
            return false;
        }
        self.iframe_init_states
            .get(session_id)
            .is_none_or(|state| matches!(state, IframeInitState::Done))
    }

    /// Pin a frame operation to both frame identity and the session captured by
    /// its caller. A live main session alone is insufficient after a swap.
    pub(crate) fn frame_ready(&self, frame_id: &FrameId, session_id: &SessionId) -> bool {
        self.frame_session_ready(session_id)
            && self
                .frame_manager
                .frame(frame_id)
                .is_some_and(|frame| frame.session_id() == Some(session_id))
    }

    fn build_frame_info(&self, frame: &crate::handler::frame::Frame) -> Result<FrameInfo> {
        Ok(FrameInfo {
            frame_id: frame.id().clone(),
            session_id: frame.require_session_id()?.clone(),
            main_session_id: self
                .frame_manager
                .main_session_id()
                .ok_or(CdpError::FrameNotReady)?
                .clone(),
            parent_id: frame.parent_id().cloned(),
            url: frame.url().map(str::to_owned),
            security_origin: frame.security_origin().to_owned(),
        })
    }

    /// Build the immutable cross-session topology snapshot used by Element
    /// geometry. Same-session ancestors are intentionally omitted because CDP
    /// DOM quads are already relative to that session's root frame.
    fn build_boundary_chain(
        &self,
        leaf: &FrameId,
        expected_session_id: &SessionId,
    ) -> Result<Vec<FrameBoundary>> {
        let leaf_frame = self
            .frame_manager
            .frame(leaf)
            .ok_or_else(|| CdpError::FrameNotFound(leaf.clone()))?;
        if leaf_frame.session_id() != Some(expected_session_id)
            || !self.frame_session_ready(expected_session_id)
        {
            return Err(CdpError::FrameNotReady);
        }

        let main_frame_id = self
            .frame_manager
            .main_frame()
            .map(|frame| frame.id().clone())
            .ok_or(CdpError::FrameNotReady)?;
        let mut chain = Vec::new();
        let mut visited = HashSet::new();
        let mut current_id = leaf.clone();

        loop {
            let current = self
                .frame_manager
                .frame(&current_id)
                .ok_or(CdpError::FrameNotReady)?;
            let Some(parent_id) = current.parent_id().cloned() else {
                if current_id == main_frame_id {
                    break;
                }
                return Err(CdpError::FrameNotReady);
            };
            let current_session_id = current.require_session_id()?.clone();
            if !visited.insert(current_id.clone()) {
                return Err(CdpError::FrameNotReady);
            }

            let parent = self
                .frame_manager
                .frame(&parent_id)
                .ok_or(CdpError::FrameNotReady)?;
            let parent_session_id = parent.require_session_id()?;
            if !self.frame_session_ready(parent_session_id) {
                return Err(CdpError::FrameNotReady);
            }
            if parent_session_id != &current_session_id {
                chain.push(FrameBoundary {
                    child_frame_id: current_id.clone(),
                    child_session_id: current_session_id,
                    parent_frame_id: parent_id.clone(),
                    parent_session_id: parent_session_id.clone(),
                });
            }
            current_id = parent_id;
        }

        Ok(chain)
    }

    fn request_for_session<T: Command>(command: T, session_id: &SessionId) -> Request {
        Request {
            method: command.identifier(),
            session_id: Some(session_id.as_ref().to_owned()),
            params: serde_json::to_value(command).expect("Command should not panic"),
        }
    }

    fn network_requests_for_session(
        commands: &[NetworkCommand],
        session_id: &SessionId,
    ) -> Vec<Request> {
        commands
            .iter()
            .map(|(method, params)| Request {
                method: method.clone(),
                session_id: Some(session_id.as_ref().to_owned()),
                params: params.clone(),
            })
            .collect()
    }

    fn preload_request_for_session(
        params: AddScriptToEvaluateOnNewDocumentParams,
        preload_key: PreloadId,
        session_id: &SessionId,
    ) -> TargetEvent {
        TargetEvent::QueuePreloadScript {
            request: Self::request_for_session(params, session_id),
            preload_key,
        }
    }

    fn already_snapshotted_child_sessions(&self) -> Vec<SessionId> {
        let mut sessions = self
            .frame_manager
            .child_sessions()
            .filter(|session_id| !self.is_session_draining(session_id))
            .filter(|session_id| {
                matches!(
                    self.iframe_init_states.get(*session_id),
                    None | Some(IframeInitState::PostChainPreload)
                        | Some(IframeInitState::PostChainNetwork)
                        | Some(IframeInitState::PostChainUnpause)
                        | Some(IframeInitState::Done)
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| left.as_ref().cmp(right.as_ref()));
        sessions
    }

    /// Enqueue an already-tracked script for every child that has crossed its
    /// init snapshot boundary. Chaining children will see it in their later
    /// snapshot instead, which keeps each script single-sourced per session.
    pub(crate) fn enqueue_preload_fan_out(&mut self, preload_key: PreloadId) {
        let Some((_, params)) = self
            .frame_manager
            .preload_snapshot()
            .into_iter()
            .find(|(id, _)| *id == preload_key)
        else {
            return;
        };
        for session_id in self.already_snapshotted_child_sessions() {
            self.queued_events
                .push_back(Self::preload_request_for_session(
                    params.clone(),
                    preload_key,
                    &session_id,
                ));
        }
    }

    /// Queues one ordered dynamic-state update. Fully initialized sessions
    /// participate in the response acknowledgement; sessions already past
    /// their network replay receive a best-effort latest-state update without
    /// delaying the caller.
    fn enqueue_network_fan_out(
        &mut self,
        commands: Vec<NetworkCommand>,
        ack_tx: Sender<Result<()>>,
    ) {
        let Some(main_session_id) = self.frame_manager.main_session_id().cloned() else {
            let _ = ack_tx.send(Err(CdpError::FrameNotReady));
            return;
        };

        let mut child_sessions = self
            .frame_manager
            .child_sessions()
            .filter(|session_id| !self.is_session_draining(session_id))
            .cloned()
            .collect::<Vec<_>>();
        child_sessions.sort_by(|left, right| left.as_ref().cmp(right.as_ref()));

        let mut ack_reqs = Self::network_requests_for_session(&commands, &main_session_id);
        let mut send_only_reqs = Vec::new();
        for session_id in child_sessions {
            match self.iframe_init_states.get(&session_id) {
                None | Some(IframeInitState::Done) => {
                    ack_reqs.extend(Self::network_requests_for_session(&commands, &session_id))
                }
                Some(IframeInitState::PostChainNetwork)
                | Some(IframeInitState::PostChainUnpause) => send_only_reqs
                    .extend(Self::network_requests_for_session(&commands, &session_id)),
                Some(IframeInitState::Chaining { .. })
                | Some(IframeInitState::PostChainPreload)
                | Some(IframeInitState::Failed) => {}
            }
        }

        self.queued_events
            .push_back(TargetEvent::FanOutAckBatch(FanOutAckBatch {
                ack_reqs,
                send_only_reqs,
                ack_tx,
                main_session_id,
            }));
    }

    fn start_iframe_init(&mut self, session_id: SessionId) {
        if self.is_session_draining(&session_id)
            || self.iframe_init_states.contains_key(&session_id)
        {
            return;
        }
        let chain = self.build_iframe_phase_chain(&session_id, InitPhase::Frame);
        self.iframe_init_states.insert(
            session_id,
            IframeInitState::Chaining {
                chain,
                phase: InitPhase::Frame,
            },
        );
    }

    fn build_iframe_phase_chain(
        &mut self,
        session_id: &SessionId,
        phase: InitPhase,
    ) -> CommandChain {
        match phase {
            InitPhase::Frame => FrameManager::init_commands(self.config.request_timeout),
            InitPhase::IsolatedWorld => self
                .frame_manager
                .ensure_isolated_world_on_next_document_in_session(
                    UTILITY_WORLD_NAME,
                    session_id.clone(),
                )
                // `None` means the utility world is already registered for this
                // session (I-007: a paused OOP child gets the named-world script
                // before unpause and no explicit createIsolatedWorld). Nothing to
                // submit, so advance the init chain with an empty command chain.
                .unwrap_or_else(|| {
                    CommandChain::new(
                        Vec::<(MethodId, serde_json::Value)>::new(),
                        self.config.request_timeout,
                    )
                }),
            InitPhase::AutoAttach => {
                let attach = SetAutoAttachParams::builder()
                    .flatten(true)
                    .auto_attach(true)
                    .wait_for_debugger_on_start(true)
                    .build()
                    .expect("auto-attach parameters are complete");
                CommandChain::new(
                    vec![(
                        attach.identifier(),
                        serde_json::to_value(attach).expect("Command should not panic"),
                    )],
                    self.config.request_timeout,
                )
            }
            InitPhase::Bindings => CommandChain::new(
                Vec::<(MethodId, serde_json::Value)>::new(),
                self.config.request_timeout,
            ),
        }
    }

    fn advance_to_next_iframe_phase(&mut self, session_id: SessionId, phase: InitPhase) {
        if self.is_session_draining(&session_id)
            || !matches!(
                self.iframe_init_states.get(&session_id),
                Some(IframeInitState::Chaining {
                    phase: current_phase,
                    ..
                }) if *current_phase == phase
            )
        {
            return;
        }

        if let Some(next_phase) = phase.next() {
            let chain = self.build_iframe_phase_chain(&session_id, next_phase);
            self.iframe_init_states.insert(
                session_id,
                IframeInitState::Chaining {
                    chain,
                    phase: next_phase,
                },
            );
        } else {
            self.finish_iframe_init_phases(session_id);
        }
    }

    fn finish_iframe_init_phases(&mut self, session_id: SessionId) {
        if matches!(
            self.iframe_init_states.get(&session_id),
            Some(IframeInitState::Chaining { .. })
        ) {
            for (preload_key, params) in self.frame_manager.preload_snapshot() {
                self.queued_events
                    .push_back(Self::preload_request_for_session(
                        params,
                        preload_key,
                        &session_id,
                    ));
            }
            self.iframe_init_states
                .insert(session_id, IframeInitState::PostChainPreload);
        }
    }

    fn transition_iframe_to_failed_and_unpause(&mut self, session_id: SessionId) {
        if !matches!(
            self.iframe_init_states.get(&session_id),
            Some(IframeInitState::Chaining { .. })
        ) {
            return;
        }
        self.iframe_init_states
            .insert(session_id.clone(), IframeInitState::Failed);
        self.queued_events
            .push_back(TargetEvent::Request(Self::request_for_session(
                RunIfWaitingForDebuggerParams::default(),
                &session_id,
            )));
    }

    fn iframe_network_sync_requests(&self, session_id: &SessionId) -> Vec<Request> {
        let mut requests = Vec::new();
        let mut push = |method: MethodId, params: serde_json::Value| {
            requests.push(Request {
                method,
                session_id: Some(session_id.as_ref().to_owned()),
                params,
            });
        };

        let enable = cdp_network::EnableParams::default();
        push(
            enable.identifier(),
            serde_json::to_value(enable).expect("Command should not panic"),
        );

        if self.network_manager.ignore_https_errors() {
            let ignore = SetIgnoreCertificateErrorsParams::new(true);
            push(
                ignore.identifier(),
                serde_json::to_value(ignore).expect("Command should not panic"),
            );
        }

        let cache = SetCacheDisabledParams::new(self.network_manager.is_cache_disabled());
        push(
            cache.identifier(),
            serde_json::to_value(cache).expect("Command should not panic"),
        );

        let headers = serde_json::to_value(self.network_manager.extra_headers().clone())
            .expect("headers should serialize");
        let headers = SetExtraHttpHeadersParams::new(Headers::new(headers));
        push(
            headers.identifier(),
            serde_json::to_value(headers).expect("Command should not panic"),
        );

        if self.network_manager.is_offline() {
            #[allow(deprecated)]
            let offline = EmulateNetworkConditionsParams::builder()
                .offline(true)
                .latency(0)
                .download_throughput(-1.)
                .upload_throughput(-1.)
                .build()
                .expect("offline parameters are complete");
            push(
                offline.identifier(),
                serde_json::to_value(offline).expect("Command should not panic"),
            );
        }

        if self.network_manager.is_request_interception_enabled()
            || self.network_manager.credentials().is_some()
        {
            let fetch = fetch::EnableParams::builder()
                .handle_auth_requests(true)
                .pattern(RequestPattern::builder().url_pattern("*").build())
                .build();
            push(
                fetch.identifier(),
                serde_json::to_value(fetch).expect("Command should not panic"),
            );
        }

        if let Some(viewport) = self.config.viewport.as_ref() {
            for (method, params) in EmulationManager::viewport_commands(viewport) {
                push(method, params);
            }
        }

        requests
    }

    fn poll_iframe_init(&mut self, now: Instant) -> bool {
        let session_ids = self.iframe_init_states.keys().cloned().collect::<Vec<_>>();
        for session_id in session_ids {
            let action = match self.iframe_init_states.get_mut(&session_id) {
                Some(IframeInitState::Chaining { chain, phase }) => match chain.poll(now) {
                    Poll::Pending => None,
                    Poll::Ready(Some(Ok((method, params)))) => {
                        Some(IframeInitAction::Send { method, params })
                    }
                    Poll::Ready(Some(Err(_))) => Some(IframeInitAction::Timeout),
                    Poll::Ready(None) => Some(IframeInitAction::PhaseComplete(*phase)),
                },
                Some(IframeInitState::PostChainPreload) => Some(IframeInitAction::EnqueueNetwork),
                Some(IframeInitState::PostChainNetwork) => Some(IframeInitAction::EnqueueUnpause),
                Some(IframeInitState::PostChainUnpause) => Some(IframeInitAction::MarkDone),
                Some(IframeInitState::Done) => Some(IframeInitAction::RemoveDone),
                Some(IframeInitState::Failed) | None => None,
            };

            let Some(action) = action else {
                continue;
            };
            match action {
                IframeInitAction::Send { method, params } => {
                    self.queued_events.push_back(TargetEvent::Request(Request {
                        method,
                        session_id: Some(session_id.as_ref().to_owned()),
                        params,
                    }));
                }
                IframeInitAction::Timeout => {
                    self.transition_iframe_to_failed_and_unpause(session_id);
                }
                IframeInitAction::PhaseComplete(phase) => {
                    self.advance_to_next_iframe_phase(session_id, phase);
                }
                IframeInitAction::EnqueueNetwork => {
                    let requests = self.iframe_network_sync_requests(&session_id);
                    self.iframe_init_states
                        .insert(session_id, IframeInitState::PostChainNetwork);
                    self.queued_events
                        .extend(requests.into_iter().map(TargetEvent::Request));
                }
                IframeInitAction::EnqueueUnpause => {
                    self.iframe_init_states
                        .insert(session_id.clone(), IframeInitState::PostChainUnpause);
                    self.queued_events
                        .push_back(TargetEvent::Request(Self::request_for_session(
                            RunIfWaitingForDebuggerParams::default(),
                            &session_id,
                        )));
                }
                IframeInitAction::MarkDone => {
                    self.iframe_init_states
                        .insert(session_id, IframeInitState::Done);
                }
                IframeInitAction::RemoveDone => {
                    self.iframe_init_states.remove(&session_id);
                }
            }
            return true;
        }
        false
    }

    fn create_page(&mut self) {
        if self.page.is_none() {
            if let Some(session) = self.session_id.clone() {
                let handle =
                    PageHandle::new(self.target_id().clone(), session, self.opener_id().cloned());
                self.page = Some(handle);
            }
        }
    }

    /// Tries to create the `PageInner` if this target is already initialized
    pub(crate) fn get_or_create_page(&mut self) -> Option<&Arc<PageInner>> {
        self.create_page();
        self.page.as_ref().map(|p| p.inner())
    }

    /// Records Page exposure only after the receiving user-visible channel
    /// accepted the clone. Creating an internal PageHandle is not exposure.
    pub(crate) fn mark_page_exposed_after_successful_send(&mut self, delivered: bool) {
        if delivered {
            self.page_exposed = true;
        }
    }

    #[cfg(test)]
    pub(crate) fn page_exposed(&self) -> bool {
        self.page_exposed
    }

    pub fn is_page(&self) -> bool {
        self.r#type().is_page()
    }

    pub fn browser_context_id(&self) -> Option<&BrowserContextId> {
        self.info.browser_context_id.as_ref()
    }

    pub fn info(&self) -> &TargetInfo {
        &self.info
    }

    /// Get the target that opened this target. Top-level targets return `None`.
    pub fn opener_id(&self) -> Option<&TargetId> {
        self.info.opener_id.as_ref()
    }

    pub fn frame_manager(&self) -> &FrameManager {
        &self.frame_manager
    }

    pub fn frame_manager_mut(&mut self) -> &mut FrameManager {
        &mut self.frame_manager
    }

    #[cfg(test)]
    pub(crate) fn queued_events(&self) -> &VecDeque<TargetEvent> {
        &self.queued_events
    }

    /// Mark a child session as draining and remove work that has not reached
    /// the connection yet.
    ///
    /// Inserting the tombstone first makes repeated swap/detach notifications
    /// idempotent and ensures queue cleanup runs at most once.
    pub(crate) fn enter_draining_session(&mut self, session_id: SessionId) {
        if !self.draining_sessions.insert(session_id.clone()) {
            return;
        }

        self.iframe_init_states.remove(&session_id);
        self.network_manager.on_session_draining(&session_id);
        self.frame_manager
            .fail_navigation_state_for_session(&session_id, FrameWaitError::FrameSwappedOrDetached);
        self.rebuild_queued_events_for_session(&session_id);
        self.queued_events
            .push_back(TargetEvent::SessionDraining(session_id));
    }

    pub(crate) fn is_session_draining(&self, session_id: &SessionId) -> bool {
        self.draining_sessions.contains(session_id)
    }

    /// Clear a draining tombstone only after the handler has removed the
    /// session-to-target route.
    pub(crate) fn clear_draining(&mut self, session_id: &SessionId) {
        self.draining_sessions.remove(session_id);
    }

    /// Fail queued work for one dead child session while preserving every
    /// other event's relative position.
    pub(crate) fn fail_queued_events(&mut self, session_id: &SessionId) {
        self.rebuild_queued_events_for_session(session_id);
    }

    /// Remove one session from a queued fan-out operation without changing
    /// the batch's position in the surrounding target queue.
    fn filter_fan_out_batch(
        &mut self,
        mut batch: FanOutAckBatch,
        session_id: &SessionId,
    ) -> Option<FanOutAckBatch> {
        // The main-session check is identity-based. Inferring it from an empty
        // request bag could incorrectly report success while the page itself
        // is being torn down.
        if session_id == &batch.main_session_id {
            let _ = batch.ack_tx.send(Err(CdpError::FrameNotReady));
            return None;
        }

        batch
            .ack_reqs
            .retain(|request| request.session_id.as_deref() != Some(session_id.as_ref()));
        batch
            .send_only_reqs
            .retain(|request| request.session_id.as_deref() != Some(session_id.as_ref()));
        Some(batch)
    }

    /// Rebuild the queue in two ownership phases so sender resolution cannot
    /// conflict with a borrow of `self.queued_events`.
    fn rebuild_queued_events_for_session(&mut self, session_id: &SessionId) {
        let queued_events = mem::take(&mut self.queued_events);
        let mut surviving_events = VecDeque::with_capacity(queued_events.len());

        for event in queued_events {
            match event {
                TargetEvent::Request(request) => {
                    if request.session_id.as_deref() != Some(session_id.as_ref()) {
                        surviving_events.push_back(TargetEvent::Request(request));
                    }
                }
                TargetEvent::NavigationRequest(id, request) => {
                    if request.session_id.as_deref() != Some(session_id.as_ref()) {
                        surviving_events.push_back(TargetEvent::NavigationRequest(id, request));
                    }
                }
                TargetEvent::NavigationResult(result) => {
                    surviving_events.push_back(TargetEvent::NavigationResult(result));
                }
                TargetEvent::Command(command) => {
                    if command.session_id.as_ref() == Some(session_id) {
                        let _ = command.sender.send(Err(CdpError::FrameNotReady));
                    } else {
                        surviving_events.push_back(TargetEvent::Command(command));
                    }
                }
                TargetEvent::RegisterChildSession(child_session_id) => {
                    surviving_events.push_back(TargetEvent::RegisterChildSession(child_session_id))
                }
                TargetEvent::UnregisterChildSession(child_session_id) => surviving_events
                    .push_back(TargetEvent::UnregisterChildSession(child_session_id)),
                TargetEvent::SessionDraining(child_session_id) => {
                    surviving_events.push_back(TargetEvent::SessionDraining(child_session_id))
                }
                TargetEvent::FrameNavigate {
                    session_id: event_session_id,
                    frame_id,
                    req,
                    tx,
                } => {
                    if event_session_id == *session_id {
                        let _ = tx.send(Err(CdpError::FrameNotReady));
                    } else {
                        surviving_events.push_back(TargetEvent::FrameNavigate {
                            session_id: event_session_id,
                            frame_id,
                            req,
                            tx,
                        });
                    }
                }
                TargetEvent::FrameWaitForNavigation {
                    session_id: event_session_id,
                    frame_id,
                    tx,
                } => {
                    if event_session_id == *session_id {
                        let _ = tx.send(Err(FrameWaitError::FrameSwappedOrDetached));
                    } else {
                        surviving_events.push_back(TargetEvent::FrameWaitForNavigation {
                            session_id: event_session_id,
                            frame_id,
                            tx,
                        });
                    }
                }
                TargetEvent::QueuePreloadScript {
                    request,
                    preload_key,
                } => {
                    if request.session_id.as_deref() != Some(session_id.as_ref()) {
                        surviving_events.push_back(TargetEvent::QueuePreloadScript {
                            request,
                            preload_key,
                        });
                    }
                }
                TargetEvent::AddPreloadScript { params, tx } => {
                    surviving_events.push_back(TargetEvent::AddPreloadScript { params, tx })
                }
                TargetEvent::FanOutAckBatch(batch) => {
                    if let Some(batch) = self.filter_fan_out_batch(batch, session_id) {
                        surviving_events.push_back(TargetEvent::FanOutAckBatch(batch));
                    }
                }
            }
        }

        self.queued_events = surviving_events;
    }

    /// Settle all queued senders when the entire target is gone.
    ///
    /// This is deliberately separate from per-session filtering: no queued
    /// event may survive target teardown, including a fan-out group that has
    /// not yet been registered by the handler.
    pub(crate) fn fail_all_queued_events(&mut self) {
        for event in mem::take(&mut self.queued_events) {
            match event {
                TargetEvent::Request(_) => {}
                TargetEvent::NavigationRequest(_, _) => {}
                TargetEvent::NavigationResult(_) => {}
                TargetEvent::Command(command) => {
                    let _ = command.sender.send(Err(CdpError::NoResponse));
                }
                TargetEvent::RegisterChildSession(_) => {}
                TargetEvent::UnregisterChildSession(_) => {}
                TargetEvent::SessionDraining(_) => {}
                TargetEvent::FrameNavigate { tx, .. } => {
                    let _ = tx.send(Err(CdpError::NoResponse));
                }
                TargetEvent::FrameWaitForNavigation { tx, .. } => {
                    let _ = tx.send(Err(FrameWaitError::FrameSwappedOrDetached));
                }
                TargetEvent::QueuePreloadScript { .. } => {}
                TargetEvent::AddPreloadScript { tx, .. } => {
                    let _ = tx.send(Err(CdpError::NoResponse));
                }
                TargetEvent::FanOutAckBatch(batch) => {
                    let _ = batch.ack_tx.send(Err(CdpError::NoResponse));
                }
            }
        }
    }

    fn settle_target_message_on_teardown(message: TargetMessage) {
        match message {
            TargetMessage::Command(command) => {
                let _ = command.sender.send(Err(CdpError::NoResponse));
            }
            TargetMessage::MainFrame(tx) => {
                let _ = tx.send(None);
            }
            TargetMessage::AllFrames(tx) => {
                let _ = tx.send(Vec::new());
            }
            TargetMessage::Url(GetUrl { tx, .. }) => {
                let _ = tx.send(None);
            }
            TargetMessage::Name(GetName { tx, .. }) => {
                let _ = tx.send(None);
            }
            TargetMessage::Parent(GetParent { tx, .. }) => {
                let _ = tx.send(None);
            }
            TargetMessage::WaitForNavigation(tx) => {
                let _ = tx.send(None);
            }
            TargetMessage::AddEventListener(_) => {}
            TargetMessage::GetExecutionContext(GetExecutionContext { tx, .. }) => {
                let _ = tx.send(None);
            }
            TargetMessage::Authenticate(_) => {}
        }
    }

    fn settle_internal_message_on_teardown(message: InternalTargetMessage) {
        match message {
            InternalTargetMessage::FrameCommand { command, .. }
            | InternalTargetMessage::SessionCommand { command, .. } => {
                let _ = command.sender.send(Err(CdpError::NoResponse));
            }
            InternalTargetMessage::FrameNavigate { tx, .. } => {
                let _ = tx.send(Err(CdpError::NoResponse));
            }
            InternalTargetMessage::FrameWaitForNavigation { tx, .. } => {
                let _ = tx.send(Err(FrameWaitError::FrameSwappedOrDetached));
            }
            InternalTargetMessage::WaitForNavigationResult { tx } => {
                let _ = tx.send(Err(CdpError::FrameNotReady));
            }
            InternalTargetMessage::GetPinnedExecutionContext { tx, .. } => {
                let _ = tx.send(Err(CdpError::NoResponse));
            }
            InternalTargetMessage::GetFrameInfo { tx, .. } => {
                let _ = tx.send(Err(CdpError::NoResponse));
            }
            InternalTargetMessage::GetAllFrames { tx } => {
                let _ = tx.send(Vec::new());
            }
            InternalTargetMessage::GetFrameBoundaryChain { tx, .. } => {
                let _ = tx.send(Err(CdpError::NoResponse));
            }
            InternalTargetMessage::RegisterPausedRequestStream { tx, .. } => {
                // The event and command side channels close when this message
                // is dropped after its acknowledgement is settled.
                let _ = tx.send(Err(CdpError::NoResponse));
            }
            InternalTargetMessage::AddPreloadScript { tx, .. } => {
                let _ = tx.send(Err(CdpError::NoResponse));
            }
            InternalTargetMessage::RegisterEventListener { tx, .. } => {
                let _ = tx.send(Err(CdpError::NoResponse));
            }
            InternalTargetMessage::SetCredentials { tx, .. } => {
                let _ = tx.send(Err(CdpError::NoResponse));
            }
            InternalTargetMessage::SetRequestInterception { tx, .. } => {
                let _ = tx.send(Err(CdpError::NoResponse));
            }
        }
    }

    /// Close every page ingress before draining it, then settle each sender by
    /// its actual result type. This method is only for teardown of the whole
    /// target; child-session detach must leave the page creation waiter alive.
    pub(crate) fn settle_whole_target_teardown(&mut self) {
        if let Some(page) = self.page.as_mut() {
            // Both receivers must be closed before either is drained so no
            // caller can enqueue into the other channel during teardown.
            page.rx.get_mut().close();
            page.internal_rx.get_mut().close();

            drain_closed_receiver(page.rx.get_mut(), Self::settle_target_message_on_teardown);
            drain_closed_receiver(
                page.internal_rx.get_mut(),
                Self::settle_internal_message_on_teardown,
            );
        }

        for tx in mem::take(&mut self.wait_for_frame_navigation) {
            let _ = tx.send(None);
        }
        for tx in mem::take(&mut self.wait_for_navigation_results) {
            let _ = tx.send(Err(CdpError::FrameNotReady));
        }
        self.frame_manager.clear_isolated_world_registrations();
        if let Some(initiator) = self.initiator.take() {
            let _ = initiator.send(Err(CdpError::NoResponse));
        }
    }

    pub fn event_listeners_mut(&mut self) -> &mut EventListeners {
        &mut self.event_listeners
    }

    /// Received a response to a command issued by this target
    pub fn on_response(&mut self, resp: Response, method: &str) {
        self.on_response_in_session(resp, method, None)
    }

    pub(crate) fn on_response_in_session(
        &mut self,
        resp: Response,
        method: &str,
        session_id: Option<SessionId>,
    ) {
        if session_id
            .as_ref()
            .is_some_and(|session_id| self.is_session_draining(session_id))
        {
            return;
        }

        let main_session_id = self.frame_manager.main_session_id().cloned();
        let is_main = session_id
            .as_ref()
            .is_none_or(|session_id| main_session_id.as_ref() == Some(session_id));
        let is_known_child = session_id
            .as_ref()
            .is_some_and(|session_id| self.frame_manager.is_child_session(session_id));
        if is_main {
            let matched = self
                .init_state
                .commands_mut()
                .is_some_and(|cmds| cmds.received_response(method));
            if matched && method == AddScriptToEvaluateOnNewDocumentParams::IDENTIFIER {
                if let Some(main_session_id) = main_session_id.as_ref() {
                    self.frame_manager.settle_isolated_world_registration(
                        main_session_id,
                        UTILITY_WORLD_NAME,
                        resp.error.is_none(),
                    );
                }
            }
        } else if is_known_child {
            let matched_phase = session_id.as_ref().and_then(|session_id| {
                let state = self.iframe_init_states.get_mut(session_id)?;
                let IframeInitState::Chaining { chain, phase } = state else {
                    return None;
                };
                chain.received_response(method).then_some(*phase)
            });
            if matches!(matched_phase, Some(InitPhase::IsolatedWorld)) {
                if let Some(session_id) = session_id.as_ref() {
                    self.frame_manager.settle_isolated_world_registration(
                        session_id,
                        UTILITY_WORLD_NAME,
                        resp.error.is_none(),
                    );
                }
            }
            if resp.error.is_some() {
                if let (Some(session_id), Some(phase)) = (session_id.clone(), matched_phase) {
                    match phase {
                        InitPhase::Frame | InitPhase::AutoAttach => {
                            self.transition_iframe_to_failed_and_unpause(session_id);
                        }
                        InitPhase::IsolatedWorld | InitPhase::Bindings => {
                            self.advance_to_next_iframe_phase(session_id, phase);
                        }
                    }
                }
            }
        } else {
            // A response for a detached child must not mutate the main target
            // or recreate frames through a late getFrameTree result.
            return;
        }

        let process_side_effects = is_main
            || session_id
                .as_ref()
                .and_then(|session_id| self.iframe_init_states.get(session_id))
                .is_some_and(|state| matches!(state, IframeInitState::Chaining { .. }));
        if !process_side_effects {
            return;
        }

        if method == GetFrameTreeParams::IDENTIFIER {
            if let Some(frame_tree) = resp
                .result
                .and_then(|value| GetFrameTreeParams::response_from_value(value).ok())
                .map(|response| response.frame_tree)
            {
                if let Some(session_id) = session_id.or(main_session_id) {
                    for old_session_id in self
                        .frame_manager
                        .on_frame_tree_in_session(frame_tree, session_id)
                    {
                        self.enter_draining_session(old_session_id);
                    }
                } else {
                    self.frame_manager.on_frame_tree(frame_tree);
                }
            }
        }
    }

    pub fn on_event(&mut self, event: CdpEventMessage) {
        let CdpEventMessage {
            params,
            method,
            session_id,
        } = event;
        let event_session_id = session_id
            .map(SessionId::new)
            .or_else(|| self.frame_manager.main_session_id().cloned());

        if event_session_id
            .as_ref()
            .is_some_and(|session_id| self.is_session_draining(session_id))
            && !matches!(&params, CdpEvent::TargetDetachedFromTarget(_))
        {
            return;
        }

        match &params {
            // `FrameManager` events
            CdpEvent::PageFrameAttached(ev) => {
                if let Some(session_id) = event_session_id.clone() {
                    if let Some(old_session_id) = self.frame_manager.on_frame_attached_in_session(
                        ev.frame_id.clone(),
                        Some(ev.parent_frame_id.clone()),
                        session_id,
                    ) {
                        self.enter_draining_session(old_session_id);
                    }
                } else {
                    self.frame_manager
                        .on_frame_attached(ev.frame_id.clone(), Some(ev.parent_frame_id.clone()));
                }
            }
            CdpEvent::PageFrameDetached(ev) => self.frame_manager.on_frame_detached(ev),
            CdpEvent::PageFrameNavigated(ev) => {
                if let Some(session_id) = event_session_id.clone() {
                    self.frame_manager
                        .on_frame_navigated_in_session(&ev.frame, session_id);
                } else {
                    self.frame_manager.on_frame_navigated(&ev.frame);
                }
            }
            CdpEvent::PageNavigatedWithinDocument(ev) => {
                if let Some(session_id) = event_session_id.clone() {
                    self.frame_manager
                        .on_frame_navigated_within_document_in_session(ev, session_id);
                } else {
                    self.frame_manager.on_frame_navigated_within_document(ev);
                }
            }
            CdpEvent::RuntimeExecutionContextCreated(ev) => {
                if let Some(session_id) = event_session_id.clone() {
                    self.frame_manager
                        .on_frame_execution_context_created_in_session(ev, session_id);
                } else {
                    self.frame_manager.on_frame_execution_context_created(ev);
                }
            }
            CdpEvent::RuntimeExecutionContextDestroyed(ev) => {
                if let Some(session_id) = event_session_id.clone() {
                    self.frame_manager
                        .on_frame_execution_context_destroyed_in_session(ev, session_id);
                } else {
                    self.frame_manager.on_frame_execution_context_destroyed(ev);
                }
            }
            CdpEvent::RuntimeExecutionContextsCleared(_) => {
                if let Some(session_id) = event_session_id.clone() {
                    self.frame_manager
                        .on_execution_contexts_cleared_in_session(session_id);
                } else {
                    self.frame_manager.on_execution_contexts_cleared();
                }
            }
            CdpEvent::RuntimeBindingCalled(ev) => {
                if let Some(session_id) = event_session_id.clone() {
                    self.frame_manager
                        .on_runtime_binding_called_in_session(ev, session_id);
                } else {
                    self.frame_manager.on_runtime_binding_called(ev);
                }
            }
            CdpEvent::PageLifecycleEvent(ev) => {
                if let Some(session_id) = event_session_id.clone() {
                    self.frame_manager
                        .on_page_lifecycle_event_in_session(ev, session_id);
                } else {
                    self.frame_manager.on_page_lifecycle_event(ev);
                }
            }
            CdpEvent::PageFrameStartedLoading(ev) => {
                if let Some(session_id) = event_session_id.clone() {
                    self.frame_manager
                        .on_frame_started_loading_in_session(ev, session_id);
                } else {
                    self.frame_manager.on_frame_started_loading(ev);
                }
            }
            CdpEvent::PageFrameStoppedLoading(ev) => {
                if let Some(session_id) = event_session_id.clone() {
                    self.frame_manager
                        .on_frame_stopped_loading_in_session(ev, session_id);
                } else {
                    self.frame_manager.on_frame_stopped_loading(ev);
                }
            }

            // `Target` events
            CdpEvent::TargetAttachedToTarget(ev) => {
                let is_iframe = ev.target_info.r#type == "iframe";
                if is_iframe {
                    self.queued_events
                        .push_back(TargetEvent::RegisterChildSession(ev.session_id.clone()));
                    self.frame_manager
                        .on_attached_to_target_in_session(ev, ev.session_id.clone());
                    self.start_iframe_init(ev.session_id.clone());
                }
                if ev.waiting_for_debugger && !is_iframe {
                    let runtime_cmd = RunIfWaitingForDebuggerParams::default();

                    self.queued_events.push_back(TargetEvent::Request(Request {
                        method: runtime_cmd.identifier(),
                        session_id: Some(ev.session_id.clone().into()),
                        params: serde_json::to_value(runtime_cmd).unwrap(),
                    }));
                }

                if "service_worker" == &ev.target_info.r#type {
                    let detach_command = DetachFromTargetParams::builder()
                        .session_id(ev.session_id.clone())
                        .build();

                    self.queued_events.push_back(TargetEvent::Request(Request {
                        method: detach_command.identifier(),
                        session_id: self.session_id.clone().map(Into::into),
                        params: serde_json::to_value(detach_command).unwrap(),
                    }));
                }
            }
            CdpEvent::TargetDetachedFromTarget(ev) => {
                if let Some(parent_session_id) = event_session_id.clone() {
                    self.handle_detached_from_target(ev, parent_session_id);
                }
            }

            // `NetworkManager` events
            CdpEvent::FetchRequestPaused(ev) => {
                if let Some(session_id) = event_session_id.as_ref() {
                    let disposition = self
                        .network_manager
                        .on_fetch_request_paused_in_session(ev, session_id);
                    if disposition == PauseDisposition::UserIntercept {
                        let pushed = self.paused_request_sink.take().is_some_and(|sink| {
                            let paused = PausedRequest::new(
                                Arc::new((**ev).clone()),
                                session_id.clone(),
                                sink.commands.clone(),
                            );
                            if sink.events.unbounded_send(paused).is_ok() {
                                self.paused_request_sink = Some(sink);
                                true
                            } else {
                                false
                            }
                        });
                        let is_main_session = self
                            .frame_manager
                            .main_session_id()
                            .is_some_and(|main_session_id| main_session_id == session_id);
                        if !pushed && (!is_main_session || !self.page_exposed) {
                            self.network_manager.push_cdp_request_session(
                                ContinueRequestParams::new(ev.request_id.clone()),
                                session_id.clone(),
                            );
                        }
                    }
                } else {
                    self.network_manager.on_fetch_request_paused(ev);
                }
            }
            CdpEvent::FetchAuthRequired(ev) => {
                if let Some(session_id) = event_session_id.as_ref() {
                    self.network_manager
                        .on_fetch_auth_required_in_session(ev, session_id);
                } else {
                    self.network_manager.on_fetch_auth_required(ev);
                }
            }
            CdpEvent::NetworkRequestWillBeSent(ev) => {
                if let Some(session_id) = event_session_id.as_ref() {
                    self.network_manager
                        .on_request_will_be_sent_in_session(ev, session_id);
                } else {
                    self.network_manager.on_request_will_be_sent(ev);
                }
            }
            CdpEvent::NetworkRequestServedFromCache(ev) => {
                self.network_manager.on_request_served_from_cache(ev)
            }
            CdpEvent::NetworkResponseReceived(ev) => self.network_manager.on_response_received(ev),
            CdpEvent::NetworkLoadingFinished(ev) => {
                self.network_manager.on_network_loading_finished(ev)
            }
            CdpEvent::NetworkLoadingFailed(ev) => {
                self.network_manager.on_network_loading_failed(ev)
            }
            // This matches Chrome's full `CdpEvent` set (not the internal
            // `TargetEvent` queue, whose match is intentionally exhaustive). We
            // only consume the events above and silently ignore the rest.
            _ => {}
        }
        while let Some(request) = self.network_manager.poll_session_request() {
            self.queued_events.push_back(TargetEvent::Request(request));
        }
        chromiumoxide_cdp::consume_event!(match params {
           |ev| self.event_listeners.start_send(ev),
           |json| { let _ = self.event_listeners.try_send_custom(&method, json);}
        });
    }

    fn handle_detached_from_target(
        &mut self,
        event: &EventDetachedFromTarget,
        parent_session_id: SessionId,
    ) {
        self.enter_draining_session(event.session_id.clone());
        self.frame_manager
            .on_detached_from_target(&event.session_id, &parent_session_id);
        self.network_manager.on_session_detached(&event.session_id);
        self.queued_events
            .push_back(TargetEvent::UnregisterChildSession(
                event.session_id.clone(),
            ));
    }

    /// Called when a init command timed out
    fn on_initialization_failed(&mut self) -> TargetEvent {
        if let Some(initiator) = self.initiator.take() {
            let _ = initiator.send(Err(CdpError::Timeout));
        }
        self.init_state = TargetInit::Closing;
        let close_target = CloseTargetParams::new(self.info.target_id.clone());
        TargetEvent::Request(Request {
            method: close_target.identifier(),
            session_id: self.session_id.clone().map(Into::into),
            params: serde_json::to_value(close_target).unwrap(),
        })
    }

    /// Advance that target's state
    pub(crate) fn poll(&mut self, cx: &mut Context<'_>, now: Instant) -> Option<TargetEvent> {
        if !self.is_page() {
            // can only poll pages
            return None;
        }
        match &mut self.init_state {
            TargetInit::AttachToTarget => {
                self.init_state = TargetInit::InitializingFrame(FrameManager::init_commands(
                    self.config.request_timeout,
                ));
                let params = AttachToTargetParams::builder()
                    .target_id(self.target_id().clone())
                    .flatten(true)
                    .build()
                    .unwrap();

                return Some(TargetEvent::Request(Request::new(
                    params.identifier(),
                    serde_json::to_value(params).unwrap(),
                )));
            }
            TargetInit::InitializingFrame(cmds) => {
                self.session_id.as_ref()?;
                if let Poll::Ready(poll) = cmds.poll(now) {
                    return match poll {
                        None => {
                            if self.main_isolated_world_attempted {
                                self.init_state = TargetInit::InitializingNetwork(
                                    self.network_manager.init_commands(),
                                );
                            } else {
                                self.main_isolated_world_attempted = true;
                                if let Some(isolated_world_cmds) =
                                    self.frame_manager.ensure_isolated_world(UTILITY_WORLD_NAME)
                                {
                                    *cmds = isolated_world_cmds;
                                } else {
                                    self.init_state = TargetInit::InitializingNetwork(
                                        self.network_manager.init_commands(),
                                    );
                                }
                            }
                            self.poll(cx, now)
                        }
                        Some(Ok((method, params))) => Some(TargetEvent::Request(Request {
                            method,
                            session_id: self.session_id.clone().map(Into::into),
                            params,
                        })),
                        Some(Err(_)) => Some(self.on_initialization_failed()),
                    };
                } else {
                    return None;
                }
            }
            TargetInit::InitializingNetwork(cmds) => {
                advance_state!(
                    self,
                    cx,
                    now,
                    cmds,
                    TargetInit::InitializingPage(Self::page_init_commands(
                        self.config.request_timeout
                    ))
                );
            }
            TargetInit::InitializingPage(cmds) => {
                advance_state!(
                    self,
                    cx,
                    now,
                    cmds,
                    match self.config.viewport.as_ref() {
                        Some(viewport) => TargetInit::InitializingEmulation(
                            self.emulation_manager.init_commands(viewport)
                        ),
                        None => TargetInit::Initialized,
                    }
                );
            }
            TargetInit::InitializingEmulation(cmds) => {
                advance_state!(self, cx, now, cmds, TargetInit::Initialized);
            }
            TargetInit::Initialized => {
                if let Some(initiator) = self.initiator.take() {
                    // make sure that the main frame of the page has finished loading
                    if self
                        .frame_manager
                        .main_frame()
                        .map(|frame| frame.is_loaded())
                        .unwrap_or_default()
                    {
                        if let Some(page) = self.get_or_create_page() {
                            let page = Page::from(page.clone());
                            let delivered = initiator.send(Ok(page)).is_ok();
                            self.mark_page_exposed_after_successful_send(delivered);
                        } else {
                            self.initiator = Some(initiator);
                        }
                    } else {
                        self.initiator = Some(initiator);
                    }
                }
            }
            TargetInit::Closing => return None,
        };
        loop {
            if let Some(frame) = self.frame_manager.main_frame() {
                if frame.is_loaded() {
                    while let Some(tx) = self.wait_for_frame_navigation.pop() {
                        let _ = tx.send(frame.http_request().cloned());
                    }
                    while let Some(tx) = self.wait_for_navigation_results.pop() {
                        let _ = tx.send(Ok(frame.http_request().cloned()));
                    }
                }
            }

            // Drain queued messages first.
            if let Some(ev) = self.queued_events.pop_front() {
                return Some(ev);
            }

            // Drain each channel into a local batch before mutating Target
            // state. This releases the PageHandle borrow and keeps per-channel
            // FIFO without inventing an ordering relation between channels.
            let (target_messages, internal_messages) = if let Some(handle) = self.page.as_mut() {
                let (target_messages, _) = poll_receiver_batch(&mut handle.rx, cx);
                let (internal_messages, _) = poll_receiver_batch(&mut handle.internal_rx, cx);
                (target_messages, internal_messages)
            } else {
                (Vec::new(), Vec::new())
            };

            for msg in target_messages {
                match msg {
                    TargetMessage::Command(cmd) => {
                        self.queued_events.push_back(TargetEvent::Command(cmd));
                    }
                    TargetMessage::MainFrame(tx) => {
                        let _ = tx.send(self.frame_manager.main_frame().map(|f| f.id().clone()));
                    }
                    TargetMessage::AllFrames(tx) => {
                        let _ = tx.send(
                            self.frame_manager
                                .frames()
                                .map(|f| f.id().clone())
                                .collect(),
                        );
                    }
                    TargetMessage::Url(req) => {
                        let GetUrl { frame_id, tx } = req;
                        let frame = if let Some(frame_id) = frame_id {
                            self.frame_manager.frame(&frame_id)
                        } else {
                            self.frame_manager.main_frame()
                        };
                        let _ = tx.send(frame.and_then(|f| f.url().map(str::to_string)));
                    }
                    TargetMessage::Name(req) => {
                        let GetName { frame_id, tx } = req;
                        let frame = if let Some(frame_id) = frame_id {
                            self.frame_manager.frame(&frame_id)
                        } else {
                            self.frame_manager.main_frame()
                        };
                        let _ = tx.send(frame.and_then(|f| f.name().map(str::to_string)));
                    }
                    TargetMessage::Parent(req) => {
                        let GetParent { frame_id, tx } = req;
                        let frame = self.frame_manager.frame(&frame_id);
                        let _ = tx.send(frame.and_then(|f| f.parent_id().cloned()));
                    }
                    TargetMessage::WaitForNavigation(tx) => {
                        if let Some(frame) = self.frame_manager.main_frame() {
                            // This legacy waiter observes the main frame only;
                            // frame-scoped navigation uses the internal channel.
                            if frame.is_loaded() {
                                let _ = tx.send(frame.http_request().cloned());
                            } else {
                                self.wait_for_frame_navigation.push(tx);
                            }
                        } else {
                            self.wait_for_frame_navigation.push(tx);
                        }
                    }
                    TargetMessage::AddEventListener(req) => {
                        self.event_listeners.add_listener(req);
                    }
                    TargetMessage::GetExecutionContext(ctx) => {
                        let GetExecutionContext {
                            dom_world,
                            frame_id,
                            tx,
                        } = ctx;
                        let frame = if let Some(frame_id) = frame_id {
                            self.frame_manager.frame(&frame_id)
                        } else {
                            self.frame_manager.main_frame()
                        };

                        let main_session_id = self.frame_manager.main_session_id();
                        if let Some(frame) = frame.filter(|frame| {
                            main_session_id.is_some_and(|main_session_id| {
                                frame.session_id() == Some(main_session_id)
                            })
                        }) {
                            match dom_world {
                                DOMWorldKind::Main => {
                                    let _ = tx.send(frame.main_world().execution_context());
                                }
                                DOMWorldKind::Secondary => {
                                    let _ = tx.send(frame.secondary_world().execution_context());
                                }
                            }
                        } else {
                            let _ = tx.send(None);
                        }
                    }
                    TargetMessage::Authenticate(credentials) => {
                        self.network_manager.authenticate(credentials);
                    }
                }
            }

            for msg in internal_messages {
                match msg {
                    InternalTargetMessage::FrameCommand {
                        frame_id,
                        expected_session_id,
                        mut command,
                    } => {
                        // Defense in depth: `Frame::execute` already rejects
                        // navigation before it reaches this channel, so this arm
                        // guards only a future internal caller that constructs a
                        // FrameCommand directly. Routing a navigation through the
                        // generic command path would drop the frame identity and
                        // misroute to the main frame; navigation must use
                        // FrameNavigate.
                        if command.is_navigation() {
                            let _ = command.sender.send(Err(CdpError::NotAllowed(
                                "Page.navigate is not supported through Frame::execute; use Frame::goto instead"
                                    .to_owned(),
                            )));
                            continue;
                        }
                        if !self.frame_ready(&frame_id, &expected_session_id) {
                            let _ = command.sender.send(Err(CdpError::FrameNotReady));
                            continue;
                        }
                        command.session_id = Some(expected_session_id);
                        self.queued_events.push_back(TargetEvent::Command(command));
                    }
                    InternalTargetMessage::SessionCommand {
                        session_id,
                        mut command,
                    } => {
                        // A captured session does not carry the frame identity
                        // needed to route Page.navigate safely across OOP swaps.
                        if command.is_navigation() {
                            let _ = command.sender.send(Err(CdpError::NotAllowed(
                                "Page.navigate is not supported through a captured session; use Frame::goto instead"
                                    .to_owned(),
                            )));
                            continue;
                        }
                        command.session_id = Some(session_id);
                        self.queued_events.push_back(TargetEvent::Command(command));
                    }
                    InternalTargetMessage::FrameNavigate {
                        frame_id,
                        session_id,
                        req,
                        tx,
                    } => {
                        if !self.frame_ready(&frame_id, &session_id) {
                            let _ = tx.send(Err(CdpError::FrameNotReady));
                            continue;
                        }
                        self.queued_events.push_back(TargetEvent::FrameNavigate {
                            session_id,
                            frame_id,
                            req,
                            tx,
                        });
                    }
                    InternalTargetMessage::FrameWaitForNavigation {
                        frame_id,
                        session_id,
                        tx,
                    } => {
                        if !self.frame_ready(&frame_id, &session_id) {
                            let _ = tx.send(Err(FrameWaitError::FrameSwappedOrDetached));
                            continue;
                        }
                        self.queued_events
                            .push_back(TargetEvent::FrameWaitForNavigation {
                                session_id,
                                frame_id,
                                tx,
                            });
                    }
                    InternalTargetMessage::WaitForNavigationResult { tx } => {
                        if let Some(frame) = self.frame_manager.main_frame() {
                            if frame.is_loaded() {
                                let _ = tx.send(Ok(frame.http_request().cloned()));
                            } else {
                                self.wait_for_navigation_results.push(tx);
                            }
                        } else {
                            self.wait_for_navigation_results.push(tx);
                        }
                    }
                    InternalTargetMessage::GetPinnedExecutionContext {
                        dom_world,
                        frame_id,
                        expected_session_id,
                        tx,
                    } => {
                        if !self.frame_ready(&frame_id, &expected_session_id) {
                            let _ = tx.send(Err(CdpError::FrameNotReady));
                            continue;
                        }
                        let context_id =
                            self.frame_manager
                                .frame(&frame_id)
                                .and_then(|frame| match dom_world {
                                    DOMWorldKind::Main => frame.main_world().execution_context(),
                                    DOMWorldKind::Secondary => {
                                        frame.secondary_world().execution_context()
                                    }
                                });
                        let _ = tx.send(Ok(context_id.map(|context_id| ExecutionContextInfo {
                            context_id,
                            session_id: expected_session_id,
                        })));
                    }
                    InternalTargetMessage::GetFrameInfo { frame_id, tx } => {
                        let info = self
                            .frame_manager
                            .frame(&frame_id)
                            .map(|frame| self.build_frame_info(frame))
                            .transpose();
                        let _ = tx.send(info);
                    }
                    InternalTargetMessage::GetAllFrames { tx } => {
                        let mut frames = self
                            .frame_manager
                            .frames()
                            .filter_map(|frame| self.build_frame_info(frame).ok())
                            .collect::<Vec<_>>();
                        frames.sort_by(|left, right| {
                            left.frame_id.as_ref().cmp(right.frame_id.as_ref())
                        });
                        let _ = tx.send(frames);
                    }
                    InternalTargetMessage::GetFrameBoundaryChain {
                        frame_id,
                        expected_session_id,
                        tx,
                    } => {
                        let _ = tx.send(self.build_boundary_chain(&frame_id, &expected_session_id));
                    }
                    InternalTargetMessage::RegisterPausedRequestStream {
                        events,
                        commands,
                        tx,
                    } => {
                        let occupied = self
                            .paused_request_sink
                            .as_ref()
                            .is_some_and(|sink| !sink.events.is_closed());
                        if occupied {
                            let _ = tx.send(Err(CdpError::PausedRequestResponderAlreadyRegistered));
                        } else {
                            self.paused_request_sink = Some(PausedRequestSink { events, commands });
                            let _ = tx.send(Ok(()));
                        }
                    }
                    InternalTargetMessage::AddPreloadScript { params, tx } => {
                        self.queued_events
                            .push_back(TargetEvent::AddPreloadScript { params, tx });
                    }
                    InternalTargetMessage::RegisterEventListener { request, tx } => {
                        self.event_listeners.add_listener(request);
                        let _ = tx.send(Ok(()));
                    }
                    InternalTargetMessage::SetCredentials { credentials, tx } => {
                        let commands = self.network_manager.authenticate_core(credentials);
                        self.enqueue_network_fan_out(commands, tx);
                    }
                    InternalTargetMessage::SetRequestInterception { enabled, tx } => {
                        let commands = self.network_manager.set_request_interception_core(enabled);
                        self.enqueue_network_fan_out(commands, tx);
                    }
                }
            }

            if self.poll_iframe_init(now) {
                continue;
            }

            while let Some(request) = self.network_manager.poll_session_request() {
                self.queued_events.push_back(TargetEvent::Request(request));
            }

            while let Some(event) = self.network_manager.poll() {
                match event {
                    NetworkEvent::SendCdpRequest((method, params)) => {
                        // send a message to the browser
                        self.queued_events.push_back(TargetEvent::Request(Request {
                            method,
                            session_id: self.session_id.clone().map(Into::into),
                            params,
                        }))
                    }
                    NetworkEvent::Request(_) => {}
                    NetworkEvent::Response(_) => {}
                    NetworkEvent::RequestFailed(request) => {
                        self.frame_manager.on_http_request_finished(request);
                    }
                    NetworkEvent::RequestFinished(request) => {
                        self.frame_manager.on_http_request_finished(request);
                    }
                }
            }

            while let Some(event) = self.frame_manager.poll(now) {
                match event {
                    FrameEvent::NavigationResult(res) => {
                        self.queued_events
                            .push_back(TargetEvent::NavigationResult(res));
                    }
                    FrameEvent::NavigationRequest(id, req) => {
                        self.queued_events
                            .push_back(TargetEvent::NavigationRequest(id, req));
                    }
                }
            }

            if self.queued_events.is_empty() {
                return None;
            }
        }
    }

    /// Set the sender half of the channel who requested the creation of this
    /// target
    pub fn set_initiator(&mut self, tx: Sender<Result<Page>>) {
        self.initiator = Some(tx);
    }

    pub(crate) fn page_init_commands(timeout: Duration) -> CommandChain {
        let attach = SetAutoAttachParams::builder()
            .flatten(true)
            .auto_attach(true)
            .wait_for_debugger_on_start(true)
            .build()
            .unwrap();
        let enable_performance = performance::EnableParams::default();
        let enable_log = cdplog::EnableParams::default();
        CommandChain::new(
            vec![
                (attach.identifier(), serde_json::to_value(attach).unwrap()),
                (
                    enable_performance.identifier(),
                    serde_json::to_value(enable_performance).unwrap(),
                ),
                (
                    enable_log.identifier(),
                    serde_json::to_value(enable_log).unwrap(),
                ),
            ],
            timeout,
        )
    }
}

#[derive(Debug, Clone)]
pub struct TargetConfig {
    pub ignore_https_errors: bool,
    ///  Request timeout to use
    pub request_timeout: Duration,
    pub viewport: Option<Viewport>,
    pub request_intercept: bool,
    pub cache_enabled: bool,
}

impl Default for TargetConfig {
    fn default() -> Self {
        Self {
            ignore_https_errors: true,
            request_timeout: Duration::from_millis(REQUEST_TIMEOUT),
            viewport: Default::default(),
            request_intercept: false,
            cache_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TargetType {
    Page,
    BackgroundPage,
    ServiceWorker,
    SharedWorker,
    Other,
    Browser,
    Webview,
    Unknown(String),
}

#[derive(Debug)]
enum IframeInitState {
    Chaining {
        chain: CommandChain,
        phase: InitPhase,
    },
    PostChainPreload,
    PostChainNetwork,
    PostChainUnpause,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum InitPhase {
    Frame,
    IsolatedWorld,
    AutoAttach,
    Bindings,
}

impl InitPhase {
    fn next(self) -> Option<Self> {
        match self {
            Self::Frame => Some(Self::IsolatedWorld),
            Self::IsolatedWorld => Some(Self::AutoAttach),
            Self::AutoAttach => Some(Self::Bindings),
            Self::Bindings => None,
        }
    }
}

enum IframeInitAction {
    Send {
        method: MethodId,
        params: serde_json::Value,
    },
    Timeout,
    PhaseComplete(InitPhase),
    EnqueueNetwork,
    EnqueueUnpause,
    MarkDone,
    RemoveDone,
}

impl TargetType {
    pub fn new(ty: &str) -> Self {
        match ty {
            "page" => TargetType::Page,
            "background_page" => TargetType::BackgroundPage,
            "service_worker" => TargetType::ServiceWorker,
            "shared_worker" => TargetType::SharedWorker,
            "other" => TargetType::Other,
            "browser" => TargetType::Browser,
            "webview" => TargetType::Webview,
            s => TargetType::Unknown(s.to_string()),
        }
    }

    pub fn is_page(&self) -> bool {
        matches!(self, TargetType::Page)
    }

    pub fn is_background_page(&self) -> bool {
        matches!(self, TargetType::BackgroundPage)
    }

    pub fn is_service_worker(&self) -> bool {
        matches!(self, TargetType::ServiceWorker)
    }

    pub fn is_shared_worker(&self) -> bool {
        matches!(self, TargetType::SharedWorker)
    }

    pub fn is_other(&self) -> bool {
        matches!(self, TargetType::Other)
    }

    pub fn is_browser(&self) -> bool {
        matches!(self, TargetType::Browser)
    }

    pub fn is_webview(&self) -> bool {
        matches!(self, TargetType::Webview)
    }
}

/// A response-confirmed multi-session command batch waiting to be registered
/// by the handler.
///
/// The two request bags are disjoint: `ack_reqs` contribute responses to the
/// caller's acknowledgement, while `send_only_reqs` only receive the latest
/// state. Keeping them in one queue item preserves ordering against later
/// navigation commands.
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct FanOutAckBatch {
    pub(crate) ack_reqs: Vec<Request>,
    pub(crate) send_only_reqs: Vec<Request>,
    pub(crate) ack_tx: Sender<Result<()>>,
    pub(crate) main_session_id: SessionId,
}

/// Session-qualified execution context returned to frame-pinned callers.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct ExecutionContextInfo {
    pub(crate) context_id: ExecutionContextId,
    pub(crate) session_id: SessionId,
}

/// Immutable frame metadata used to construct a public session-pinned handle.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct FrameInfo {
    pub(crate) frame_id: FrameId,
    pub(crate) session_id: SessionId,
    pub(crate) main_session_id: SessionId,
    pub(crate) parent_id: Option<FrameId>,
    pub(crate) url: Option<String>,
    pub(crate) security_origin: String,
}

/// One cross-session edge in a frame's ancestor chain.
///
/// All topology fields are captured so geometry code can re-query the chain
/// before dispatching input and reject mixed pre/post-swap coordinates.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct FrameBoundary {
    pub(crate) child_frame_id: FrameId,
    pub(crate) child_session_id: SessionId,
    pub(crate) parent_frame_id: FrameId,
    pub(crate) parent_session_id: SessionId,
}

#[derive(Debug)]
#[allow(dead_code)]
struct PausedRequestSink {
    events: UnboundedSender<PausedRequest>,
    commands: futures::channel::mpsc::Sender<TargetMessage>,
}

/// Private page-to-target operations that need stronger identity or ordering
/// guarantees than the legacy public message contract can express.
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum InternalTargetMessage {
    FrameCommand {
        frame_id: FrameId,
        expected_session_id: SessionId,
        command: CommandMessage,
    },
    SessionCommand {
        session_id: SessionId,
        command: CommandMessage,
    },
    FrameNavigate {
        frame_id: FrameId,
        session_id: SessionId,
        req: Request,
        tx: Sender<Result<Response>>,
    },
    FrameWaitForNavigation {
        frame_id: FrameId,
        session_id: SessionId,
        tx: Sender<std::result::Result<(), FrameWaitError>>,
    },
    WaitForNavigationResult {
        tx: Sender<Result<ArcHttpRequest>>,
    },
    GetPinnedExecutionContext {
        dom_world: DOMWorldKind,
        frame_id: FrameId,
        expected_session_id: SessionId,
        tx: Sender<Result<Option<ExecutionContextInfo>>>,
    },
    GetFrameInfo {
        frame_id: FrameId,
        tx: Sender<Result<Option<FrameInfo>>>,
    },
    GetAllFrames {
        tx: Sender<Vec<FrameInfo>>,
    },
    GetFrameBoundaryChain {
        frame_id: FrameId,
        expected_session_id: SessionId,
        tx: Sender<Result<Vec<FrameBoundary>>>,
    },
    RegisterPausedRequestStream {
        events: UnboundedSender<PausedRequest>,
        commands: futures::channel::mpsc::Sender<TargetMessage>,
        tx: Sender<Result<()>>,
    },
    AddPreloadScript {
        params: AddScriptToEvaluateOnNewDocumentParams,
        tx: Sender<Result<ScriptIdentifier>>,
    },
    RegisterEventListener {
        request: EventListenerRequest,
        tx: Sender<Result<()>>,
    },
    SetCredentials {
        credentials: Credentials,
        tx: Sender<Result<()>>,
    },
    SetRequestInterception {
        enabled: bool,
        tx: Sender<Result<()>>,
    },
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum TargetEvent {
    /// An internal request
    Request(Request),
    /// An internal navigation request
    NavigationRequest(NavigationId, Request),
    /// Indicates that a previous requested navigation has finished
    NavigationResult(Result<NavigationOk, NavigationError>),
    /// A new command arrived via a channel
    Command(CommandMessage),
    /// Register a child session for event-envelope routing.
    RegisterChildSession(SessionId),
    /// Remove a child session after all session-scoped pending work is settled.
    UnregisterChildSession(SessionId),
    /// Proactively settle submitted work before Chrome's detach event arrives.
    SessionDraining(SessionId),
    /// Navigate a frame on the immutable session captured by its public handle.
    FrameNavigate {
        session_id: SessionId,
        frame_id: FrameId,
        req: Request,
        tx: Sender<Result<Response>>,
    },
    /// Wait for the next navigation on a frame/session pair.
    FrameWaitForNavigation {
        session_id: SessionId,
        frame_id: FrameId,
        tx: Sender<std::result::Result<(), FrameWaitError>>,
    },
    /// Submit one tracked preload script to a child session.
    QueuePreloadScript {
        request: Request,
        preload_key: PreloadId,
    },
    /// Add and track a preload script on the main session before child replay.
    AddPreloadScript {
        params: AddScriptToEvaluateOnNewDocumentParams,
        tx: Sender<Result<ScriptIdentifier>>,
    },
    /// Submit one response-confirmed dynamic-state update across live sessions.
    FanOutAckBatch(FanOutAckBatch),
}

// TODO this can be moved into the classes?
#[derive(Debug)]
pub enum TargetInit {
    InitializingFrame(CommandChain),
    InitializingNetwork(CommandChain),
    InitializingPage(CommandChain),
    InitializingEmulation(CommandChain),
    AttachToTarget,
    Initialized,
    Closing,
}

impl TargetInit {
    fn commands_mut(&mut self) -> Option<&mut CommandChain> {
        match self {
            TargetInit::InitializingFrame(cmd) => Some(cmd),
            TargetInit::InitializingNetwork(cmd) => Some(cmd),
            TargetInit::InitializingPage(cmd) => Some(cmd),
            TargetInit::InitializingEmulation(cmd) => Some(cmd),
            TargetInit::AttachToTarget => None,
            TargetInit::Initialized => None,
            TargetInit::Closing => None,
        }
    }
}

#[derive(Debug)]
pub struct GetExecutionContext {
    /// For which world the execution context was requested
    pub dom_world: DOMWorldKind,
    /// The if of the frame to get the `ExecutionContext` for
    pub frame_id: Option<FrameId>,
    /// Sender half of the channel to send the response back
    pub tx: Sender<Option<ExecutionContextId>>,
}

impl GetExecutionContext {
    pub fn new(tx: Sender<Option<ExecutionContextId>>) -> Self {
        Self {
            dom_world: DOMWorldKind::Main,
            frame_id: None,
            tx,
        }
    }
}

#[derive(Debug)]
pub struct GetUrl {
    /// The id of the frame to get the url for (None = main frame)
    pub frame_id: Option<FrameId>,
    /// Sender half of the channel to send the response back
    pub tx: Sender<Option<String>>,
}

impl GetUrl {
    pub fn new(tx: Sender<Option<String>>) -> Self {
        Self { frame_id: None, tx }
    }
}

#[derive(Debug)]
pub struct GetName {
    /// The id of the frame to get the name for (None = main frame)
    pub frame_id: Option<FrameId>,
    /// Sender half of the channel to send the response back
    pub tx: Sender<Option<String>>,
}

#[derive(Debug)]
pub struct GetParent {
    /// The id of the frame to get the parent for (None = main frame)
    pub frame_id: FrameId,
    /// Sender half of the channel to send the response back
    pub tx: Sender<Option<FrameId>>,
}

#[derive(Debug)]
pub enum TargetMessage {
    /// Execute a command within the session of this target
    Command(CommandMessage),
    /// Return the main frame of this target's page
    MainFrame(Sender<Option<FrameId>>),
    /// Return all the frames of this target's page
    AllFrames(Sender<Vec<FrameId>>),
    /// Return the url if available
    Url(GetUrl),
    /// Return the name if available
    Name(GetName),
    /// Return the parent id of a frame
    Parent(GetParent),
    /// A Message that resolves when the frame finished loading a new url
    WaitForNavigation(Sender<ArcHttpRequest>),
    /// A request to submit a new listener that gets notified with every
    /// received event
    AddEventListener(EventListenerRequest),
    /// Get the `ExecutionContext` if available
    GetExecutionContext(GetExecutionContext),
    Authenticate(Credentials),
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use futures::channel::mpsc;
    use futures::channel::oneshot;
    use futures::executor::block_on;
    use futures::task::{ArcWake, noop_waker_ref, waker_ref};
    use futures::{SinkExt, StreamExt};
    use proptest::prelude::*;
    use serde_json::json;

    use chromiumoxide_cdp::cdp::browser_protocol::fetch::{EventAuthRequired, EventRequestPaused};
    use chromiumoxide_cdp::cdp::browser_protocol::page::{
        CrossOriginIsolatedContextType, Frame as CdpFrame, FrameTree, GatedApiFeatures,
        GetFrameTreeReturns, NavigateParams, SecureContextType,
    };
    use chromiumoxide_cdp::cdp::browser_protocol::target::EventAttachedToTarget;
    use chromiumoxide_cdp::cdp::js_protocol::runtime::{
        EventExecutionContextCreated, ExecutionContextDescription,
    };

    use super::*;

    #[derive(Debug, Default)]
    struct WakeCounter(AtomicUsize);

    impl ArcWake for WakeCounter {
        fn wake_by_ref(arc_self: &Arc<Self>) {
            arc_self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn session(id: &str) -> SessionId {
        SessionId::new(id)
    }

    fn test_target() -> Target {
        let info = TargetInfo::builder()
            .target_id("target".to_owned())
            .r#type("page")
            .title("test")
            .url("about:blank")
            .attached(true)
            .can_access_opener(false)
            .build()
            .expect("test TargetInfo has all mandatory fields");
        Target::new(info, TargetConfig::default(), BrowserContext::default())
    }

    fn initialized_target() -> (Target, SessionId) {
        let mut target = test_target();
        let main_session = session("main");
        target.set_session_id(main_session.clone());
        target.init_state = TargetInit::Initialized;
        target
            .frame_manager
            .on_frame_navigated_in_session(&cdp_frame("main-frame", None), main_session.clone());
        (target, main_session)
    }

    fn cdp_frame(id: &str, parent_id: Option<&str>) -> CdpFrame {
        let mut builder = CdpFrame::builder()
            .id(FrameId::new(id))
            .loader_id(format!("{id}-loader"))
            .url(format!("https://{id}.example/"))
            .domain_and_registry("example")
            .security_origin(format!("https://{id}.example"))
            .mime_type("text/html")
            .secure_context_type(SecureContextType::Secure)
            .cross_origin_isolated_context_type(CrossOriginIsolatedContextType::NotIsolated)
            .gated_api_features(Vec::<GatedApiFeatures>::new());
        if let Some(parent_id) = parent_id {
            builder = builder.parent_id(FrameId::new(parent_id));
        }
        builder
            .build()
            .expect("frame fixture has all mandatory fields")
    }

    fn child_target_info(frame_id: &str, parent_id: &str) -> TargetInfo {
        TargetInfo::builder()
            .target_id(TargetId::new(frame_id))
            .r#type("iframe")
            .title("iframe")
            .url(format!("https://{frame_id}.example/"))
            .attached(true)
            .can_access_opener(false)
            .parent_frame_id(FrameId::new(parent_id))
            .build()
            .expect("target fixture has all mandatory fields")
    }

    fn attached_event_with_parent(
        frame_id: &str,
        parent_id: &str,
        child_session: &SessionId,
    ) -> EventAttachedToTarget {
        EventAttachedToTarget {
            session_id: child_session.clone(),
            target_info: child_target_info(frame_id, parent_id),
            waiting_for_debugger: true,
        }
    }

    fn attached_event(frame_id: &str, child_session: &SessionId) -> EventAttachedToTarget {
        attached_event_with_parent(frame_id, "main-frame", child_session)
    }

    fn attach_event_message(
        main_session: &SessionId,
        event: EventAttachedToTarget,
    ) -> CdpEventMessage {
        CdpEventMessage {
            method: EventAttachedToTarget::IDENTIFIER.into(),
            session_id: Some(main_session.as_ref().to_owned()),
            params: CdpEvent::TargetAttachedToTarget(Box::new(event)),
        }
    }

    fn paused_request(id: &str) -> EventRequestPaused {
        serde_json::from_value(json!({
            "requestId": id,
            "request": {
                "url": format!("https://{id}.example/"),
                "method": "GET",
                "headers": {},
                "initialPriority": "High",
                "referrerPolicy": "no-referrer"
            },
            "frameId": "frame",
            "resourceType": "Document"
        }))
        .expect("requestPaused fixture is valid")
    }

    fn paused_event_message(session_id: &SessionId, id: &str) -> CdpEventMessage {
        CdpEventMessage {
            method: EventRequestPaused::IDENTIFIER.into(),
            session_id: Some(session_id.as_ref().to_owned()),
            params: CdpEvent::FetchRequestPaused(Box::new(paused_request(id))),
        }
    }

    fn auth_required_event_message(session_id: &SessionId, id: &str) -> CdpEventMessage {
        let event = serde_json::from_value(json!({
            "requestId": id,
            "request": {
                "url": "https://auth.example/",
                "method": "GET",
                "headers": {},
                "initialPriority": "High",
                "referrerPolicy": "no-referrer"
            },
            "frameId": "frame",
            "resourceType": "Document",
            "authChallenge": {
                "source": "Server",
                "origin": "https://auth.example/",
                "scheme": "basic",
                "realm": "test"
            }
        }))
        .expect("authRequired fixture is valid");
        CdpEventMessage {
            method: EventAuthRequired::IDENTIFIER.into(),
            session_id: Some(session_id.as_ref().to_owned()),
            params: CdpEvent::FetchAuthRequired(Box::new(event)),
        }
    }

    fn enable_user_interception(target: &mut Target) {
        let commands = target.network_manager.set_request_interception_core(true);
        assert_eq!(commands.len(), 2);
        while target.network_manager.poll().is_some() {}
    }

    fn continue_requests(target: &Target) -> Vec<&Request> {
        target
            .queued_events
            .iter()
            .filter_map(|event| match event {
                TargetEvent::Request(request)
                    if request.method.as_ref() == ContinueRequestParams::IDENTIFIER =>
                {
                    Some(request)
                }
                _ => None,
            })
            .collect()
    }

    fn install_child(target: &mut Target, frame_id: &str, child_session: &SessionId) {
        let event = attached_event(frame_id, child_session);
        target
            .frame_manager
            .on_attached_to_target_in_session(&event, child_session.clone());
        target.start_iframe_init(child_session.clone());
    }

    fn install_nested_child(
        target: &mut Target,
        frame_id: &str,
        parent_id: &str,
        child_session: &SessionId,
    ) {
        let event = attached_event_with_parent(frame_id, parent_id, child_session);
        target
            .frame_manager
            .on_attached_to_target_in_session(&event, child_session.clone());
        target.start_iframe_init(child_session.clone());
    }

    fn poll_target(target: &mut Target, now: Instant) -> Option<TargetEvent> {
        let mut cx = Context::from_waker(noop_waker_ref());
        target.poll(&mut cx, now)
    }

    fn poll_future_once<F: std::future::Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
        let mut cx = Context::from_waker(noop_waker_ref());
        future.poll(&mut cx)
    }

    fn ok_response(result: serde_json::Value) -> Response {
        serde_json::from_value(json!({ "id": 1, "result": result }))
            .expect("response fixture is valid")
    }

    fn error_response() -> Response {
        serde_json::from_value(json!({
            "id": 1,
            "error": { "code": -32000, "message": "init failed" }
        }))
        .expect("response fixture is valid")
    }

    fn frame_tree_result(frame_id: &str) -> serde_json::Value {
        let mut value = serde_json::to_value(GetFrameTreeReturns::new(FrameTree::new(cdp_frame(
            frame_id,
            Some("main-frame"),
        ))))
        .expect("frame tree fixture should serialize");
        value["frameTree"]["frame"]["gatedAPIFeatures"] = json!([]);
        value
    }

    fn frame_tree_with_child_result(frame_id: &str, child_id: &str) -> serde_json::Value {
        let mut tree = FrameTree::new(cdp_frame(frame_id, Some("main-frame")));
        tree.child_frames = Some(vec![FrameTree::new(cdp_frame(child_id, Some(frame_id)))]);
        let mut value = serde_json::to_value(GetFrameTreeReturns::new(tree))
            .expect("frame tree fixture should serialize");
        value["frameTree"]["frame"]["gatedAPIFeatures"] = json!([]);
        value["frameTree"]["childFrames"][0]["frame"]["gatedAPIFeatures"] = json!([]);
        value
    }

    fn child_request(event: TargetEvent, session_id: &SessionId) -> Request {
        let TargetEvent::Request(request) = event else {
            panic!("expected child initialization request")
        };
        assert_eq!(request.session_id.as_deref(), Some(session_id.as_ref()));
        request
    }

    fn advance_child_to_isolated_world(
        target: &mut Target,
        child_session: &SessionId,
        now: Instant,
    ) -> Request {
        for expected in [
            "Page.enable",
            GetFrameTreeParams::IDENTIFIER,
            "Page.setLifecycleEventsEnabled",
            "Runtime.enable",
        ] {
            let request = child_request(
                poll_target(target, now).expect("frame phase command is ready"),
                child_session,
            );
            assert_eq!(request.method.as_ref(), expected);
            let result = if expected == GetFrameTreeParams::IDENTIFIER {
                frame_tree_result("child-frame")
            } else {
                json!({})
            };
            target.on_response_in_session(
                ok_response(result),
                request.method.as_ref(),
                Some(child_session.clone()),
            );
        }

        let request = child_request(
            poll_target(target, now).expect("isolated-world phase starts"),
            child_session,
        );
        assert_eq!(
            request.method.as_ref(),
            AddScriptToEvaluateOnNewDocumentParams::IDENTIFIER
        );
        request
    }

    fn wire_request(id: usize, session_id: Option<&SessionId>) -> Request {
        Request {
            method: format!("Test.command{id}").into(),
            session_id: session_id.map(|id| id.as_ref().to_owned()),
            params: json!({ "id": id }),
        }
    }

    fn wire_event_ids(target: &Target) -> Vec<usize> {
        target
            .queued_events
            .iter()
            .filter_map(|event| match event {
                TargetEvent::Request(request) | TargetEvent::NavigationRequest(_, request) => {
                    request.params["id"].as_u64().map(|id| id as usize)
                }
                TargetEvent::NavigationResult(_)
                | TargetEvent::Command(_)
                | TargetEvent::RegisterChildSession(_)
                | TargetEvent::UnregisterChildSession(_)
                | TargetEvent::SessionDraining(_)
                | TargetEvent::FrameNavigate { .. }
                | TargetEvent::FrameWaitForNavigation { .. }
                | TargetEvent::QueuePreloadScript { .. }
                | TargetEvent::AddPreloadScript { .. }
                | TargetEvent::FanOutAckBatch(_) => None,
            })
            .collect()
    }

    fn assert_frame_not_ready(result: Result<Response>) {
        assert!(matches!(result, Err(CdpError::FrameNotReady)));
    }

    fn execution_context(
        frame_id: &str,
        context_id: i64,
        unique_id: &str,
    ) -> EventExecutionContextCreated {
        EventExecutionContextCreated {
            context: ExecutionContextDescription::builder()
                .id(ExecutionContextId::new(context_id))
                .origin("https://example.test")
                .name("")
                .unique_id(unique_id)
                .aux_data(json!({
                    "frameId": frame_id,
                    "isDefault": true,
                    "type": "default"
                }))
                .build()
                .expect("context fixture has all mandatory fields"),
        }
    }

    fn isolated_execution_context(
        frame_id: &str,
        context_id: i64,
        unique_id: &str,
        name: &str,
    ) -> EventExecutionContextCreated {
        EventExecutionContextCreated {
            context: ExecutionContextDescription::builder()
                .id(ExecutionContextId::new(context_id))
                .origin("https://example.test")
                .name(name)
                .unique_id(unique_id)
                .aux_data(json!({
                    "frameId": frame_id,
                    "isDefault": false,
                    "type": "isolated"
                }))
                .build()
                .expect("isolated context fixture has all mandatory fields"),
        }
    }

    fn send_internal(target: &mut Target, message: InternalTargetMessage) {
        drain_target_events(target);
        let mut sender = target
            .get_or_create_page()
            .expect("initialized target exposes a page")
            .internal_sender()
            .clone();
        block_on(sender.send(message)).expect("internal receiver remains live");
    }

    fn send_public(target: &mut Target, message: TargetMessage) {
        drain_target_events(target);
        let mut sender = target
            .get_or_create_page()
            .expect("initialized target exposes a page")
            .sender()
            .clone();
        block_on(sender.send(message)).expect("public receiver remains live");
    }

    fn drain_target_events(target: &mut Target) {
        for _ in 0..128 {
            if poll_target(target, Instant::now()).is_none() {
                return;
            }
        }
        panic!("test target did not become idle");
    }

    #[test]
    fn paused_request_registration_rejects_a_live_stream_and_replaces_a_closed_one() {
        let (mut target, _) = initialized_target();
        drain_target_events(&mut target);
        let commands = target
            .get_or_create_page()
            .expect("initialized target exposes a page")
            .sender()
            .clone();

        let (first_tx, first_rx) = mpsc::unbounded();
        let (first_ack_tx, first_ack_rx) = oneshot::channel();
        send_internal(
            &mut target,
            InternalTargetMessage::RegisterPausedRequestStream {
                events: first_tx,
                commands: commands.clone(),
                tx: first_ack_tx,
            },
        );
        let _ = poll_target(&mut target, Instant::now());
        assert!(
            block_on(first_ack_rx)
                .expect("first registration resolves")
                .is_ok()
        );

        let (second_tx, _second_rx) = mpsc::unbounded();
        let (second_ack_tx, second_ack_rx) = oneshot::channel();
        send_internal(
            &mut target,
            InternalTargetMessage::RegisterPausedRequestStream {
                events: second_tx,
                commands: commands.clone(),
                tx: second_ack_tx,
            },
        );
        let _ = poll_target(&mut target, Instant::now());
        assert!(matches!(
            block_on(second_ack_rx).expect("second registration resolves"),
            Err(CdpError::PausedRequestResponderAlreadyRegistered)
        ));

        drop(first_rx);
        let (third_tx, _third_rx) = mpsc::unbounded();
        let (third_ack_tx, third_ack_rx) = oneshot::channel();
        send_internal(
            &mut target,
            InternalTargetMessage::RegisterPausedRequestStream {
                events: third_tx,
                commands,
                tx: third_ack_tx,
            },
        );
        let _ = poll_target(&mut target, Instant::now());
        assert!(
            block_on(third_ack_rx)
                .expect("replacement resolves")
                .is_ok()
        );
    }

    #[test]
    fn concurrent_paused_request_registrations_have_one_winner() {
        let (mut target, _) = initialized_target();
        drain_target_events(&mut target);
        let page = target
            .get_or_create_page()
            .expect("initialized target exposes a page")
            .clone();
        let commands = page.sender().clone();
        let mut first_sender = page.internal_sender().clone();
        let mut second_sender = page.internal_sender().clone();
        let (first_events, _first_receiver) = mpsc::unbounded();
        let (second_events, _second_receiver) = mpsc::unbounded();
        let (first_ack_tx, first_ack_rx) = oneshot::channel();
        let (second_ack_tx, second_ack_rx) = oneshot::channel();

        first_sender
            .try_send(InternalTargetMessage::RegisterPausedRequestStream {
                events: first_events,
                commands: commands.clone(),
                tx: first_ack_tx,
            })
            .expect("first sender has its reserved slot");
        second_sender
            .try_send(InternalTargetMessage::RegisterPausedRequestStream {
                events: second_events,
                commands,
                tx: second_ack_tx,
            })
            .expect("cloned sender has its reserved slot");
        let _ = poll_target(&mut target, Instant::now());

        assert!(
            block_on(first_ack_rx)
                .expect("first registration resolves")
                .is_ok()
        );
        assert!(matches!(
            block_on(second_ack_rx).expect("second registration resolves"),
            Err(CdpError::PausedRequestResponderAlreadyRegistered)
        ));
    }

    #[test]
    fn create_page_marks_exposure_only_after_successful_initiator_delivery() {
        fn loaded_target() -> Target {
            let (mut target, main_session) = initialized_target();
            drain_target_events(&mut target);
            target.frame_manager.on_frame_stopped_loading_in_session(
                &chromiumoxide_cdp::cdp::browser_protocol::page::EventFrameStoppedLoading {
                    frame_id: FrameId::new("main-frame"),
                },
                main_session,
            );
            target
        }

        let mut delivered_target = loaded_target();
        let (tx, rx) = oneshot::channel();
        delivered_target.set_initiator(tx);
        let _ = poll_target(&mut delivered_target, Instant::now());
        assert!(block_on(rx).expect("initiator receives a result").is_ok());
        assert!(delivered_target.page_exposed);

        let mut canceled_target = loaded_target();
        let (tx, rx) = oneshot::channel();
        drop(rx);
        canceled_target.set_initiator(tx);
        let _ = poll_target(&mut canceled_target, Instant::now());
        assert!(!canceled_target.page_exposed);
    }

    #[test]
    fn paused_request_delivery_restores_sink_and_drop_has_no_protocol_side_effect() {
        let (mut target, main_session) = initialized_target();
        drain_target_events(&mut target);
        enable_user_interception(&mut target);
        let commands = target
            .get_or_create_page()
            .expect("initialized target exposes a page")
            .sender()
            .clone();
        let (events, mut receiver) = mpsc::unbounded();
        target.paused_request_sink = Some(PausedRequestSink { events, commands });

        target.on_event(paused_event_message(&main_session, "main-request"));
        let paused = block_on(receiver.next()).expect("managed request is delivered");
        assert_eq!(paused.event().request_id.as_ref(), "main-request");
        assert_eq!(paused.session_id, main_session);
        assert!(target.paused_request_sink.is_some());
        assert!(continue_requests(&target).is_empty());

        drop(paused);
        assert!(continue_requests(&target).is_empty());
    }

    #[test]
    fn paused_request_pre_delivery_fallback_matrix_uses_the_captured_session() {
        let (mut target, main_session) = initialized_target();
        drain_target_events(&mut target);
        enable_user_interception(&mut target);

        target.on_event(paused_event_message(&main_session, "bootstrap-main"));
        let requests = continue_requests(&target);
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].session_id.as_deref(),
            Some(main_session.as_ref())
        );
        assert_eq!(requests[0].params["requestId"], "bootstrap-main");

        target.queued_events.clear();
        target.page_exposed = true;
        target.on_event(paused_event_message(&main_session, "legacy-main"));
        assert!(continue_requests(&target).is_empty());

        let child_session = session("child");
        install_child(&mut target, "child-frame", &child_session);
        target
            .iframe_init_states
            .insert(child_session.clone(), IframeInitState::Done);
        target.on_event(paused_event_message(&child_session, "child-request"));
        let requests = continue_requests(&target);
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].session_id.as_deref(),
            Some(child_session.as_ref())
        );
        assert_eq!(requests[0].params["requestId"], "child-request");
    }

    #[test]
    fn paused_request_push_failure_clears_sink_and_auto_continues_child() {
        let (mut target, _) = initialized_target();
        drain_target_events(&mut target);
        enable_user_interception(&mut target);
        let child_session = session("child");
        install_child(&mut target, "child-frame", &child_session);
        target
            .iframe_init_states
            .insert(child_session.clone(), IframeInitState::Done);

        let commands = target
            .get_or_create_page()
            .expect("initialized target exposes a page")
            .sender()
            .clone();
        let (events, receiver) = mpsc::unbounded();
        drop(receiver);
        target.paused_request_sink = Some(PausedRequestSink { events, commands });

        target.on_event(paused_event_message(&child_session, "failed-push"));
        assert!(target.paused_request_sink.is_none());
        let requests = continue_requests(&target);
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].session_id.as_deref(),
            Some(child_session.as_ref())
        );
        assert_eq!(requests[0].params["requestId"], "failed-push");
    }

    #[test]
    fn auth_auto_response_is_queued_before_typed_listener_delivery() {
        let (mut target, main_session) = initialized_target();
        drain_target_events(&mut target);
        let (events, receiver) = mpsc::unbounded();
        target
            .event_listeners
            .add_listener(EventListenerRequest::new::<EventAuthRequired>(events));
        let mut stream = crate::listeners::EventStream::<EventAuthRequired>::new(receiver);

        target.on_event(auth_required_event_message(&main_session, "auth-request"));

        let request = target
            .queued_events
            .front()
            .and_then(|event| match event {
                TargetEvent::Request(request) => Some(request),
                _ => None,
            })
            .expect("auth response is queued before listener wakeup");
        assert_eq!(request.method.as_ref(), "Fetch.continueWithAuth");
        assert_eq!(request.session_id.as_deref(), Some(main_session.as_ref()));
        assert_eq!(request.params["requestId"], "auth-request");

        let mut cx = Context::from_waker(noop_waker_ref());
        target.event_listeners.poll(&mut cx);
        let observed = block_on(stream.next()).expect("typed auth event remains observable");
        assert_eq!(observed.request_id.as_ref(), "auth-request");
    }

    #[test]
    fn dynamic_network_fan_out_partitions_ack_send_only_and_excluded_sessions() {
        let (mut target, main_session) = initialized_target();
        drain_target_events(&mut target);
        let sessions = [
            ("done", Some(IframeInitState::Done)),
            ("absent", None),
            ("network", Some(IframeInitState::PostChainNetwork)),
            ("unpause", Some(IframeInitState::PostChainUnpause)),
            ("preload", Some(IframeInitState::PostChainPreload)),
            ("failed", Some(IframeInitState::Failed)),
        ];
        for (name, state) in sessions {
            let session_id = session(name);
            install_child(&mut target, &format!("{name}-frame"), &session_id);
            if let Some(state) = state {
                target.iframe_init_states.insert(session_id, state);
            } else {
                target.iframe_init_states.remove(&session_id);
            }
        }
        let chaining = session("chaining");
        install_child(&mut target, "chaining-frame", &chaining);
        let draining = session("draining");
        install_child(&mut target, "draining-frame", &draining);
        target
            .iframe_init_states
            .insert(draining.clone(), IframeInitState::Done);
        target.draining_sessions.insert(draining);

        let commands = target.network_manager.set_request_interception_core(true);
        let (ack_tx, _ack_rx) = oneshot::channel();
        target.enqueue_network_fan_out(commands, ack_tx);

        let batch = match target.queued_events.pop_front() {
            Some(TargetEvent::FanOutAckBatch(batch)) => batch,
            other => panic!("expected one fan-out batch, got {other:?}"),
        };
        assert!(target.queued_events.is_empty());
        assert_eq!(batch.main_session_id, main_session);

        let session_methods = |requests: &[Request]| {
            let mut values = requests
                .iter()
                .map(|request| {
                    (
                        request
                            .session_id
                            .clone()
                            .expect("request is session scoped"),
                        request.method.as_ref().to_owned(),
                    )
                })
                .collect::<Vec<_>>();
            values.sort();
            values
        };
        assert_eq!(
            session_methods(&batch.ack_reqs),
            vec![
                ("absent".to_owned(), "Fetch.enable".to_owned()),
                ("absent".to_owned(), "Network.setCacheDisabled".to_owned()),
                ("done".to_owned(), "Fetch.enable".to_owned()),
                ("done".to_owned(), "Network.setCacheDisabled".to_owned()),
                ("main".to_owned(), "Fetch.enable".to_owned()),
                ("main".to_owned(), "Network.setCacheDisabled".to_owned()),
            ]
        );
        assert_eq!(
            session_methods(&batch.send_only_reqs),
            vec![
                ("network".to_owned(), "Fetch.enable".to_owned()),
                ("network".to_owned(), "Network.setCacheDisabled".to_owned()),
                ("unpause".to_owned(), "Fetch.enable".to_owned()),
                ("unpause".to_owned(), "Network.setCacheDisabled".to_owned()),
            ]
        );
    }

    #[test]
    fn dynamic_network_internal_mutator_queues_only_one_fan_out_event() {
        let (mut target, main_session) = initialized_target();
        drain_target_events(&mut target);
        let (ack_tx, _ack_rx) = oneshot::channel();
        send_internal(
            &mut target,
            InternalTargetMessage::SetRequestInterception {
                enabled: true,
                tx: ack_tx,
            },
        );

        let batch = match poll_target(&mut target, Instant::now()) {
            Some(TargetEvent::FanOutAckBatch(batch)) => batch,
            other => panic!("expected the sole fan-out event, got {other:?}"),
        };
        assert_eq!(batch.ack_reqs.len(), 2);
        assert!(batch.send_only_reqs.is_empty());
        assert_eq!(batch.main_session_id, main_session);
        assert!(target.queued_events.is_empty());
        assert!(target.network_manager.poll().is_none());
    }

    #[test]
    fn dynamic_interception_same_value_retry_re_fans_out_non_empty() {
        // I-020: a same-value retry must re-emit a real (non-empty) fan-out
        // batch. Under the old equality short-circuit the second call produced
        // an empty batch, which `submit_fan_out_ack_batch` resolves `Ok(())`
        // immediately (mod.rs) — a silent false success after a failed enable.
        let (mut target, _main_session) = initialized_target();
        drain_target_events(&mut target);

        let (first_tx, _first_rx) = oneshot::channel();
        send_internal(
            &mut target,
            InternalTargetMessage::SetRequestInterception {
                enabled: true,
                tx: first_tx,
            },
        );
        let first = match poll_target(&mut target, Instant::now()) {
            Some(TargetEvent::FanOutAckBatch(batch)) => batch,
            other => panic!("expected first fan-out batch, got {other:?}"),
        };
        assert!(!first.ack_reqs.is_empty(), "first enable fans out");

        // Retry with the identical value: must still fan out a non-empty batch.
        let (second_tx, _second_rx) = oneshot::channel();
        send_internal(
            &mut target,
            InternalTargetMessage::SetRequestInterception {
                enabled: true,
                tx: second_tx,
            },
        );
        let second = match poll_target(&mut target, Instant::now()) {
            Some(TargetEvent::FanOutAckBatch(batch)) => batch,
            other => panic!("expected a re-emitted fan-out batch on retry, got {other:?}"),
        };
        assert_eq!(
            second.ack_reqs.len(),
            first.ack_reqs.len(),
            "same-value retry re-fans-out the full idempotent batch, not an empty one"
        );
    }

    #[test]
    fn target_config_timeout_is_millis() {
        assert_eq!(
            TargetConfig::default().request_timeout,
            Duration::from_millis(REQUEST_TIMEOUT)
        );
    }

    #[test]
    fn frame_info_distinguishes_gone_unbound_and_bound_frames() {
        let mut target = test_target();
        let main_session = session("main");
        target
            .frame_manager
            .on_frame_navigated_in_session(&cdp_frame("main-frame", None), main_session.clone());
        target.frame_manager.on_frame_attached(
            FrameId::new("unbound-frame"),
            Some(FrameId::new("main-frame")),
        );
        target.set_session_id(main_session.clone());
        target.init_state = TargetInit::Initialized;

        let (gone_tx, gone_rx) = oneshot::channel();
        send_internal(
            &mut target,
            InternalTargetMessage::GetFrameInfo {
                frame_id: FrameId::new("gone"),
                tx: gone_tx,
            },
        );
        let _ = poll_target(&mut target, Instant::now());
        assert!(matches!(
            block_on(gone_rx).expect("frame info resolves"),
            Ok(None)
        ));

        let (unbound_tx, unbound_rx) = oneshot::channel();
        send_internal(
            &mut target,
            InternalTargetMessage::GetFrameInfo {
                frame_id: FrameId::new("unbound-frame"),
                tx: unbound_tx,
            },
        );
        let _ = poll_target(&mut target, Instant::now());
        assert!(matches!(
            block_on(unbound_rx).expect("frame info resolves"),
            Err(CdpError::FrameNotReady)
        ));

        let (bound_tx, bound_rx) = oneshot::channel();
        send_internal(
            &mut target,
            InternalTargetMessage::GetFrameInfo {
                frame_id: FrameId::new("main-frame"),
                tx: bound_tx,
            },
        );
        let _ = poll_target(&mut target, Instant::now());
        let info = block_on(bound_rx)
            .expect("frame info resolves")
            .expect("bound frame is ready")
            .expect("bound frame exists");
        assert_eq!(info.frame_id, FrameId::new("main-frame"));
        assert_eq!(info.session_id, main_session);
        assert_eq!(info.url.as_deref(), Some("https://main-frame.example/"));

        let (all_tx, all_rx) = oneshot::channel();
        send_internal(
            &mut target,
            InternalTargetMessage::GetAllFrames { tx: all_tx },
        );
        let _ = poll_target(&mut target, Instant::now());
        let all = block_on(all_rx).expect("all frames resolves");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].frame_id, FrameId::new("main-frame"));
    }

    #[test]
    fn frame_boundary_chain_skips_same_session_ancestors_and_keeps_oop_edges() {
        let (mut target, main_session) = initialized_target();
        let first_session = session("child-1");
        let second_session = session("child-2");
        install_child(&mut target, "child-frame-1", &first_session);
        install_nested_child(
            &mut target,
            "child-frame-2",
            "child-frame-1",
            &second_session,
        );
        target
            .iframe_init_states
            .insert(first_session.clone(), IframeInitState::Done);
        target
            .iframe_init_states
            .insert(second_session.clone(), IframeInitState::Done);
        target.frame_manager.on_frame_attached_in_session(
            FrameId::new("same-session-descendant"),
            Some(FrameId::new("child-frame-1")),
            first_session.clone(),
        );

        assert!(
            target
                .build_boundary_chain(&FrameId::new("main-frame"), &main_session)
                .expect("main frame chain is ready")
                .is_empty()
        );

        let same_session_chain = target
            .build_boundary_chain(&FrameId::new("same-session-descendant"), &first_session)
            .expect("same-session descendant chain is ready");
        assert_eq!(
            same_session_chain,
            vec![FrameBoundary {
                child_frame_id: FrameId::new("child-frame-1"),
                child_session_id: first_session.clone(),
                parent_frame_id: FrameId::new("main-frame"),
                parent_session_id: main_session.clone(),
            }]
        );

        let nested_chain = target
            .build_boundary_chain(&FrameId::new("child-frame-2"), &second_session)
            .expect("nested OOP chain is ready");
        assert_eq!(
            nested_chain,
            vec![
                FrameBoundary {
                    child_frame_id: FrameId::new("child-frame-2"),
                    child_session_id: second_session,
                    parent_frame_id: FrameId::new("child-frame-1"),
                    parent_session_id: first_session.clone(),
                },
                FrameBoundary {
                    child_frame_id: FrameId::new("child-frame-1"),
                    child_session_id: first_session,
                    parent_frame_id: FrameId::new("main-frame"),
                    parent_session_id: main_session,
                },
            ]
        );
    }

    #[test]
    fn frame_boundary_chain_distinguishes_missing_stale_and_draining_frames() {
        let (mut target, main_session) = initialized_target();
        assert!(matches!(
            target.build_boundary_chain(&FrameId::new("missing"), &main_session),
            Err(CdpError::FrameNotFound(frame)) if frame == FrameId::new("missing")
        ));

        let child_session = session("child");
        install_child(&mut target, "child-frame", &child_session);
        target
            .iframe_init_states
            .insert(child_session.clone(), IframeInitState::Done);
        assert!(matches!(
            target.build_boundary_chain(&FrameId::new("child-frame"), &main_session),
            Err(CdpError::FrameNotReady)
        ));

        target.enter_draining_session(child_session.clone());
        assert!(matches!(
            target.build_boundary_chain(&FrameId::new("child-frame"), &child_session),
            Err(CdpError::FrameNotReady)
        ));
    }

    #[test]
    fn frame_boundary_chain_rejects_unbound_orphan_missing_ancestor_and_cycle() {
        let (mut unbound, main_session) = initialized_target();
        unbound.frame_manager.on_frame_attached_in_session(
            FrameId::new("child"),
            Some(FrameId::new("main-frame")),
            main_session.clone(),
        );
        unbound
            .frame_manager
            .test_unbind_frame(&FrameId::new("child"));
        assert!(matches!(
            unbound.build_boundary_chain(&FrameId::new("child"), &main_session),
            Err(CdpError::FrameNotReady)
        ));

        let (mut orphan, main_session) = initialized_target();
        orphan.frame_manager.on_frame_attached_in_session(
            FrameId::new("child"),
            Some(FrameId::new("main-frame")),
            main_session.clone(),
        );
        orphan
            .frame_manager
            .test_set_frame_parent(&FrameId::new("child"), None);
        assert!(matches!(
            orphan.build_boundary_chain(&FrameId::new("child"), &main_session),
            Err(CdpError::FrameNotReady)
        ));

        let (mut missing_ancestor, main_session) = initialized_target();
        missing_ancestor.frame_manager.on_frame_attached_in_session(
            FrameId::new("child"),
            Some(FrameId::new("main-frame")),
            main_session.clone(),
        );
        missing_ancestor.frame_manager.on_frame_attached_in_session(
            FrameId::new("grandchild"),
            Some(FrameId::new("child")),
            main_session.clone(),
        );
        missing_ancestor
            .frame_manager
            .test_remove_frame_without_descendants(&FrameId::new("child"));
        assert!(matches!(
            missing_ancestor.build_boundary_chain(&FrameId::new("grandchild"), &main_session),
            Err(CdpError::FrameNotReady)
        ));

        let (mut cyclic, main_session) = initialized_target();
        cyclic.frame_manager.on_frame_attached_in_session(
            FrameId::new("child"),
            Some(FrameId::new("main-frame")),
            main_session.clone(),
        );
        cyclic.frame_manager.on_frame_attached_in_session(
            FrameId::new("grandchild"),
            Some(FrameId::new("child")),
            main_session.clone(),
        );
        cyclic
            .frame_manager
            .test_set_frame_parent(&FrameId::new("child"), Some(FrameId::new("grandchild")));
        assert!(matches!(
            cyclic.build_boundary_chain(&FrameId::new("child"), &main_session),
            Err(CdpError::FrameNotReady)
        ));
    }

    #[test]
    fn frame_command_passes_and_forces_the_expected_session() {
        let (mut target, main_session) = initialized_target();
        let (command_tx, command_rx) = oneshot::channel();
        send_internal(
            &mut target,
            InternalTargetMessage::FrameCommand {
                frame_id: FrameId::new("main-frame"),
                expected_session_id: main_session.clone(),
                command: CommandMessage {
                    method: "Runtime.evaluate".into(),
                    session_id: Some(session("wrong")),
                    params: json!({}),
                    sender: command_tx,
                },
            },
        );

        let TargetEvent::Command(command) =
            poll_target(&mut target, Instant::now()).expect("command is queued")
        else {
            panic!("expected a routed frame command")
        };
        assert_eq!(command.session_id, Some(main_session));
        let _ = command.sender.send(Ok(ok_response(json!({}))));
        assert!(
            block_on(command_rx)
                .expect("command sender resolves")
                .is_ok()
        );
    }

    #[test]
    fn frame_command_rejects_a_stale_session_without_queueing() {
        let (mut target, main_session) = initialized_target();
        target.frame_manager.on_frame_attached_in_session(
            FrameId::new("child-frame"),
            Some(FrameId::new("main-frame")),
            main_session.clone(),
        );
        let child_session = session("child");
        target.frame_manager.on_attached_to_target_in_session(
            &attached_event("child-frame", &child_session),
            child_session,
        );

        let (command_tx, command_rx) = oneshot::channel();
        send_internal(
            &mut target,
            InternalTargetMessage::FrameCommand {
                frame_id: FrameId::new("child-frame"),
                expected_session_id: main_session,
                command: CommandMessage {
                    method: "Runtime.evaluate".into(),
                    session_id: None,
                    params: json!({}),
                    sender: command_tx,
                },
            },
        );

        assert!(!matches!(
            poll_target(&mut target, Instant::now()),
            Some(TargetEvent::Command(_))
        ));
        assert_frame_not_ready(block_on(command_rx).expect("command sender resolves"));
    }

    #[test]
    fn frame_command_rejects_navigation_without_queueing() {
        let (mut target, main_session) = initialized_target();
        let (command_tx, command_rx) = oneshot::channel();
        send_internal(
            &mut target,
            InternalTargetMessage::FrameCommand {
                frame_id: FrameId::new("main-frame"),
                expected_session_id: main_session,
                command: CommandMessage {
                    method: NavigateParams::IDENTIFIER.into(),
                    session_id: None,
                    params: json!({ "url": "https://example/next" }),
                    sender: command_tx,
                },
            },
        );

        // A navigation on the raw frame-command path must be rejected before it
        // reaches the queue, so nothing is ever submitted to the target.
        assert!(poll_target(&mut target, Instant::now()).is_none());
        assert!(matches!(
            block_on(command_rx).expect("command sender resolves"),
            Err(CdpError::NotAllowed(_))
        ));
    }

    #[test]
    fn session_command_rejects_navigation_but_page_command_stays_supported() {
        let (mut target, main_session) = initialized_target();
        let (session_tx, session_rx) = oneshot::channel();
        send_internal(
            &mut target,
            InternalTargetMessage::SessionCommand {
                session_id: main_session,
                command: CommandMessage {
                    method: NavigateParams::IDENTIFIER.into(),
                    session_id: None,
                    params: json!({ "url": "https://example/child" }),
                    sender: session_tx,
                },
            },
        );

        assert!(poll_target(&mut target, Instant::now()).is_none());
        assert!(matches!(
            block_on(session_rx).expect("captured-session sender resolves"),
            Err(CdpError::NotAllowed(message)) if message.contains("Frame::goto")
        ));

        let (page_tx, _page_rx) = oneshot::channel();
        send_public(
            &mut target,
            TargetMessage::Command(CommandMessage {
                method: NavigateParams::IDENTIFIER.into(),
                session_id: None,
                params: json!({ "url": "https://example/main" }),
                sender: page_tx,
            }),
        );
        assert!(matches!(
            poll_target(&mut target, Instant::now()),
            Some(TargetEvent::Command(command)) if command.is_navigation()
        ));
    }

    #[test]
    fn execution_context_queries_keep_child_ids_on_the_pinned_path() {
        let (mut target, main_session) = initialized_target();
        target.frame_manager.on_frame_attached_in_session(
            FrameId::new("same-session-frame"),
            Some(FrameId::new("main-frame")),
            main_session.clone(),
        );
        let child_session = session("child");
        target.frame_manager.on_frame_attached_in_session(
            FrameId::new("oop-frame"),
            Some(FrameId::new("main-frame")),
            child_session.clone(),
        );
        target
            .frame_manager
            .on_frame_execution_context_created_in_session(
                &execution_context("main-frame", 7, "main-context"),
                main_session.clone(),
            );
        target
            .frame_manager
            .on_frame_execution_context_created_in_session(
                &execution_context("same-session-frame", 8, "same-context"),
                main_session.clone(),
            );
        target
            .frame_manager
            .on_frame_execution_context_created_in_session(
                &execution_context("oop-frame", 7, "child-context"),
                child_session.clone(),
            );

        let (main_tx, main_rx) = oneshot::channel();
        send_public(
            &mut target,
            TargetMessage::GetExecutionContext(GetExecutionContext {
                dom_world: DOMWorldKind::Main,
                frame_id: Some(FrameId::new("main-frame")),
                tx: main_tx,
            }),
        );
        let _ = poll_target(&mut target, Instant::now());
        assert_eq!(
            block_on(main_rx).expect("main context resolves"),
            Some(ExecutionContextId::new(7))
        );

        let (same_tx, same_rx) = oneshot::channel();
        send_public(
            &mut target,
            TargetMessage::GetExecutionContext(GetExecutionContext {
                dom_world: DOMWorldKind::Main,
                frame_id: Some(FrameId::new("same-session-frame")),
                tx: same_tx,
            }),
        );
        let _ = poll_target(&mut target, Instant::now());
        assert_eq!(
            block_on(same_rx).expect("same-session context resolves"),
            Some(ExecutionContextId::new(8))
        );

        let (oop_tx, oop_rx) = oneshot::channel();
        send_public(
            &mut target,
            TargetMessage::GetExecutionContext(GetExecutionContext {
                dom_world: DOMWorldKind::Main,
                frame_id: Some(FrameId::new("oop-frame")),
                tx: oop_tx,
            }),
        );
        let _ = poll_target(&mut target, Instant::now());
        assert_eq!(block_on(oop_rx).expect("OOP context resolves"), None);

        let (pinned_tx, pinned_rx) = oneshot::channel();
        send_internal(
            &mut target,
            InternalTargetMessage::GetPinnedExecutionContext {
                dom_world: DOMWorldKind::Main,
                frame_id: FrameId::new("oop-frame"),
                expected_session_id: child_session.clone(),
                tx: pinned_tx,
            },
        );
        let _ = poll_target(&mut target, Instant::now());
        let pinned = block_on(pinned_rx)
            .expect("pinned context resolves")
            .expect("pinned frame is ready")
            .expect("pinned context exists");
        assert_eq!(pinned.context_id, ExecutionContextId::new(7));
        assert_eq!(pinned.session_id, child_session);
    }

    #[test]
    fn child_session_registration_precedes_iframe_init_and_attach_stays_paused() {
        let (mut target, main_session) = initialized_target();
        let child_session = session("child");

        target.on_event(attach_event_message(
            &main_session,
            attached_event("child-frame", &child_session),
        ));

        assert!(matches!(
            target.queued_events.front(),
            Some(TargetEvent::RegisterChildSession(session_id)) if session_id == &child_session
        ));
        assert!(matches!(
            target.iframe_init_states.get(&child_session),
            Some(IframeInitState::Chaining {
                phase: InitPhase::Frame,
                ..
            })
        ));
        assert!(!target.frame_session_ready(&child_session));
        assert_eq!(
            target
                .frame_manager
                .frame(&FrameId::new("child-frame"))
                .and_then(|frame| frame.session_id()),
            Some(&child_session)
        );
        assert!(!target.queued_events.iter().any(|event| matches!(
            event,
            TargetEvent::Request(request)
                if request.method.as_ref() == RunIfWaitingForDebuggerParams::IDENTIFIER
                    && request.session_id.as_deref() == Some(child_session.as_ref())
        )));
    }

    #[test]
    fn iframe_init_responses_with_colliding_methods_do_not_cross_sessions() {
        let (mut target, _) = initialized_target();
        let first = session("child-1");
        let second = session("child-2");
        install_child(&mut target, "child-frame-1", &first);
        install_child(&mut target, "child-frame-2", &second);
        let now = Instant::now();

        let TargetEvent::Request(first_request) =
            poll_target(&mut target, now).expect("one child starts initialization")
        else {
            panic!("expected child initialization request")
        };
        let actual_first = SessionId::new(
            first_request
                .session_id
                .as_ref()
                .expect("child request has a session")
                .clone(),
        );
        let other = if actual_first == first {
            second.clone()
        } else {
            first.clone()
        };
        assert_eq!(first_request.method.as_ref(), "Page.enable");

        target.on_response_in_session(
            ok_response(json!({})),
            first_request.method.as_ref(),
            Some(other.clone()),
        );
        let other_request = child_request(
            poll_target(&mut target, now).expect("the other child starts independently"),
            &other,
        );
        assert_eq!(other_request.method.as_ref(), "Page.enable");

        target.on_response_in_session(
            ok_response(json!({})),
            first_request.method.as_ref(),
            Some(actual_first.clone()),
        );
        let next_first = child_request(
            poll_target(&mut target, now).expect("only the matching child advances"),
            &actual_first,
        );
        assert_eq!(next_first.method.as_ref(), GetFrameTreeParams::IDENTIFIER);
    }

    #[test]
    fn iframe_init_frame_tree_side_effects_use_the_response_session() {
        let (mut target, _) = initialized_target();
        let child_session = session("child");
        install_child(&mut target, "child-frame", &child_session);
        let now = Instant::now();

        let enable = child_request(
            poll_target(&mut target, now).expect("frame phase starts"),
            &child_session,
        );
        target.on_response_in_session(
            ok_response(json!({})),
            enable.method.as_ref(),
            Some(child_session.clone()),
        );
        let get_tree = child_request(
            poll_target(&mut target, now).expect("frame tree command is ready"),
            &child_session,
        );
        let tree_result = frame_tree_with_child_result("child-frame", "same-session-descendant");
        assert!(GetFrameTreeParams::response_from_value(tree_result.clone()).is_ok());
        target.on_response_in_session(
            ok_response(tree_result),
            get_tree.method.as_ref(),
            Some(child_session.clone()),
        );

        let descendant = target
            .frame_manager
            .frame(&FrameId::new("same-session-descendant"))
            .expect("frame-tree child should be created");
        assert_eq!(descendant.session_id(), Some(&child_session));
    }

    #[test]
    fn iframe_init_critical_error_fails_and_unpauses_exactly_once() {
        let (mut target, _) = initialized_target();
        let child_session = session("child");
        install_child(&mut target, "child-frame", &child_session);
        let now = Instant::now();
        let request = child_request(
            poll_target(&mut target, now).expect("frame phase starts"),
            &child_session,
        );

        target.on_response_in_session(
            error_response(),
            request.method.as_ref(),
            Some(child_session.clone()),
        );
        target.on_response_in_session(
            error_response(),
            request.method.as_ref(),
            Some(child_session.clone()),
        );

        assert!(matches!(
            target.iframe_init_states.get(&child_session),
            Some(IframeInitState::Failed)
        ));
        assert!(!target.frame_session_ready(&child_session));
        assert_eq!(
            target
                .queued_events
                .iter()
                .filter(|event| matches!(
                    event,
                    TargetEvent::Request(request)
                        if request.method.as_ref()
                            == RunIfWaitingForDebuggerParams::IDENTIFIER
                            && request.session_id.as_deref() == Some(child_session.as_ref())
                ))
                .count(),
            1
        );
    }

    #[test]
    fn iframe_init_timeout_fails_and_unpauses_exactly_once() {
        let (mut target, _) = initialized_target();
        target.config.request_timeout = Duration::from_millis(1);
        let child_session = session("child");
        install_child(&mut target, "child-frame", &child_session);
        let now = Instant::now();
        let request = child_request(
            poll_target(&mut target, now).expect("frame phase starts"),
            &child_session,
        );
        assert_eq!(request.method.as_ref(), "Page.enable");

        let unpause = child_request(
            poll_target(&mut target, now + Duration::from_millis(2))
                .expect("timeout unpauses the child"),
            &child_session,
        );
        assert_eq!(
            unpause.method.as_ref(),
            RunIfWaitingForDebuggerParams::IDENTIFIER
        );
        target.transition_iframe_to_failed_and_unpause(child_session.clone());
        assert!(!target.queued_events.iter().any(|event| matches!(
            event,
            TargetEvent::Request(request)
                if request.method.as_ref() == RunIfWaitingForDebuggerParams::IDENTIFIER
                    && request.session_id.as_deref() == Some(child_session.as_ref())
        )));
    }

    #[test]
    fn iframe_init_optional_error_skips_to_recursive_auto_attach() {
        let (mut target, _) = initialized_target();
        let child_session = session("child");
        install_child(&mut target, "child-frame", &child_session);
        let now = Instant::now();

        let isolated_world = advance_child_to_isolated_world(&mut target, &child_session, now);
        target.on_response_in_session(
            error_response(),
            isolated_world.method.as_ref(),
            Some(child_session.clone()),
        );
        assert_eq!(
            target
                .frame_manager
                .isolated_world_state(&child_session, UTILITY_WORLD_NAME),
            None
        );

        let auto_attach = child_request(
            poll_target(&mut target, now).expect("critical auto-attach still runs"),
            &child_session,
        );
        assert_eq!(auto_attach.method.as_ref(), "Target.setAutoAttach");
        assert!(matches!(
            target.iframe_init_states.get(&child_session),
            Some(IframeInitState::Chaining {
                phase: InitPhase::AutoAttach,
                ..
            })
        ));
        assert!(
            target
                .frame_manager
                .ensure_isolated_world_on_next_document_in_session(
                    UTILITY_WORLD_NAME,
                    child_session,
                )
                .is_some(),
            "an explicit failure leaves no stale local registration marker"
        );
    }

    #[test]
    fn iframe_init_isolated_world_success_confirms_registration() {
        let (mut target, _) = initialized_target();
        let child_session = session("child");
        install_child(&mut target, "child-frame", &child_session);
        let now = Instant::now();
        let isolated_world = advance_child_to_isolated_world(&mut target, &child_session, now);

        target.on_response_in_session(
            ok_response(json!({ "identifier": "utility-script" })),
            isolated_world.method.as_ref(),
            Some(child_session.clone()),
        );

        assert_eq!(
            target
                .frame_manager
                .isolated_world_state(&child_session, UTILITY_WORLD_NAME),
            Some(crate::handler::frame::IsolatedWorldState::Confirmed)
        );
        assert!(
            target
                .frame_manager
                .ensure_isolated_world_on_next_document_in_session(
                    UTILITY_WORLD_NAME,
                    child_session.clone(),
                )
                .is_none()
        );
        let auto_attach = child_request(
            poll_target(&mut target, now).expect("successful optional phase advances"),
            &child_session,
        );
        assert_eq!(auto_attach.method.as_ref(), "Target.setAutoAttach");
    }

    #[test]
    fn main_isolated_world_protocol_error_attempts_add_script_once_then_enters_network_init() {
        let (mut target, main_session) = initialized_target();
        let isolated_world_commands = target
            .frame_manager
            .ensure_isolated_world(UTILITY_WORLD_NAME)
            .expect("main utility-world registration starts once");
        target.main_isolated_world_attempted = true;
        target.init_state = TargetInit::InitializingFrame(isolated_world_commands);
        let now = Instant::now();

        let request = match poll_target(&mut target, now) {
            Some(TargetEvent::Request(request)) => request,
            other => panic!("expected utility-world request, got {other:?}"),
        };
        assert_eq!(
            request.method.as_ref(),
            AddScriptToEvaluateOnNewDocumentParams::IDENTIFIER
        );
        target.on_response_in_session(
            error_response(),
            request.method.as_ref(),
            Some(main_session.clone()),
        );
        assert_eq!(
            target
                .frame_manager
                .isolated_world_state(&main_session, UTILITY_WORLD_NAME),
            None
        );

        let mut methods = vec![request.method.as_ref().to_owned()];
        for _ in 0..8 {
            let next = match poll_target(&mut target, now) {
                Some(TargetEvent::Request(request)) => request,
                other => {
                    panic!("expected remaining utility-world or network request, got {other:?}")
                }
            };
            methods.push(next.method.as_ref().to_owned());
            if next.method.as_ref() == "Network.enable" {
                break;
            }
            assert_eq!(
                next.method.as_ref(),
                chromiumoxide_cdp::cdp::browser_protocol::page::CreateIsolatedWorldParams::IDENTIFIER
            );
            target.on_response_in_session(
                ok_response(json!({})),
                next.method.as_ref(),
                Some(main_session.clone()),
            );
        }

        assert_eq!(
            methods
                .iter()
                .filter(
                    |method| method.as_str() == AddScriptToEvaluateOnNewDocumentParams::IDENTIFIER
                )
                .count(),
            1,
            "main initialization must issue the named-world registration only once"
        );
        assert_eq!(methods.last().map(String::as_str), Some("Network.enable"));
        assert!(matches!(
            target.init_state,
            TargetInit::InitializingNetwork(_)
        ));
    }

    #[test]
    fn iframe_init_isolated_world_timeout_stays_pending_and_unpauses() {
        let timeout = Duration::from_millis(1);
        let mut target = test_target();
        target.config.request_timeout = timeout;
        target.frame_manager = FrameManager::new(timeout);
        let main_session = session("main");
        target.set_session_id(main_session.clone());
        target.init_state = TargetInit::Initialized;
        target
            .frame_manager
            .on_frame_navigated_in_session(&cdp_frame("main-frame", None), main_session);
        let child_session = session("child");
        install_child(&mut target, "child-frame", &child_session);
        let now = Instant::now();
        let _isolated_world = advance_child_to_isolated_world(&mut target, &child_session, now);

        assert!(target.poll_iframe_init(now + Duration::from_millis(2)));
        assert!(target.queued_events.iter().any(|event| matches!(
            event,
            TargetEvent::Request(request)
                if request.method.as_ref() == RunIfWaitingForDebuggerParams::IDENTIFIER
                    && request.session_id.as_deref() == Some(child_session.as_ref())
        )));
        assert!(matches!(
            target.iframe_init_states.get(&child_session),
            Some(IframeInitState::Failed)
        ));
        assert_eq!(
            target
                .frame_manager
                .isolated_world_state(&child_session, UTILITY_WORLD_NAME),
            Some(crate::handler::frame::IsolatedWorldState::Pending)
        );
        assert!(
            target
                .frame_manager
                .ensure_isolated_world_on_next_document_in_session(
                    UTILITY_WORLD_NAME,
                    child_session.clone(),
                )
                .is_none(),
            "a timed-out request must not be duplicated"
        );
        assert!(!target.queued_events.iter().any(|event| matches!(
            event,
            TargetEvent::Request(request)
                if request.method.as_ref() == "Target.setAutoAttach"
                    && request.session_id.as_deref() == Some(child_session.as_ref())
        )));

        target
            .frame_manager
            .on_frame_execution_context_created_in_session(
                &isolated_execution_context(
                    "child-frame",
                    91,
                    "timeout-utility-context",
                    UTILITY_WORLD_NAME,
                ),
                child_session.clone(),
            );
        assert_eq!(
            target
                .frame_manager
                .isolated_world_state(&child_session, UTILITY_WORLD_NAME),
            Some(crate::handler::frame::IsolatedWorldState::Confirmed),
            "an observed utility context upgrades the Pending timeout marker"
        );
    }

    #[test]
    fn preload_chaining_snapshot_and_post_chain_fanout_are_exactly_once() {
        let (mut target, _) = initialized_target();
        drain_target_events(&mut target);
        let first_key = target.frame_manager.add_preload_script(
            AddScriptToEvaluateOnNewDocumentParams::new("globalThis.first = true"),
            ScriptIdentifier::new("main-first"),
        );
        let child_session = session("child");
        install_child(&mut target, "child-frame", &child_session);

        target.enqueue_preload_fan_out(first_key);
        assert!(
            !target
                .queued_events
                .iter()
                .any(|event| matches!(event, TargetEvent::QueuePreloadScript { .. }))
        );

        target.finish_iframe_init_phases(child_session.clone());
        assert!(matches!(
            target.iframe_init_states.get(&child_session),
            Some(IframeInitState::PostChainPreload)
        ));
        let second_key = target.frame_manager.add_preload_script(
            AddScriptToEvaluateOnNewDocumentParams::new("globalThis.second = true"),
            ScriptIdentifier::new("main-second"),
        );
        target.enqueue_preload_fan_out(second_key);

        let queued = target
            .queued_events
            .iter()
            .filter_map(|event| match event {
                TargetEvent::QueuePreloadScript {
                    request,
                    preload_key,
                } => Some((
                    *preload_key,
                    request.session_id.as_deref(),
                    request.params["source"].as_str(),
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            queued,
            vec![
                (
                    first_key,
                    Some(child_session.as_ref()),
                    Some("globalThis.first = true")
                ),
                (
                    second_key,
                    Some(child_session.as_ref()),
                    Some("globalThis.second = true")
                ),
            ]
        );
    }

    #[test]
    fn preload_fanout_targets_only_children_past_the_snapshot_boundary() {
        let (mut target, _) = initialized_target();
        drain_target_events(&mut target);
        let preload_key = target.frame_manager.add_preload_script(
            AddScriptToEvaluateOnNewDocumentParams::new("globalThis.preloaded = true"),
            ScriptIdentifier::new("main-script"),
        );

        let chaining = session("chaining");
        install_child(&mut target, "chaining-frame", &chaining);

        let post_preload = session("post-preload");
        install_child(&mut target, "post-preload-frame", &post_preload);
        target
            .iframe_init_states
            .insert(post_preload.clone(), IframeInitState::PostChainPreload);

        let post_network = session("post-network");
        install_child(&mut target, "post-network-frame", &post_network);
        target
            .iframe_init_states
            .insert(post_network.clone(), IframeInitState::PostChainNetwork);

        let post_unpause = session("post-unpause");
        install_child(&mut target, "post-unpause-frame", &post_unpause);
        target
            .iframe_init_states
            .insert(post_unpause.clone(), IframeInitState::PostChainUnpause);

        let done = session("done");
        install_child(&mut target, "done-frame", &done);
        target
            .iframe_init_states
            .insert(done.clone(), IframeInitState::Done);

        let completed = session("completed");
        install_child(&mut target, "completed-frame", &completed);
        target.iframe_init_states.remove(&completed);

        let failed = session("failed");
        install_child(&mut target, "failed-frame", &failed);
        target
            .iframe_init_states
            .insert(failed.clone(), IframeInitState::Failed);

        let draining = session("draining");
        install_child(&mut target, "draining-frame", &draining);
        target.enter_draining_session(draining);

        target.enqueue_preload_fan_out(preload_key);

        let mut actual = target
            .queued_events
            .iter()
            .filter_map(|event| match event {
                TargetEvent::QueuePreloadScript { request, .. } => {
                    request.session_id.as_deref().map(str::to_owned)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        actual.sort();
        assert_eq!(
            actual,
            vec![
                "completed".to_owned(),
                "done".to_owned(),
                "post-network".to_owned(),
                "post-preload".to_owned(),
                "post-unpause".to_owned(),
            ]
        );
        assert!(
            !actual
                .iter()
                .any(|session_id| session_id == chaining.as_ref())
        );
        assert!(
            !actual
                .iter()
                .any(|session_id| session_id == failed.as_ref())
        );
    }

    #[test]
    fn preload_snapshot_commands_precede_network_replay_and_unpause() {
        let (mut target, _) = initialized_target();
        drain_target_events(&mut target);
        for (index, source) in ["globalThis.first = true", "globalThis.second = true"]
            .into_iter()
            .enumerate()
        {
            target.frame_manager.add_preload_script(
                AddScriptToEvaluateOnNewDocumentParams::new(source),
                ScriptIdentifier::new(format!("main-script-{index}")),
            );
        }
        let child_session = session("child");
        install_child(&mut target, "child-frame", &child_session);
        target.finish_iframe_init_phases(child_session.clone());

        let now = Instant::now();
        let mut methods = Vec::new();
        for _ in 0..32 {
            let Some(event) = poll_target(&mut target, now) else {
                continue;
            };
            let request = match event {
                TargetEvent::QueuePreloadScript { request, .. } | TargetEvent::Request(request) => {
                    request
                }
                other => panic!("unexpected child init event: {other:?}"),
            };
            assert_eq!(request.session_id.as_deref(), Some(child_session.as_ref()));
            methods.push(request.method.as_ref().to_owned());
            if request.method.as_ref() == RunIfWaitingForDebuggerParams::IDENTIFIER {
                break;
            }
        }

        assert_eq!(
            &methods[..2],
            [
                AddScriptToEvaluateOnNewDocumentParams::IDENTIFIER,
                AddScriptToEvaluateOnNewDocumentParams::IDENTIFIER,
            ]
        );
        let network_index = methods
            .iter()
            .position(|method| method == "Network.enable")
            .expect("network replay is emitted");
        let unpause_index = methods
            .iter()
            .position(|method| method == RunIfWaitingForDebuggerParams::IDENTIFIER)
            .expect("child is unpaused");
        assert!(network_index >= 2);
        assert!(network_index < unpause_index);
        assert_eq!(
            methods
                .iter()
                .filter(|method| {
                    method.as_str() == AddScriptToEvaluateOnNewDocumentParams::IDENTIFIER
                })
                .count(),
            2
        );
    }

    #[test]
    fn iframe_init_post_chain_replays_latest_state_before_unpause_and_becomes_ready() {
        let (mut target, _) = initialized_target();
        let child_session = session("child");
        target.network_manager.set_request_interception(true);
        target
            .network_manager
            .set_extra_headers(HashMap::from([("x-test".to_owned(), "latest".to_owned())]));
        target.network_manager.set_offline_mode(true);
        while target.network_manager.poll().is_some() {}
        target.config.viewport = Some(Viewport::default());
        install_child(&mut target, "child-frame", &child_session);

        let now = Instant::now();
        let mut methods = Vec::new();
        for _ in 0..64 {
            let Some(event) = poll_target(&mut target, now) else {
                continue;
            };
            let request = child_request(event, &child_session);
            let method = request.method.as_ref().to_owned();
            methods.push(method.clone());
            if method == RunIfWaitingForDebuggerParams::IDENTIFIER {
                break;
            }
            if matches!(
                target.iframe_init_states.get(&child_session),
                Some(IframeInitState::Chaining { .. })
            ) {
                let result = if method == GetFrameTreeParams::IDENTIFIER {
                    frame_tree_result("child-frame")
                } else {
                    json!({})
                };
                target.on_response_in_session(
                    ok_response(result),
                    &method,
                    Some(child_session.clone()),
                );
            }
        }

        let index = |method: &str| {
            methods
                .iter()
                .position(|candidate| candidate == method)
                .unwrap_or_else(|| panic!("missing {method} in {methods:?}"))
        };
        assert!(index("Page.addScriptToEvaluateOnNewDocument") < index("Target.setAutoAttach"));
        assert!(
            !methods
                .iter()
                .any(|method| method == "Page.createIsolatedWorld")
        );
        assert!(index("Target.setAutoAttach") < index("Network.enable"));
        assert!(index("Network.enable") < index("Fetch.enable"));
        assert!(index("Fetch.enable") < index(RunIfWaitingForDebuggerParams::IDENTIFIER));
        assert!(
            index("Emulation.setDeviceMetricsOverride")
                < index(RunIfWaitingForDebuggerParams::IDENTIFIER)
        );
        assert_eq!(
            methods
                .iter()
                .filter(|method| method.as_str() == RunIfWaitingForDebuggerParams::IDENTIFIER)
                .count(),
            1
        );
        for method in [
            "Network.enable",
            "Security.setIgnoreCertificateErrors",
            "Network.setCacheDisabled",
            "Network.setExtraHTTPHeaders",
            "Network.emulateNetworkConditions",
            "Fetch.enable",
            "Emulation.setDeviceMetricsOverride",
            "Emulation.setTouchEmulationEnabled",
        ] {
            assert_eq!(
                methods
                    .iter()
                    .filter(|candidate| candidate.as_str() == method)
                    .count(),
                1,
                "{method} should be replayed once"
            );
        }

        assert!(poll_target(&mut target, now).is_none());
        assert!(!target.iframe_init_states.contains_key(&child_session));
        assert!(target.frame_session_ready(&child_session));
    }

    #[test]
    fn child_session_draining_cleans_init_network_and_frame_state() {
        let (mut target, main_session) = initialized_target();
        let child_session = session("child");
        install_child(&mut target, "child-frame", &child_session);
        let (wait_tx, wait_rx) = oneshot::channel();
        target.frame_manager.wait_for_navigation(
            child_session.clone(),
            FrameId::new("child-frame"),
            wait_tx,
        );
        target.network_manager.push_cdp_request_session(
            RunIfWaitingForDebuggerParams::default(),
            child_session.clone(),
        );
        target
            .queued_events
            .push_back(TargetEvent::Request(wire_request(1, Some(&child_session))));

        target.enter_draining_session(child_session.clone());

        assert!(!target.iframe_init_states.contains_key(&child_session));
        assert!(target.network_manager.poll_session_request().is_none());
        assert!(!target.frame_session_ready(&child_session));
        assert_eq!(
            block_on(wait_rx).expect("draining settles the promoted frame waiter"),
            Err(FrameWaitError::FrameSwappedOrDetached)
        );
        assert!(!target.queued_events.iter().any(|event| matches!(
            event,
            TargetEvent::Request(request)
                if request.session_id.as_deref() == Some(child_session.as_ref())
        )));

        target.handle_detached_from_target(
            &EventDetachedFromTarget {
                session_id: child_session.clone(),
            },
            main_session.clone(),
        );
        assert!(!target.frame_manager.is_child_session(&child_session));
        assert_eq!(
            target
                .frame_manager
                .frame(&FrameId::new("child-frame"))
                .and_then(|frame| frame.session_id()),
            Some(&main_session)
        );
        assert!(target.queued_events.iter().any(|event| matches!(
            event,
            TargetEvent::UnregisterChildSession(session_id) if session_id == &child_session
        )));
    }

    #[test]
    fn child_session_nested_children_first_detach_rebinds_each_level_to_its_parent() {
        let (mut target, main_session) = initialized_target();
        let first_session = session("child-1");
        let second_session = session("child-2");
        install_child(&mut target, "child-frame-1", &first_session);
        install_nested_child(
            &mut target,
            "child-frame-2",
            "child-frame-1",
            &second_session,
        );

        target.handle_detached_from_target(
            &EventDetachedFromTarget {
                session_id: second_session.clone(),
            },
            first_session.clone(),
        );
        assert_eq!(
            target
                .frame_manager
                .frame(&FrameId::new("child-frame-2"))
                .and_then(|frame| frame.session_id()),
            Some(&first_session)
        );
        assert!(!target.frame_manager.is_child_session(&second_session));

        target.handle_detached_from_target(
            &EventDetachedFromTarget {
                session_id: first_session.clone(),
            },
            main_session.clone(),
        );
        for frame_id in ["child-frame-1", "child-frame-2"] {
            assert_eq!(
                target
                    .frame_manager
                    .frame(&FrameId::new(frame_id))
                    .and_then(|frame| frame.session_id()),
                Some(&main_session)
            );
        }
        assert!(!target.frame_manager.is_child_session(&first_session));
        assert!(!target.iframe_init_states.contains_key(&first_session));
        assert!(!target.iframe_init_states.contains_key(&second_session));
    }

    #[test]
    fn iframe_init_unknown_or_terminal_child_response_has_no_frame_tree_side_effect() {
        let (mut target, _) = initialized_target();
        let ghost = session("ghost");
        target.on_response_in_session(
            ok_response(frame_tree_result("ghost-frame")),
            GetFrameTreeParams::IDENTIFIER,
            Some(ghost),
        );
        assert!(
            target
                .frame_manager
                .frame(&FrameId::new("ghost-frame"))
                .is_none()
        );

        let child_session = session("child");
        install_child(&mut target, "child-frame", &child_session);
        target
            .iframe_init_states
            .insert(child_session.clone(), IframeInitState::Failed);
        target.on_response_in_session(
            ok_response(frame_tree_result("late-frame")),
            GetFrameTreeParams::IDENTIFIER,
            Some(child_session),
        );
        assert!(
            target
                .frame_manager
                .frame(&FrameId::new("late-frame"))
                .is_none()
        );
    }

    #[test]
    fn page_receiver_poll_budget_is_bounded_and_self_wakes() {
        let (sender, receiver) = mpsc::unbounded();
        for value in 0..(PAGE_MESSAGE_POLL_BUDGET + 4) {
            sender
                .unbounded_send(value)
                .expect("test receiver remains open");
        }
        let mut receiver = receiver.fuse();
        let wake_counter = Arc::new(WakeCounter::default());
        let waker = waker_ref(&wake_counter);
        let mut cx = Context::from_waker(&waker);

        let (first_batch, exhausted) = poll_receiver_batch(&mut receiver, &mut cx);
        assert_eq!(first_batch.len(), PAGE_MESSAGE_POLL_BUDGET);
        assert!(exhausted);
        assert_eq!(wake_counter.0.load(Ordering::Relaxed), 1);

        let (second_batch, exhausted) = poll_receiver_batch(&mut receiver, &mut cx);
        assert_eq!(
            second_batch,
            (PAGE_MESSAGE_POLL_BUDGET..PAGE_MESSAGE_POLL_BUDGET + 4).collect::<Vec<_>>()
        );
        assert!(!exhausted);
    }

    #[test]
    fn whole_target_teardown_settles_ingress_storage_and_initiator() {
        let (mut target, main_session) = initialized_target();
        let page = target
            .get_or_create_page()
            .expect("initialized target creates a page handle")
            .clone();

        let mut wait = page.wait_for_navigation();
        assert!(poll_future_once(wait.as_mut()).is_pending());

        let (command_tx, command_rx) = oneshot::channel();
        block_on(
            page.sender()
                .clone()
                .send(TargetMessage::Command(CommandMessage {
                    method: "Runtime.evaluate".into(),
                    session_id: None,
                    params: json!({}),
                    sender: command_tx,
                })),
        )
        .expect("page ingress remains open before teardown");

        let (legacy_tx, legacy_rx) = oneshot::channel();
        target.wait_for_frame_navigation.push(legacy_tx);
        let (promoted_tx, promoted_rx) = oneshot::channel();
        target.wait_for_navigation_results.push(promoted_tx);
        let (initiator_tx, initiator_rx) = oneshot::channel();
        target.set_initiator(initiator_tx);
        assert!(
            target
                .frame_manager
                .ensure_isolated_world_in_session(UTILITY_WORLD_NAME, main_session.clone(),)
                .is_some()
        );

        target.settle_whole_target_teardown();

        assert!(matches!(block_on(wait), Err(CdpError::FrameNotReady)));
        assert!(matches!(
            block_on(command_rx).expect("command sender resolves"),
            Err(CdpError::NoResponse)
        ));
        assert!(
            block_on(legacy_rx)
                .expect("legacy waiter resolves")
                .is_none()
        );
        assert!(matches!(
            block_on(promoted_rx).expect("promoted waiter resolves"),
            Err(CdpError::FrameNotReady)
        ));
        assert!(matches!(
            block_on(initiator_rx).expect("initiator resolves"),
            Err(CdpError::NoResponse)
        ));
        assert_eq!(
            target
                .frame_manager
                .isolated_world_state(&main_session, UTILITY_WORLD_NAME),
            None
        );

        let (late_tx, _late_rx) = oneshot::channel();
        assert!(
            block_on(
                page.internal_sender()
                    .clone()
                    .send(InternalTargetMessage::WaitForNavigationResult { tx: late_tx })
            )
            .is_err(),
            "a send that starts after teardown must fail at the closed ingress"
        );
    }

    #[test]
    fn initiator_already_resolved_does_not_double_settle_on_teardown() {
        let (mut target, main_session) = initialized_target();
        drain_target_events(&mut target);
        target.frame_manager.on_frame_stopped_loading_in_session(
            &chromiumoxide_cdp::cdp::browser_protocol::page::EventFrameStoppedLoading {
                frame_id: FrameId::new("main-frame"),
            },
            main_session,
        );
        let (initiator_tx, initiator_rx) = oneshot::channel();
        target.set_initiator(initiator_tx);

        let _ = poll_target(&mut target, Instant::now());
        assert!(
            block_on(initiator_rx)
                .expect("initiator sender resolves")
                .is_ok()
        );
        assert!(target.initiator.is_none());

        target.settle_whole_target_teardown();
        target.settle_whole_target_teardown();
        assert!(target.initiator.is_none());
    }

    #[test]
    fn promoted_page_wait_and_http_future_keep_typed_teardown_errors() {
        let (mut target, _) = initialized_target();
        let page = target
            .get_or_create_page()
            .expect("initialized target creates a page handle")
            .clone();

        let mut page_wait = page.wait_for_navigation();
        assert!(poll_future_once(page_wait.as_mut()).is_pending());
        let _ = poll_target(&mut target, Instant::now());
        assert_eq!(target.wait_for_navigation_results.len(), 1);

        target.settle_whole_target_teardown();
        assert!(matches!(block_on(page_wait), Err(CdpError::FrameNotReady)));

        let (mut target, _) = initialized_target();
        let page = target
            .get_or_create_page()
            .expect("initialized target creates a page handle")
            .clone();
        let mut http_future = Box::pin(
            page.http_future(RunIfWaitingForDebuggerParams::default())
                .expect("command serializes"),
        );
        assert!(poll_future_once(http_future.as_mut()).is_pending());
        let command = loop {
            match poll_target(&mut target, Instant::now()) {
                Some(TargetEvent::Command(command)) => break command,
                Some(_) => continue,
                None => panic!("command event was not emitted"),
            }
        };
        let _ = command.sender.send(Ok(ok_response(json!({}))));
        assert!(poll_future_once(http_future.as_mut()).is_pending());
        assert!(poll_future_once(http_future.as_mut()).is_pending());
        for _ in 0..32 {
            let _ = poll_target(&mut target, Instant::now());
            if !target.wait_for_navigation_results.is_empty() {
                break;
            }
        }
        assert_eq!(target.wait_for_navigation_results.len(), 1);

        target.settle_whole_target_teardown();
        assert!(matches!(
            block_on(http_future),
            Err(CdpError::FrameNotReady)
        ));
    }

    #[test]
    fn teardown_message_matrix_uses_each_senders_natural_error_shape() {
        let (all_frames_tx, all_frames_rx) = oneshot::channel();
        Target::settle_target_message_on_teardown(TargetMessage::AllFrames(all_frames_tx));
        assert!(
            block_on(all_frames_rx)
                .expect("all-frames sender resolves")
                .is_empty()
        );

        let (main_frame_tx, main_frame_rx) = oneshot::channel();
        Target::settle_target_message_on_teardown(TargetMessage::MainFrame(main_frame_tx));
        assert_eq!(
            block_on(main_frame_rx).expect("main-frame sender resolves"),
            None
        );

        let (legacy_tx, legacy_rx) = oneshot::channel();
        Target::settle_target_message_on_teardown(TargetMessage::WaitForNavigation(legacy_tx));
        assert!(
            block_on(legacy_rx)
                .expect("legacy wait sender resolves")
                .is_none()
        );

        let (all_info_tx, all_info_rx) = oneshot::channel();
        Target::settle_internal_message_on_teardown(InternalTargetMessage::GetAllFrames {
            tx: all_info_tx,
        });
        assert!(
            block_on(all_info_rx)
                .expect("all-frame-info sender resolves")
                .is_empty()
        );

        let (frame_wait_tx, frame_wait_rx) = oneshot::channel();
        Target::settle_internal_message_on_teardown(
            InternalTargetMessage::FrameWaitForNavigation {
                frame_id: FrameId::new("frame"),
                session_id: session("session"),
                tx: frame_wait_tx,
            },
        );
        assert_eq!(
            block_on(frame_wait_rx).expect("frame wait sender resolves"),
            Err(FrameWaitError::FrameSwappedOrDetached)
        );
    }

    proptest! {
        #[test]
        fn queued_event_drain_removes_dead_wires_and_preserves_survivor_fifo(
            entries in prop::collection::vec((0_u8..2, 0_u8..3), 0..80)
        ) {
            let dead = session("dead");
            let live = session("live");
            let mut target = test_target();
            let mut expected = Vec::new();

            for (id, (event_kind, session_kind)) in entries.into_iter().enumerate() {
                let event_session = match session_kind {
                    0 => Some(&dead),
                    1 => Some(&live),
                    2 => None,
                    _ => unreachable!("generated session class is bounded"),
                };
                let request = wire_request(id, event_session);
                let event = if event_kind == 0 {
                    TargetEvent::Request(request)
                } else {
                    TargetEvent::NavigationRequest(NavigationId(id), request)
                };
                target.queued_events.push_back(event);
                if session_kind != 0 {
                    expected.push(id);
                }
            }

            target.fail_queued_events(&dead);

            let actual = wire_event_ids(&target);
            prop_assert_eq!(&actual, &expected);
            for id in expected {
                prop_assert_eq!(actual.iter().filter(|actual_id| **actual_id == id).count(), 1);
            }
        }
    }

    #[test]
    fn queued_event_drain_resolves_each_matching_sender_with_its_typed_error() {
        let dead = session("dead");
        let frame_id = FrameId::new("frame");
        let mut target = test_target();

        let (command_tx, command_rx) = oneshot::channel();
        target
            .queued_events
            .push_back(TargetEvent::Command(CommandMessage {
                method: "Runtime.evaluate".into(),
                session_id: Some(dead.clone()),
                params: json!({}),
                sender: command_tx,
            }));

        let (navigate_tx, navigate_rx) = oneshot::channel();
        target.queued_events.push_back(TargetEvent::FrameNavigate {
            session_id: dead.clone(),
            frame_id: frame_id.clone(),
            req: wire_request(1, Some(&dead)),
            tx: navigate_tx,
        });

        let (wait_tx, wait_rx) = oneshot::channel();
        target
            .queued_events
            .push_back(TargetEvent::FrameWaitForNavigation {
                session_id: dead.clone(),
                frame_id,
                tx: wait_tx,
            });

        target.fail_queued_events(&dead);

        assert_frame_not_ready(block_on(command_rx).expect("command sender resolves"));
        assert_frame_not_ready(block_on(navigate_rx).expect("navigation sender resolves"));
        assert_eq!(
            block_on(wait_rx).expect("wait sender resolves"),
            Err(FrameWaitError::FrameSwappedOrDetached)
        );
        assert!(target.queued_events.is_empty());
    }

    #[test]
    fn queued_event_enter_draining_is_idempotent_and_emits_one_control_event() {
        let dead = session("dead");
        let mut target = test_target();
        target
            .queued_events
            .push_back(TargetEvent::Request(wire_request(0, Some(&dead))));

        target.enter_draining_session(dead.clone());
        target.enter_draining_session(dead.clone());

        assert!(target.is_session_draining(&dead));
        assert_eq!(target.queued_events.len(), 1);
        assert!(matches!(
            target.queued_events.front(),
            Some(TargetEvent::SessionDraining(id)) if id == &dead
        ));
        target.clear_draining(&dead);
        assert!(!target.is_session_draining(&dead));
    }

    #[test]
    fn fan_out_batch_main_removal_fails_ack_and_drops_batch() {
        let main = session("main");
        let child = session("child");
        let mut target = test_target();
        let (ack_tx, ack_rx) = oneshot::channel();

        target
            .queued_events
            .push_back(TargetEvent::FanOutAckBatch(FanOutAckBatch {
                ack_reqs: vec![wire_request(0, Some(&main)), wire_request(1, Some(&child))],
                send_only_reqs: vec![],
                ack_tx,
                main_session_id: main.clone(),
            }));

        target.fail_queued_events(&main);

        assert!(target.queued_events.is_empty());
        assert!(matches!(
            block_on(ack_rx).expect("fan-out ack sender resolves"),
            Err(CdpError::FrameNotReady)
        ));
    }

    #[test]
    fn fan_out_batch_child_removal_keeps_reduced_batch_in_place() {
        let main = session("main");
        let removed = session("removed");
        let live = session("live");
        let mut target = test_target();
        let (ack_tx, _ack_rx) = oneshot::channel();

        target
            .queued_events
            .push_back(TargetEvent::Request(wire_request(0, None)));
        target
            .queued_events
            .push_back(TargetEvent::FanOutAckBatch(FanOutAckBatch {
                ack_reqs: vec![
                    wire_request(1, Some(&main)),
                    wire_request(2, Some(&removed)),
                    wire_request(3, Some(&live)),
                ],
                send_only_reqs: vec![
                    wire_request(4, Some(&removed)),
                    wire_request(5, Some(&live)),
                ],
                ack_tx,
                main_session_id: main,
            }));
        target
            .queued_events
            .push_back(TargetEvent::NavigationRequest(
                NavigationId(6),
                wire_request(6, None),
            ));

        target.fail_queued_events(&removed);

        assert_eq!(target.queued_events.len(), 3);
        assert!(matches!(
            target.queued_events.front(),
            Some(TargetEvent::Request(request)) if request.params["id"] == 0
        ));
        let batch = match target.queued_events.get(1) {
            Some(TargetEvent::FanOutAckBatch(batch)) => batch,
            other => panic!("reduced batch moved from its queue position: {other:?}"),
        };
        assert_eq!(
            batch
                .ack_reqs
                .iter()
                .map(|request| request.params["id"].as_u64().expect("numeric id"))
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(
            batch
                .send_only_reqs
                .iter()
                .map(|request| request.params["id"].as_u64().expect("numeric id"))
                .collect::<Vec<_>>(),
            vec![5]
        );
        assert!(matches!(
            target.queued_events.back(),
            Some(TargetEvent::NavigationRequest(_, request)) if request.params["id"] == 6
        ));
    }

    #[test]
    fn target_drain_all_settles_every_queued_sender() {
        let main = session("main");
        let frame_id = FrameId::new("frame");
        let mut target = test_target();

        let (command_tx, command_rx) = oneshot::channel();
        target
            .queued_events
            .push_back(TargetEvent::Command(CommandMessage {
                method: "Runtime.evaluate".into(),
                session_id: Some(main.clone()),
                params: json!({}),
                sender: command_tx,
            }));
        let (navigate_tx, navigate_rx) = oneshot::channel();
        target.queued_events.push_back(TargetEvent::FrameNavigate {
            session_id: main.clone(),
            frame_id: frame_id.clone(),
            req: wire_request(0, Some(&main)),
            tx: navigate_tx,
        });
        let (wait_tx, wait_rx) = oneshot::channel();
        target
            .queued_events
            .push_back(TargetEvent::FrameWaitForNavigation {
                session_id: main.clone(),
                frame_id,
                tx: wait_tx,
            });
        let (preload_tx, preload_rx) = oneshot::channel();
        target
            .queued_events
            .push_back(TargetEvent::AddPreloadScript {
                params: AddScriptToEvaluateOnNewDocumentParams::new("globalThis.ready = true"),
                tx: preload_tx,
            });
        let (ack_tx, ack_rx) = oneshot::channel();
        target
            .queued_events
            .push_back(TargetEvent::FanOutAckBatch(FanOutAckBatch {
                ack_reqs: vec![wire_request(1, Some(&main))],
                send_only_reqs: vec![],
                ack_tx,
                main_session_id: main,
            }));

        target.fail_all_queued_events();

        assert!(target.queued_events.is_empty());
        assert!(matches!(
            block_on(command_rx).expect("command sender resolves"),
            Err(CdpError::NoResponse)
        ));
        assert!(matches!(
            block_on(navigate_rx).expect("navigation sender resolves"),
            Err(CdpError::NoResponse)
        ));
        assert_eq!(
            block_on(wait_rx).expect("wait sender resolves"),
            Err(FrameWaitError::FrameSwappedOrDetached)
        );
        assert!(matches!(
            block_on(preload_rx).expect("preload sender resolves"),
            Err(CdpError::NoResponse)
        ));
        assert!(matches!(
            block_on(ack_rx).expect("fan-out ack sender resolves"),
            Err(CdpError::NoResponse)
        ));
    }
}
