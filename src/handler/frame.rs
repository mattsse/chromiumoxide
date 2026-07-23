use std::collections::VecDeque;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::channel::oneshot::Sender as OneshotSender;
use serde_json::map::Entry;

use chromiumoxide_cdp::cdp::browser_protocol::network::LoaderId;
use chromiumoxide_cdp::cdp::browser_protocol::page::{
    AddScriptToEvaluateOnNewDocumentParams, CreateIsolatedWorldParams, EventFrameDetached,
    EventFrameStartedLoading, EventFrameStoppedLoading, EventLifecycleEvent,
    EventNavigatedWithinDocument, Frame as CdpFrame, FrameDetachedReason, FrameTree,
    ScriptIdentifier,
};
use chromiumoxide_cdp::cdp::browser_protocol::target::{EventAttachedToTarget, SessionId};
use chromiumoxide_cdp::cdp::js_protocol::runtime::*;
use chromiumoxide_cdp::cdp::{
    browser_protocol::page::{self, FrameId},
    js_protocol::runtime,
};
use chromiumoxide_types::{Method, MethodId, Request};

use crate::error::{CdpError, DeadlineExceeded, Result};
use crate::handler::REQUEST_TIMEOUT;
use crate::handler::domworld::DOMWorld;
use crate::handler::http::HttpRequest;
use crate::{ArcHttpRequest, cmd::CommandChain};

pub const UTILITY_WORLD_NAME: &str = "__chromiumoxide_utility_world__";
pub(crate) const HTTP_RESPONSE_CODE_FAILURE: &str = "net::ERR_HTTP_RESPONSE_CODE_FAILURE";
const EVALUATION_SCRIPT_URL: &str = "____chromiumoxide_utility_world___evaluation_script__";

/// Represents a frame on the page
#[derive(Debug)]
pub struct Frame {
    parent_frame: Option<FrameId>,
    /// Cdp identifier of this frame
    id: FrameId,
    main_world: DOMWorld,
    secondary_world: DOMWorld,
    loader_id: Option<LoaderId>,
    /// Current url of this frame
    url: Option<String>,
    /// The http request that loaded this with this frame
    http_request: ArcHttpRequest,
    /// The frames contained in this frame
    child_frames: HashSet<FrameId>,
    name: Option<String>,
    /// Session that currently owns this frame. `None` is a real transient
    /// unbound state and must never be serialized as an empty session id.
    session_id: Option<SessionId>,
    /// Last security origin reported by `Page.frameNavigated`.
    security_origin: String,
    /// The received lifecycle events
    lifecycle_events: HashSet<MethodId>,
}

impl Frame {
    pub fn new(id: FrameId) -> Self {
        Self {
            parent_frame: None,
            id,
            main_world: Default::default(),
            secondary_world: Default::default(),
            loader_id: None,
            url: None,
            http_request: None,
            child_frames: Default::default(),
            name: None,
            session_id: None,
            security_origin: String::new(),
            lifecycle_events: Default::default(),
        }
    }

    pub fn with_parent(id: FrameId, parent: &mut Frame) -> Self {
        parent.child_frames.insert(id.clone());
        Self {
            parent_frame: Some(parent.id.clone()),
            id,
            main_world: Default::default(),
            secondary_world: Default::default(),
            loader_id: None,
            url: None,
            http_request: None,
            child_frames: Default::default(),
            name: None,
            session_id: None,
            security_origin: String::new(),
            lifecycle_events: Default::default(),
        }
    }

    pub fn parent_id(&self) -> Option<&FrameId> {
        self.parent_frame.as_ref()
    }

    pub fn id(&self) -> &FrameId {
        &self.id
    }

    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub(crate) fn session_id(&self) -> Option<&SessionId> {
        self.session_id.as_ref()
    }

    #[allow(dead_code)]
    pub(crate) fn require_session_id(&self) -> Result<&SessionId> {
        self.session_id.as_ref().ok_or(CdpError::FrameNotReady)
    }

    pub(crate) fn set_session_id(&mut self, session_id: SessionId) {
        if self.session_id.as_ref() != Some(&session_id) {
            self.clear_contexts();
        }
        self.session_id = Some(session_id);
    }

    pub(crate) fn is_out_of_process(&self, main_session_id: &SessionId) -> bool {
        self.session_id
            .as_ref()
            .is_some_and(|session_id| session_id != main_session_id)
    }

    #[allow(dead_code)]
    pub(crate) fn security_origin(&self) -> &str {
        &self.security_origin
    }

    pub fn main_world(&self) -> &DOMWorld {
        &self.main_world
    }

    pub fn secondary_world(&self) -> &DOMWorld {
        &self.secondary_world
    }

    pub fn lifecycle_events(&self) -> &HashSet<MethodId> {
        &self.lifecycle_events
    }

    pub fn http_request(&self) -> Option<&Arc<HttpRequest>> {
        self.http_request.as_ref()
    }

    fn navigated(&mut self, frame: &CdpFrame) {
        self.name.clone_from(&frame.name);
        let url = if let Some(ref fragment) = frame.url_fragment {
            format!("{}{fragment}", frame.url)
        } else {
            frame.url.clone()
        };
        self.url = Some(url);
        self.security_origin.clone_from(&frame.security_origin);
    }

    fn navigated_within_url(&mut self, url: String) {
        self.url = Some(url)
    }

    fn on_loading_stopped(&mut self) {
        self.lifecycle_events.insert("DOMContentLoaded".into());
        self.lifecycle_events.insert("load".into());
    }

    fn on_loading_started(&mut self) {
        self.lifecycle_events.clear();
        self.http_request.take();
    }

    pub fn is_loaded(&self) -> bool {
        self.lifecycle_events.contains("load")
    }

    pub fn clear_contexts(&mut self) {
        self.main_world.take_context();
        self.secondary_world.take_context();
    }

    pub fn destroy_context(&mut self, ctx_unique_id: &str) {
        if self.main_world.execution_context_unique_id() == Some(ctx_unique_id) {
            self.main_world.take_context();
        } else if self.secondary_world.execution_context_unique_id() == Some(ctx_unique_id) {
            self.secondary_world.take_context();
        }
    }

    pub fn execution_context(&self) -> Option<ExecutionContextId> {
        self.main_world.execution_context()
    }

    pub fn set_request(&mut self, request: HttpRequest) {
        self.http_request = Some(Arc::new(request))
    }
}

impl From<CdpFrame> for Frame {
    fn from(frame: CdpFrame) -> Self {
        Self {
            parent_frame: frame.parent_id,
            id: frame.id,
            main_world: Default::default(),
            secondary_world: Default::default(),
            loader_id: Some(frame.loader_id),
            url: Some(frame.url),
            http_request: None,
            child_frames: Default::default(),
            name: frame.name,
            session_id: None,
            security_origin: frame.security_origin,
            lifecycle_events: Default::default(),
        }
    }
}

/// Maintains the state of the pages frame and listens to events produced by
/// chromium targeting the `Target`. Also listens for events that indicate that
/// a navigation was completed
type NavigationKey = (Option<SessionId>, FrameId);

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum NavigationCompletion {
    SameDocument,
    NewDocument,
}

#[derive(Debug)]
struct NavigationEntry {
    watcher: Option<NavigationWatcher>,
    /// Loader captured before an anticipated navigation begins.
    pre_loader_id: Option<LoaderId>,
    deadline: Instant,
    waiters: Vec<OneshotSender<std::result::Result<(), FrameWaitError>>>,
}

/// The `(session, numeric context id)` pair that uniquely identifies a live
/// execution context. Numeric context ids are only unique inside a CDP session,
/// and Chrome reuses them, so both fields are required.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ContextKey {
    session_id: SessionId,
    context_id: ExecutionContextId,
}

/// The frame a [`ContextKey`] currently maps to, tagged with the CDP unique id
/// that created the mapping. The unique id lets a late `executionContextDestroyed`
/// (whose numeric id may already have been reused by another context on the same
/// frame) delete only the mapping it actually created.
#[derive(Debug, Clone)]
struct ContextBinding {
    /// The mapping's payload: the frame owning this `(session, numeric)` key.
    /// Not yet read on any production path (the forward map is write-only in
    /// Phase 1); tests assert it, and Phase 2 context->frame routing will read
    /// it. Kept checked in test builds, silenced in release until a reader lands.
    #[cfg_attr(not(test), allow(dead_code))]
    frame_id: FrameId,
    unique_id: String,
}

/// Reverse-index entry: the frame owning a CDP unique context id, plus the
/// forward `(session, numeric)` key when the context is session-bound. The
/// legacy pre-OOPIF path has no session and therefore no forward key.
#[derive(Debug, Clone)]
struct UniqueContext {
    frame_id: FrameId,
    key: Option<ContextKey>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum IsolatedWorldState {
    Pending,
    Confirmed,
}

#[derive(Debug)]
pub struct FrameManager {
    main_frame: Option<FrameId>,
    frames: HashMap<FrameId, Frame>,
    /// Reverse index: CDP unique context id -> owning frame and, when bound, its
    /// `(session, numeric)` key. Destroy events carry only the unique id, so
    /// this resolves both the frame and the forward key to delete.
    context_by_unique: HashMap<String, UniqueContext>,
    /// Numeric context ids are only unique inside a CDP session. The binding
    /// records the unique id so destroy is idempotent under numeric-id reuse.
    context_to_frame: HashMap<ContextKey, ContextBinding>,
    /// Sessions known to represent OOP child targets.
    session_frames: HashSet<SessionId>,
    /// Stable, full-parameter preload records used for child-session replay.
    preload_scripts: Vec<PreloadScript>,
    /// Tracks in-flight registration and browser-side evidence for named
    /// isolated worlds. `Confirmed` means either the registration ACK arrived
    /// or Chrome emitted a context for that world; observing the context is
    /// monotonic evidence and a later protocol error must not downgrade it.
    /// Current initialization has no production re-trigger after a failure.
    /// Any future recovery path must split registration from context presence,
    /// or add generation-aware late-response attribution before retrying.
    isolated_worlds: HashMap<(SessionId, String), IsolatedWorldState>,
    /// Preserves the old standalone behavior before a main session is bound.
    legacy_isolated_worlds: HashSet<String>,
    main_session_id: Option<SessionId>,
    /// Timeout after which an anticipated event (related to navigation) doesn't
    /// arrive results in an error
    request_timeout: Duration,
    /// Cleanup paths enqueue cross-layer results here; polling always drains
    /// them before scanning ordinary navigation progress.
    queued_events: VecDeque<FrameEvent>,
    /// One active navigation lane per `(session, frame)` pair.
    navigation: HashMap<NavigationKey, NavigationEntry>,
    /// Additional commands wait behind the active lane for their own key.
    pending_navigations:
        HashMap<NavigationKey, VecDeque<(FrameNavigationRequest, NavigationWatcher)>>,
}

impl FrameManager {
    pub fn new(request_timeout: Duration) -> Self {
        FrameManager {
            main_frame: None,
            frames: Default::default(),
            context_by_unique: Default::default(),
            context_to_frame: Default::default(),
            session_frames: Default::default(),
            preload_scripts: Default::default(),
            isolated_worlds: Default::default(),
            legacy_isolated_worlds: Default::default(),
            main_session_id: None,
            request_timeout,
            queued_events: Default::default(),
            pending_navigations: Default::default(),
            navigation: Default::default(),
        }
    }

    /// The commands to execute in order to initialize this frame manager
    pub fn init_commands(timeout: Duration) -> CommandChain {
        let enable = page::EnableParams::default();
        let get_tree = page::GetFrameTreeParams::default();
        let set_lifecycle = page::SetLifecycleEventsEnabledParams::new(true);
        let enable_runtime = runtime::EnableParams::default();
        CommandChain::new(
            vec![
                (enable.identifier(), serde_json::to_value(enable).unwrap()),
                (
                    get_tree.identifier(),
                    serde_json::to_value(get_tree).unwrap(),
                ),
                (
                    set_lifecycle.identifier(),
                    serde_json::to_value(set_lifecycle).unwrap(),
                ),
                (
                    enable_runtime.identifier(),
                    serde_json::to_value(enable_runtime).unwrap(),
                ),
            ],
            timeout,
        )
    }

    pub fn main_frame(&self) -> Option<&Frame> {
        self.main_frame.as_ref().and_then(|id| self.frames.get(id))
    }

    pub fn main_frame_mut(&mut self) -> Option<&mut Frame> {
        if let Some(id) = self.main_frame.as_ref() {
            self.frames.get_mut(id)
        } else {
            None
        }
    }

    pub fn frames(&self) -> impl Iterator<Item = &Frame> + '_ {
        self.frames.values()
    }

    pub fn frame(&self, id: &FrameId) -> Option<&Frame> {
        self.frames.get(id)
    }

    pub(crate) fn set_main_session_id(&mut self, session_id: SessionId) {
        self.main_session_id = Some(session_id);
    }

    pub(crate) fn main_session_id(&self) -> Option<&SessionId> {
        self.main_session_id.as_ref()
    }

    pub(crate) fn is_child_session(&self, session_id: &SessionId) -> bool {
        self.session_frames.contains(session_id)
    }

    pub(crate) fn child_sessions(&self) -> impl Iterator<Item = &SessionId> {
        self.session_frames.iter()
    }

    pub(crate) fn add_preload_script(
        &mut self,
        params: AddScriptToEvaluateOnNewDocumentParams,
        main_id: ScriptIdentifier,
    ) -> PreloadId {
        let id = self.preload_scripts.len();
        self.preload_scripts.push(PreloadScript {
            id,
            params,
            main_id,
            per_session_ids: HashMap::new(),
            state: PreloadState::Live,
        });
        id
    }

    pub(crate) fn preload_snapshot(
        &self,
    ) -> Vec<(PreloadId, AddScriptToEvaluateOnNewDocumentParams)> {
        self.preload_scripts
            .iter()
            .map(|preload| {
                debug_assert_eq!(preload.state, PreloadState::Live);
                (preload.id, preload.params.clone())
            })
            .collect()
    }

    pub(crate) fn set_preload_id(
        &mut self,
        preload_id: PreloadId,
        session_id: SessionId,
        script_id: ScriptIdentifier,
    ) -> bool {
        if !self.session_frames.contains(&session_id) {
            return false;
        }
        let Some(preload) = self.preload_scripts.get_mut(preload_id) else {
            return false;
        };
        match preload.state {
            PreloadState::Live => {
                preload.per_session_ids.insert(session_id, script_id);
                true
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn preload_script(&self, preload_id: PreloadId) -> Option<&PreloadScript> {
        self.preload_scripts.get(preload_id)
    }

    fn check_lifecycle(&self, watcher: &NavigationWatcher, frame: &Frame) -> bool {
        watcher.expected_lifecycle.iter().all(|ev| {
            frame.lifecycle_events.contains(ev)
                || (frame.url.is_none() && frame.lifecycle_events.contains("DOMContentLoaded"))
        }) && frame
            .child_frames
            .iter()
            .filter_map(|f| self.frames.get(f))
            .all(|f| self.check_lifecycle(watcher, f))
    }

    fn check_lifecycle_complete(
        &self,
        watcher: &NavigationWatcher,
        frame: &Frame,
    ) -> Option<NavigationCompletion> {
        if !self.check_lifecycle(watcher, frame) {
            return None;
        }
        if frame.loader_id == watcher.loader_id && !watcher.same_document_navigation {
            return None;
        }
        if watcher.same_document_navigation {
            return Some(NavigationCompletion::SameDocument);
        }
        if frame.loader_id != watcher.loader_id {
            return Some(NavigationCompletion::NewDocument);
        }
        None
    }

    /// Track the request in the frame
    pub fn on_http_request_finished(&mut self, request: HttpRequest) {
        if let Some(id) = request.frame.as_ref() {
            if let Some(frame) = self.frames.get_mut(id) {
                frame.set_request(request);
            }
        }
    }

    pub fn poll(&mut self, now: Instant) -> Option<FrameEvent> {
        if let Some(event) = self.queued_events.pop_front() {
            return Some(event);
        }

        #[derive(Debug)]
        enum ScanOutcome {
            Timeout(Instant),
            FrameMissing(FrameId),
            Complete(NavigationCompletion),
        }

        let navigation_keys = self.navigation.keys().cloned().collect::<Vec<_>>();
        let mut completed = Vec::new();
        for key in navigation_keys {
            let Some(entry) = self.navigation.get(&key) else {
                continue;
            };
            let outcome = if now > entry.deadline {
                Some(ScanOutcome::Timeout(entry.deadline))
            } else if let Some(watcher) = entry.watcher.as_ref() {
                match self.frames.get(&watcher.frame_id) {
                    Some(frame) => self
                        .check_lifecycle_complete(watcher, frame)
                        .map(ScanOutcome::Complete),
                    None => Some(ScanOutcome::FrameMissing(watcher.frame_id.clone())),
                }
            } else {
                None
            };
            if let Some(outcome) = outcome {
                completed.push((key, outcome));
            }
        }

        // Complete every waiter-only entry in this pass. Handler-facing
        // results are queued and returned one at a time after the full scan.
        for (key, outcome) in completed {
            let Some(entry) = self.navigation.remove(&key) else {
                continue;
            };
            match outcome {
                ScanOutcome::Timeout(deadline) => {
                    for waiter in entry.waiters {
                        let _ = waiter.send(Err(FrameWaitError::Timeout));
                    }
                    if let Some(id) = entry.watcher.and_then(|watcher| watcher.id) {
                        self.queued_events
                            .push_back(FrameEvent::NavigationResult(Err(
                                NavigationError::Timeout {
                                    id,
                                    err: DeadlineExceeded::new(now, deadline),
                                },
                            )));
                    }
                }
                ScanOutcome::FrameMissing(frame_id) => {
                    for waiter in entry.waiters {
                        let _ = waiter.send(Err(FrameWaitError::FrameNotFound {
                            frame: frame_id.clone(),
                        }));
                    }
                    if let Some(id) = entry.watcher.and_then(|watcher| watcher.id) {
                        self.queued_events
                            .push_back(FrameEvent::NavigationResult(Err(
                                NavigationError::FrameNotFound {
                                    id,
                                    frame: frame_id,
                                },
                            )));
                    }
                }
                ScanOutcome::Complete(completion) => {
                    for waiter in entry.waiters {
                        let _ = waiter.send(Ok(()));
                    }
                    if let Some(id) = entry.watcher.and_then(|watcher| watcher.id) {
                        let result = match completion {
                            NavigationCompletion::SameDocument => {
                                NavigationOk::SameDocumentNavigation(id)
                            }
                            NavigationCompletion::NewDocument => {
                                NavigationOk::NewDocumentNavigation(id)
                            }
                        };
                        self.queued_events
                            .push_back(FrameEvent::NavigationResult(Ok(result)));
                    }
                }
            }
        }

        if let Some(event) = self.queued_events.pop_front() {
            return Some(event);
        }

        let pending_keys = self.pending_navigations.keys().cloned().collect::<Vec<_>>();
        for key in pending_keys {
            if self
                .navigation
                .get(&key)
                .is_some_and(|entry| entry.watcher.is_some())
            {
                continue;
            }

            let next = self
                .pending_navigations
                .get_mut(&key)
                .and_then(VecDeque::pop_front);
            let Some((req, watcher)) = next else {
                continue;
            };
            if self
                .pending_navigations
                .get(&key)
                .is_some_and(VecDeque::is_empty)
            {
                self.pending_navigations.remove(&key);
            }

            let deadline = now + req.timeout;
            if let Some(entry) = self.navigation.get_mut(&key) {
                entry.watcher = Some(watcher);
                entry.deadline = deadline;
            } else {
                self.navigation.insert(
                    key,
                    NavigationEntry {
                        watcher: Some(watcher),
                        pre_loader_id: None,
                        deadline,
                        waiters: Vec::new(),
                    },
                );
            }
            self.queued_events
                .push_back(FrameEvent::NavigationRequest(req.id, req.req));
        }

        self.queued_events.pop_front()
    }

    /// Entrypoint for page navigation
    pub fn goto(&mut self, req: FrameNavigationRequest) {
        if let Some(frame_id) = self.main_frame.clone() {
            if let Some(session_id) = self.main_session_id.clone() {
                self.navigate_frame_in_session(session_id, frame_id, req);
            } else {
                self.navigate_frame(frame_id, req);
            }
        }
    }

    /// Navigate a specific frame
    pub fn navigate_frame(&mut self, frame_id: FrameId, req: FrameNavigationRequest) {
        let session_id = self
            .frames
            .get(&frame_id)
            .and_then(|frame| frame.session_id().cloned())
            .or_else(|| {
                req.req
                    .session_id
                    .as_ref()
                    .map(|session_id| SessionId::new(session_id.clone()))
            });
        if let Some(session_id) = session_id {
            self.navigate_frame_in_session(session_id, frame_id, req);
        } else {
            self.register_pending(None, frame_id, req);
        }
    }

    pub(crate) fn navigate_frame_in_session(
        &mut self,
        session_id: SessionId,
        frame_id: FrameId,
        mut req: FrameNavigationRequest,
    ) {
        let loader_id = self.frames.get(&frame_id).and_then(|f| f.loader_id.clone());
        req.req.session_id = Some(session_id.clone().into());
        self.register_pending_with_loader(Some(session_id), frame_id, req, loader_id);
    }

    fn register_pending(
        &mut self,
        session_id: Option<SessionId>,
        frame_id: FrameId,
        req: FrameNavigationRequest,
    ) {
        let loader_id = self.frames.get(&frame_id).and_then(|f| f.loader_id.clone());
        self.register_pending_with_loader(session_id, frame_id, req, loader_id);
    }

    fn register_pending_with_loader(
        &mut self,
        session_id: Option<SessionId>,
        frame_id: FrameId,
        mut req: FrameNavigationRequest,
        loader_id: Option<LoaderId>,
    ) {
        self.evict_other_lane(&frame_id, &session_id);
        let watcher = NavigationWatcher::until_page_load(req.id, frame_id.clone(), loader_id);
        req.set_frame_id(frame_id.clone());
        self.pending_navigations
            .entry((session_id, frame_id))
            .or_default()
            .push_back((req, watcher));
    }

    fn keys_for_frame(&self, frame_id: &FrameId) -> Vec<Option<SessionId>> {
        self.navigation
            .keys()
            .chain(self.pending_navigations.keys())
            .filter(|(_, candidate)| candidate == frame_id)
            .map(|(session_id, _)| session_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }

    fn evict_other_lane(&mut self, frame_id: &FrameId, desired_session: &Option<SessionId>) {
        let victims = self
            .keys_for_frame(frame_id)
            .into_iter()
            .filter(|session_id| session_id != desired_session)
            .collect::<Vec<_>>();
        for session_id in victims {
            self.fail_navigation_key(
                &(session_id, frame_id.clone()),
                FrameWaitError::FrameSwappedOrDetached,
            );
        }
    }

    fn fail_navigation_key(&mut self, key: &NavigationKey, waiter_error: FrameWaitError) {
        if let Some(entry) = self.navigation.remove(key) {
            for waiter in entry.waiters {
                let _ = waiter.send(Err(waiter_error.clone()));
            }
            if let Some(watcher) = entry.watcher {
                if let Some(id) = watcher.id {
                    self.queued_events
                        .push_back(FrameEvent::NavigationResult(Err(
                            NavigationError::FrameNotFound {
                                id,
                                frame: watcher.frame_id,
                            },
                        )));
                }
            }
        }
        if let Some(pending) = self.pending_navigations.remove(key) {
            for (_, watcher) in pending {
                if let Some(id) = watcher.id {
                    self.queued_events
                        .push_back(FrameEvent::NavigationResult(Err(
                            NavigationError::FrameNotFound {
                                id,
                                frame: watcher.frame_id,
                            },
                        )));
                }
            }
        }
    }

    /// Settle every navigation lane owned by one CDP session. Calling this at
    /// the draining tombstone boundary prevents waiters and queued navigations
    /// from surviving until the later detach notification.
    pub(crate) fn fail_navigation_state_for_session(
        &mut self,
        session_id: &SessionId,
        waiter_error: FrameWaitError,
    ) {
        let navigation_keys = self
            .navigation
            .keys()
            .chain(self.pending_navigations.keys())
            .filter(|(candidate, _)| candidate.as_ref() == Some(session_id))
            .cloned()
            .collect::<HashSet<_>>();
        for key in navigation_keys {
            self.fail_navigation_key(&key, waiter_error.clone());
        }
    }

    /// Settle every registered navigation waiter during a whole-target or
    /// connection teardown, so a pending [`crate::Frame::wait_for_navigation`]
    /// observes a typed error instead of an opaque channel cancellation.
    ///
    /// Unlike [`Self::fail_navigation_key`], this deliberately does not enqueue
    /// Handler-facing `NavigationResult` events: the explicit `Page::goto` /
    /// `Frame::goto` sender is owned by the Handler's own pending-navigation
    /// bookkeeping, which settles it separately, and this `FrameManager` (with
    /// its `queued_events`) is about to be dropped, so any event pushed here
    /// would have no consumer.
    ///
    /// Scope: this is the whole-target path (target destroyed, main-session
    /// detach, connection close). Child-session draining is settled earlier by
    /// `fail_navigation_state_for_session` at the draining tombstone boundary;
    /// the later detach repeats that per-session sweep as a harmless no-op.
    pub(crate) fn fail_all_navigation_state(&mut self, waiter_error: FrameWaitError) {
        for entry in self.navigation.values_mut() {
            for waiter in entry.waiters.drain(..) {
                let _ = waiter.send(Err(waiter_error.clone()));
            }
        }
        self.navigation.clear();
        self.pending_navigations.clear();
    }

    /// Fired when a frame moved to another session
    pub fn on_attached_to_target(&mut self, event: &EventAttachedToTarget) {
        if self.main_session_id.is_some() {
            self.on_attached_to_target_in_session(event, event.session_id.clone());
        }
    }

    pub(crate) fn on_attached_to_target_in_session(
        &mut self,
        event: &EventAttachedToTarget,
        child_session_id: SessionId,
    ) {
        let frame_id = FrameId::new(event.target_info.target_id.as_ref());
        if !self.frames.contains_key(&frame_id) {
            if let Some(parent_id) = event.target_info.parent_frame_id.clone() {
                let _ = self.on_frame_attached_in_session(
                    frame_id.clone(),
                    Some(parent_id),
                    child_session_id.clone(),
                );
            }
        }
        if self.frames.contains_key(&frame_id) {
            self.rebind_frame_session(&frame_id, child_session_id.clone());
        }
        if self.main_session_id.as_ref() != Some(&child_session_id) {
            self.session_frames.insert(child_session_id);
        }
    }

    pub fn on_frame_tree(&mut self, frame_tree: FrameTree) {
        if let Some(session_id) = self.main_session_id.clone() {
            let _ = self.on_frame_tree_in_session(frame_tree, session_id);
        } else {
            self.on_frame_tree_core(frame_tree);
        }
    }

    fn on_frame_tree_core(&mut self, frame_tree: FrameTree) {
        self.on_frame_attached_core(
            frame_tree.frame.id.clone(),
            frame_tree.frame.parent_id.clone(),
            None,
        );
        self.on_frame_navigated_core(&frame_tree.frame, None);
        if let Some(children) = frame_tree.child_frames {
            for child_tree in children {
                self.on_frame_tree_core(child_tree);
            }
        }
    }

    pub(crate) fn on_frame_tree_in_session(
        &mut self,
        frame_tree: FrameTree,
        session_id: SessionId,
    ) -> Vec<SessionId> {
        let mut swapped_sessions = Vec::new();
        self.on_frame_tree_in_session_inner(frame_tree, &session_id, &mut swapped_sessions);
        swapped_sessions
    }

    fn on_frame_tree_in_session_inner(
        &mut self,
        frame_tree: FrameTree,
        session_id: &SessionId,
        swapped_sessions: &mut Vec<SessionId>,
    ) {
        if let Some(old_session_id) = self.on_frame_attached_in_session(
            frame_tree.frame.id.clone(),
            frame_tree.frame.parent_id.clone(),
            session_id.clone(),
        ) {
            swapped_sessions.push(old_session_id);
        }
        self.on_frame_navigated_in_session(&frame_tree.frame, session_id.clone());
        if let Some(children) = frame_tree.child_frames {
            for child_tree in children {
                self.on_frame_tree_in_session_inner(child_tree, session_id, swapped_sessions);
            }
        }
    }

    pub fn on_frame_attached(&mut self, frame_id: FrameId, parent_frame_id: Option<FrameId>) {
        if let Some(session_id) = self.main_session_id.clone() {
            let _ = self.on_frame_attached_in_session(frame_id, parent_frame_id, session_id);
        } else {
            self.on_frame_attached_core(frame_id, parent_frame_id, None);
        }
    }

    pub(crate) fn on_frame_attached_in_session(
        &mut self,
        frame_id: FrameId,
        parent_frame_id: Option<FrameId>,
        session_id: SessionId,
    ) -> Option<SessionId> {
        self.on_frame_attached_core(frame_id, parent_frame_id, Some(&session_id))
    }

    fn on_frame_attached_core(
        &mut self,
        frame_id: FrameId,
        parent_frame_id: Option<FrameId>,
        event_session_id: Option<&SessionId>,
    ) -> Option<SessionId> {
        if self.frames.contains_key(&frame_id) {
            let swap_back = match (event_session_id, parent_frame_id.as_ref()) {
                (Some(event_session_id), Some(parent_frame_id)) => {
                    let parent_session_id =
                        self.frames.get(parent_frame_id).and_then(Frame::session_id);
                    let frame = self.frames.get(&frame_id);
                    let main_session_id = self.main_session_id.as_ref();
                    frame.is_some_and(|frame| {
                        parent_session_id == Some(event_session_id)
                            && main_session_id.is_some_and(|main| frame.is_out_of_process(main))
                            && frame.session_id() != Some(event_session_id)
                    })
                }
                (Some(_), None) | (None, Some(_)) | (None, None) => false,
            };

            if swap_back {
                let old_session_id = self
                    .frames
                    .get(&frame_id)
                    .and_then(Frame::session_id)
                    .cloned()
                    .expect("a swapped OOP frame has a bound session");
                self.clear_frame_contexts_for_session(&frame_id, &old_session_id);
                self.rebind_frame_session(
                    &frame_id,
                    event_session_id
                        .expect("swap-back requires an event session")
                        .clone(),
                );
                return Some(old_session_id);
            }

            if let Some(event_session_id) = event_session_id {
                if self
                    .frames
                    .get(&frame_id)
                    .is_some_and(|frame| frame.session_id().is_none())
                {
                    self.rebind_frame_session(&frame_id, event_session_id.clone());
                }
            }
            return None;
        }

        if let Some(parent_frame_id) = parent_frame_id {
            if let Some(parent_frame) = self.frames.get_mut(&parent_frame_id) {
                let mut frame = Frame::with_parent(frame_id.clone(), parent_frame);
                if let Some(event_session_id) = event_session_id {
                    frame.set_session_id(event_session_id.clone());
                    if self.main_session_id.as_ref() != Some(event_session_id) {
                        self.session_frames.insert(event_session_id.clone());
                    }
                }
                self.frames.insert(frame_id, frame);
            }
        }
        None
    }

    pub fn on_frame_detached(&mut self, event: &EventFrameDetached) {
        match event.reason {
            FrameDetachedReason::Swap => self.clear_frame_contexts(&event.frame_id),
            FrameDetachedReason::Remove => {
                self.remove_frames_recursively(&event.frame_id);
            }
        }
    }

    pub fn on_frame_navigated(&mut self, frame: &CdpFrame) {
        if let Some(session_id) = self.main_session_id.clone() {
            self.on_frame_navigated_in_session(frame, session_id);
        } else {
            self.on_frame_navigated_core(frame, None);
        }
    }

    pub(crate) fn on_frame_navigated_in_session(
        &mut self,
        frame: &CdpFrame,
        session_id: SessionId,
    ) {
        self.on_frame_navigated_core(frame, Some(&session_id));
    }

    fn on_frame_navigated_core(&mut self, frame: &CdpFrame, session_id: Option<&SessionId>) {
        if frame.parent_id.is_some() {
            if let Some((id, mut f)) = self.frames.remove_entry(&frame.id) {
                let same_session = session_id
                    .map(|session_id| f.session_id() == Some(session_id))
                    .unwrap_or(true);
                if same_session {
                    let child_frames = f.child_frames.iter().cloned().collect::<Vec<_>>();
                    for child in child_frames {
                        self.remove_frames_recursively(&child);
                    }
                    f.child_frames.clear();
                }
                f.navigated(frame);
                self.frames.insert(id, f);
            }
        } else {
            let mut f = if let Some(main) = self.main_frame.take() {
                let mut main_frame = self
                    .frames
                    .remove(&main)
                    .unwrap_or_else(|| Frame::new(frame.id.clone()));
                let child_frames = main_frame.child_frames.iter().cloned().collect::<Vec<_>>();
                for child in child_frames {
                    self.remove_frames_recursively(&child);
                }
                main_frame.child_frames.clear();
                main_frame.id = frame.id.clone();
                main_frame
            } else {
                Frame::new(frame.id.clone())
            };
            if let Some(session_id) = session_id {
                f.set_session_id(session_id.clone());
            }
            f.navigated(frame);
            self.main_frame = Some(f.id.clone());
            self.frames.insert(f.id.clone(), f);
        }

        if let Some(session_id) = session_id {
            let key = (Some(session_id.clone()), frame.id.clone());
            if self
                .frames
                .get(&frame.id)
                .is_some_and(|tracked| tracked.session_id() == Some(session_id))
            {
                if let Some(entry) = self.navigation.get_mut(&key) {
                    if entry.watcher.is_none() {
                        entry.watcher = Some(NavigationWatcher::anticipated_page_load(
                            frame.id.clone(),
                            entry.pre_loader_id.clone(),
                        ));
                    }
                }
            }
        }
    }

    pub fn on_frame_navigated_within_document(&mut self, event: &EventNavigatedWithinDocument) {
        if let Some(session_id) = self.main_session_id.clone() {
            self.on_frame_navigated_within_document_in_session(event, session_id);
        } else {
            self.on_frame_navigated_within_document_core(event, None);
        }
    }

    pub(crate) fn on_frame_navigated_within_document_in_session(
        &mut self,
        event: &EventNavigatedWithinDocument,
        session_id: SessionId,
    ) {
        self.on_frame_navigated_within_document_core(event, Some(&session_id));
    }

    fn on_frame_navigated_within_document_core(
        &mut self,
        event: &EventNavigatedWithinDocument,
        session_id: Option<&SessionId>,
    ) {
        if let Some(frame) = self.frames.get_mut(&event.frame_id) {
            let belongs_to_session = session_id
                .map(|session_id| frame.session_id() == Some(session_id))
                .unwrap_or(true);
            if belongs_to_session {
                frame.navigated_within_url(event.url.clone());
            }
        }
        let key = (session_id.cloned(), event.frame_id.clone());
        if let Some(entry) = self.navigation.get_mut(&key) {
            if let Some(watcher) = entry.watcher.as_mut() {
                watcher.on_frame_navigated_within_document(event);
            }
        }
    }

    pub fn on_frame_stopped_loading(&mut self, event: &EventFrameStoppedLoading) {
        if let Some(session_id) = self.main_session_id.clone() {
            self.on_frame_stopped_loading_in_session(event, session_id);
        } else {
            self.on_frame_stopped_loading_core(event, None);
        }
    }

    pub(crate) fn on_frame_stopped_loading_in_session(
        &mut self,
        event: &EventFrameStoppedLoading,
        session_id: SessionId,
    ) {
        self.on_frame_stopped_loading_core(event, Some(&session_id));
    }

    fn on_frame_stopped_loading_core(
        &mut self,
        event: &EventFrameStoppedLoading,
        session_id: Option<&SessionId>,
    ) {
        if let Some(frame) = self.frames.get_mut(&event.frame_id) {
            if session_id
                .map(|session_id| frame.session_id() == Some(session_id))
                .unwrap_or(true)
            {
                frame.on_loading_stopped();
            }
        }
    }

    /// Fired when frame has started loading.
    pub fn on_frame_started_loading(&mut self, event: &EventFrameStartedLoading) {
        if let Some(session_id) = self.main_session_id.clone() {
            self.on_frame_started_loading_in_session(event, session_id);
        } else {
            self.on_frame_started_loading_core(event, None);
        }
    }

    pub(crate) fn on_frame_started_loading_in_session(
        &mut self,
        event: &EventFrameStartedLoading,
        session_id: SessionId,
    ) {
        self.on_frame_started_loading_core(event, Some(&session_id));
    }

    fn on_frame_started_loading_core(
        &mut self,
        event: &EventFrameStartedLoading,
        session_id: Option<&SessionId>,
    ) {
        if let Some(frame) = self.frames.get_mut(&event.frame_id) {
            if session_id
                .map(|session_id| frame.session_id() == Some(session_id))
                .unwrap_or(true)
            {
                frame.on_loading_started();
            }
        }
    }

    /// Notification is issued every time when binding is called
    pub fn on_runtime_binding_called(&mut self, event: &EventBindingCalled) {
        if let Some(session_id) = self.main_session_id.clone() {
            self.on_runtime_binding_called_in_session(event, session_id);
        }
    }

    pub(crate) fn on_runtime_binding_called_in_session(
        &mut self,
        _event: &EventBindingCalled,
        _session_id: SessionId,
    ) {
        // Binding delivery is intentionally inert until cross-frame binding
        // propagation is implemented. Keeping the session-aware entry point
        // prevents future code from guessing a context's owner globally.
    }

    /// Issued when new execution context is created
    pub fn on_frame_execution_context_created(&mut self, event: &EventExecutionContextCreated) {
        if let Some(session_id) = self.main_session_id.clone() {
            self.on_frame_execution_context_created_in_session(event, session_id);
        } else {
            self.on_frame_execution_context_created_core(event, None);
        }
    }

    /// Record a newly created execution context in both context indices. The
    /// forward `context_to_frame` map only exists for session-bound contexts;
    /// the reverse `context_by_unique` map always records the frame so destroy
    /// works on the legacy path too.
    fn insert_context(
        &mut self,
        session_id: Option<&SessionId>,
        context_id: ExecutionContextId,
        unique_id: String,
        frame_id: FrameId,
    ) {
        let key = session_id.map(|session_id| ContextKey {
            session_id: session_id.clone(),
            context_id,
        });
        if let Some(key) = key.clone() {
            self.context_to_frame.insert(
                key,
                ContextBinding {
                    frame_id: frame_id.clone(),
                    unique_id: unique_id.clone(),
                },
            );
        }
        self.context_by_unique
            .insert(unique_id, UniqueContext { frame_id, key });
    }

    /// Remove the context a `executionContextDestroyed` names, keyed by its CDP
    /// unique id. The forward map is only deleted when its binding still carries
    /// this exact unique id, so a late destroy cannot evict a mapping whose
    /// numeric id was already reused by a newer context. Returns the owning
    /// frame id, if the context was tracked.
    fn remove_context_by_unique(&mut self, unique_id: &str) -> Option<FrameId> {
        let context = self.context_by_unique.remove(unique_id)?;
        if let Some(key) = context.key {
            let still_ours = self
                .context_to_frame
                .get(&key)
                .is_some_and(|binding| binding.unique_id == unique_id);
            if still_ours {
                self.context_to_frame.remove(&key);
            }
        }
        Some(context.frame_id)
    }

    /// Bulk-remove contexts through the single owning index, keeping both maps
    /// consistent. The context is dropped when `keep` returns false; `keep`
    /// receives the owning frame id and the forward key (absent on the legacy
    /// path). All bulk clearing paths funnel through here so the two indices
    /// cannot drift.
    ///
    /// `keep` must be a pure, side-effect-free predicate: it is invoked once per
    /// reverse entry and may be evaluated for entries in any order.
    ///
    /// The forward map is driven entirely by the reverse map: a `context_to_frame`
    /// entry is removed only when the reverse entry that owns its exact `unique_id`
    /// is being dropped. This preserves the same numeric-id-reuse protection as
    /// [`Self::remove_context_by_unique`] — if a newer context reused this
    /// `(session, numeric)` key, its forward binding carries a different
    /// `unique_id` and is left intact. `insert_context` always writes both maps
    /// together, so every forward entry has a matching reverse entry; no separate
    /// forward scan is needed (and one would mis-delete the reused key).
    fn retain_contexts(&mut self, mut keep: impl FnMut(&FrameId, Option<&ContextKey>) -> bool) {
        let mut removed = Vec::new();
        self.context_by_unique.retain(|unique_id, context| {
            let keep = keep(&context.frame_id, context.key.as_ref());
            if !keep {
                if let Some(key) = context.key.clone() {
                    removed.push((key, unique_id.clone()));
                }
            }
            keep
        });
        for (key, unique_id) in removed {
            let still_ours = self
                .context_to_frame
                .get(&key)
                .is_some_and(|binding| binding.unique_id == unique_id);
            if still_ours {
                self.context_to_frame.remove(&key);
            }
        }
    }

    pub(crate) fn on_frame_execution_context_created_in_session(
        &mut self,
        event: &EventExecutionContextCreated,
        session_id: SessionId,
    ) {
        self.on_frame_execution_context_created_core(event, Some(&session_id));
    }

    fn on_frame_execution_context_created_core(
        &mut self,
        event: &EventExecutionContextCreated,
        session_id: Option<&SessionId>,
    ) {
        if let Some(frame_id) = event
            .context
            .aux_data
            .as_ref()
            .and_then(|v| v["frameId"].as_str())
        {
            if let Some(frame) = self.frames.get_mut(frame_id) {
                if session_id
                    .map(|session_id| frame.session_id() != Some(session_id))
                    .unwrap_or(false)
                {
                    return;
                }
                if event
                    .context
                    .aux_data
                    .as_ref()
                    .and_then(|v| v["isDefault"].as_bool())
                    .unwrap_or_default()
                {
                    frame
                        .main_world
                        .set_context(event.context.id, event.context.unique_id.clone());
                } else if event.context.name == UTILITY_WORLD_NAME
                    && frame.secondary_world.execution_context().is_none()
                {
                    frame
                        .secondary_world
                        .set_context(event.context.id, event.context.unique_id.clone());
                }
                let frame_id = frame.id.clone();
                self.insert_context(
                    session_id,
                    event.context.id,
                    event.context.unique_id.clone(),
                    frame_id,
                );
            }
        }
        if !event.context.name.is_empty()
            && event
                .context
                .aux_data
                .as_ref()
                .is_some_and(|v| v["type"].as_str() == Some("isolated"))
        {
            // Context observation is stronger browser-side evidence than an
            // ACK and can confirm a world without a preceding Pending marker.
            // This intentionally records every named isolated world Chrome
            // exposes, including extension/user worlds not created by
            // `ensure_*`. The real name keeps those keys independent from the
            // utility world, and owning-session teardown bounds their lifetime.
            if let Some(session_id) = session_id {
                self.isolated_worlds.insert(
                    (session_id.clone(), event.context.name.clone()),
                    IsolatedWorldState::Confirmed,
                );
            } else {
                self.legacy_isolated_worlds
                    .insert(event.context.name.clone());
            }
        }
    }

    /// Issued when execution context is destroyed
    pub fn on_frame_execution_context_destroyed(&mut self, event: &EventExecutionContextDestroyed) {
        if let Some(session_id) = self.main_session_id.clone() {
            self.on_frame_execution_context_destroyed_in_session(event, session_id);
        } else {
            self.on_frame_execution_context_destroyed_core(event, None);
        }
    }

    pub(crate) fn on_frame_execution_context_destroyed_in_session(
        &mut self,
        event: &EventExecutionContextDestroyed,
        session_id: SessionId,
    ) {
        self.on_frame_execution_context_destroyed_core(event, Some(&session_id));
    }

    fn on_frame_execution_context_destroyed_core(
        &mut self,
        event: &EventExecutionContextDestroyed,
        session_id: Option<&SessionId>,
    ) {
        let frame_id = self
            .context_by_unique
            .get(&event.execution_context_unique_id)
            .map(|context| context.frame_id.clone());
        let Some(frame_id) = frame_id else {
            return;
        };
        // A cross-session destroy must not evict a context owned by a frame that
        // currently lives in another session.
        if session_id.is_some_and(|session_id| {
            self.frames
                .get(&frame_id)
                .is_some_and(|frame| frame.session_id() != Some(session_id))
        }) {
            return;
        }

        // Removing by unique id keeps the numeric-keyed forward map consistent
        // even when Chrome has already reused this numeric id for another context.
        self.remove_context_by_unique(&event.execution_context_unique_id);
        if let Some(frame) = self.frames.get_mut(&frame_id) {
            frame.destroy_context(&event.execution_context_unique_id);
        }
    }

    /// Issued when all executionContexts were cleared
    pub fn on_execution_contexts_cleared(&mut self) {
        if let Some(session_id) = self.main_session_id.clone() {
            self.on_execution_contexts_cleared_in_session(session_id);
        } else {
            for frame in self.frames.values_mut() {
                frame.clear_contexts();
            }
            self.context_by_unique.clear();
            self.context_to_frame.clear();
        }
    }

    pub(crate) fn on_execution_contexts_cleared_in_session(&mut self, session_id: SessionId) {
        let frame_ids = self
            .frames
            .iter()
            .filter(|(_, frame)| frame.session_id() == Some(&session_id))
            .map(|(frame_id, _)| frame_id.clone())
            .collect::<HashSet<_>>();
        for frame_id in &frame_ids {
            if let Some(frame) = self.frames.get_mut(frame_id) {
                frame.clear_contexts();
            }
        }
        self.retain_contexts(|frame_id, key| {
            // Drop contexts owned by a cleared frame, and any context bound to
            // the cleared session (keeps the forward map free of the session).
            !frame_ids.contains(frame_id) && key.is_none_or(|key| key.session_id != session_id)
        });
    }

    /// Fired for top level page lifecycle events (nav, load, paint, etc.)
    pub fn on_page_lifecycle_event(&mut self, event: &EventLifecycleEvent) {
        if let Some(session_id) = self.main_session_id.clone() {
            self.on_page_lifecycle_event_in_session(event, session_id);
        } else {
            self.on_page_lifecycle_event_core(event, None);
        }
    }

    pub(crate) fn on_page_lifecycle_event_in_session(
        &mut self,
        event: &EventLifecycleEvent,
        session_id: SessionId,
    ) {
        self.on_page_lifecycle_event_core(event, Some(&session_id));
    }

    fn on_page_lifecycle_event_core(
        &mut self,
        event: &EventLifecycleEvent,
        session_id: Option<&SessionId>,
    ) {
        if let Some(frame) = self.frames.get_mut(&event.frame_id) {
            if session_id
                .map(|session_id| frame.session_id() == Some(session_id))
                .unwrap_or(true)
            {
                if event.name == "init" {
                    frame.loader_id = Some(event.loader_id.clone());
                    frame.lifecycle_events.clear();
                }
                frame.lifecycle_events.insert(event.name.clone().into());
            }
        }
    }

    /// Detach all child frames
    fn remove_frames_recursively(&mut self, id: &FrameId) -> Option<Frame> {
        if let Some(mut frame) = self.frames.remove(id) {
            let children = frame.child_frames.iter().cloned().collect::<Vec<_>>();
            for child in children {
                self.remove_frames_recursively(&child);
            }
            self.retain_contexts(|frame_id, _| frame_id != id);
            let navigation_keys = self
                .keys_for_frame(id)
                .into_iter()
                .map(|session_id| (session_id, id.clone()))
                .collect::<Vec<_>>();
            for key in navigation_keys {
                self.fail_navigation_key(&key, FrameWaitError::FrameNotFound { frame: id.clone() });
            }
            if let Some(parent_id) = frame.parent_frame.take() {
                if let Some(parent) = self.frames.get_mut(&parent_id) {
                    parent.child_frames.remove(&frame.id);
                }
            }
            Some(frame)
        } else {
            None
        }
    }

    fn clear_frame_contexts(&mut self, frame_id: &FrameId) {
        self.retain_contexts(|mapped_frame_id, _| mapped_frame_id != frame_id);
        if let Some(frame) = self.frames.get_mut(frame_id) {
            frame.clear_contexts();
        }
    }

    fn clear_frame_contexts_for_session(&mut self, frame_id: &FrameId, session_id: &SessionId) {
        self.retain_contexts(|mapped_frame_id, key| {
            mapped_frame_id != frame_id || key.is_none_or(|key| &key.session_id != session_id)
        });
        if let Some(frame) = self.frames.get_mut(frame_id) {
            frame.clear_contexts();
        }
    }

    fn rebind_frame_session(&mut self, frame_id: &FrameId, session_id: SessionId) {
        self.evict_other_lane(frame_id, &Some(session_id.clone()));
        self.clear_frame_contexts(frame_id);
        if let Some(frame) = self.frames.get_mut(frame_id) {
            frame.set_session_id(session_id);
        }
    }

    pub(crate) fn on_detached_from_target(
        &mut self,
        child_session_id: &SessionId,
        parent_session_id: &SessionId,
    ) {
        let child_frame_ids = self
            .frames
            .iter()
            .filter(|(_, frame)| frame.session_id() == Some(child_session_id))
            .map(|(frame_id, _)| frame_id.clone())
            .collect::<HashSet<_>>();

        self.retain_contexts(|frame_id, key| {
            !child_frame_ids.contains(frame_id)
                && key.is_none_or(|key| &key.session_id != child_session_id)
        });

        self.fail_navigation_state_for_session(
            child_session_id,
            FrameWaitError::FrameSwappedOrDetached,
        );

        self.isolated_worlds
            .retain(|(session_id, _), _| session_id != child_session_id);
        for preload in &mut self.preload_scripts {
            preload.per_session_ids.remove(child_session_id);
        }
        for frame_id in child_frame_ids {
            self.rebind_frame_session(&frame_id, parent_session_id.clone());
        }
        self.session_frames.remove(child_session_id);
    }

    pub(crate) fn wait_for_navigation(
        &mut self,
        session_id: SessionId,
        frame_id: FrameId,
        tx: OneshotSender<std::result::Result<(), FrameWaitError>>,
    ) {
        let Some(frame) = self.frames.get(&frame_id) else {
            let _ = tx.send(Err(FrameWaitError::FrameNotFound { frame: frame_id }));
            return;
        };
        if frame.session_id() != Some(&session_id) {
            let _ = tx.send(Err(FrameWaitError::FrameSwappedOrDetached));
            return;
        }
        let pre_loader_id = frame.loader_id.clone();
        let key = (Some(session_id), frame_id.clone());
        self.evict_other_lane(&frame_id, &key.0);
        if let Some(entry) = self.navigation.get_mut(&key) {
            entry.waiters.push(tx);
        } else {
            self.navigation.insert(
                key,
                NavigationEntry {
                    watcher: None,
                    pre_loader_id,
                    deadline: Instant::now() + self.request_timeout,
                    waiters: vec![tx],
                },
            );
        }
    }

    pub(crate) fn fail_navigation_by_nav_id(
        &mut self,
        navigation_id: NavigationId,
        error_text: String,
    ) {
        let key = self.navigation.iter().find_map(|(key, entry)| {
            entry
                .watcher
                .as_ref()
                .is_some_and(|watcher| watcher.id == Some(navigation_id))
                .then(|| key.clone())
        });
        if let Some(key) = key {
            if let Some(entry) = self.navigation.remove(&key) {
                for waiter in entry.waiters {
                    let _ = waiter.send(Err(FrameWaitError::NavigationFailed(error_text.clone())));
                }
            }
        }
    }

    pub fn ensure_isolated_world(&mut self, world_name: &str) -> Option<CommandChain> {
        if let Some(session_id) = self.main_session_id.clone() {
            self.ensure_isolated_world_in_session(world_name, session_id)
        } else {
            self.ensure_isolated_world_core(world_name, None)
        }
    }

    pub(crate) fn ensure_isolated_world_in_session(
        &mut self,
        world_name: &str,
        session_id: SessionId,
    ) -> Option<CommandChain> {
        self.ensure_isolated_world_core(world_name, Some(&session_id))
    }

    /// Install the world for the document that will commit after a paused
    /// child target resumes. Creating the world explicitly while that target
    /// is paused can wait forever for a document that has not committed yet.
    pub(crate) fn ensure_isolated_world_on_next_document_in_session(
        &mut self,
        world_name: &str,
        session_id: SessionId,
    ) -> Option<CommandChain> {
        match self
            .isolated_worlds
            .entry((session_id, world_name.to_owned()))
        {
            std::collections::hash_map::Entry::Occupied(_) => return None,
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(IsolatedWorldState::Pending);
            }
        }
        let command = AddScriptToEvaluateOnNewDocumentParams::builder()
            .source(format!("//# sourceURL={EVALUATION_SCRIPT_URL}"))
            .world_name(world_name)
            .build()
            .expect("isolated-world script parameters are complete");
        Some(CommandChain::new(
            vec![(
                command.identifier(),
                serde_json::to_value(command).expect("isolated-world script should serialize"),
            )],
            self.request_timeout,
        ))
    }

    fn ensure_isolated_world_core(
        &mut self,
        world_name: &str,
        session_id: Option<&SessionId>,
    ) -> Option<CommandChain> {
        let already_registered = if let Some(session_id) = session_id {
            match self
                .isolated_worlds
                .entry((session_id.clone(), world_name.to_owned()))
            {
                std::collections::hash_map::Entry::Occupied(_) => true,
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(IsolatedWorldState::Pending);
                    false
                }
            }
        } else {
            !self.legacy_isolated_worlds.insert(world_name.to_owned())
        };
        if already_registered {
            return None;
        }

        let cmd = AddScriptToEvaluateOnNewDocumentParams::builder()
            .source(format!("//# sourceURL={EVALUATION_SCRIPT_URL}"))
            .world_name(world_name)
            .build()
            .unwrap();

        let mut cmds = Vec::with_capacity(self.frames.len() + 1);

        cmds.push((cmd.identifier(), serde_json::to_value(cmd).unwrap()));

        cmds.extend(self.frames.iter().filter_map(|(id, frame)| {
            if session_id
                .map(|session_id| frame.session_id() != Some(session_id))
                .unwrap_or(false)
            {
                return None;
            }
            let cmd = CreateIsolatedWorldParams::builder()
                .frame_id(id.clone())
                .grant_univeral_access(true)
                .world_name(world_name)
                .build()
                .unwrap();
            Some((cmd.identifier(), serde_json::to_value(cmd).unwrap()))
        }));
        Some(CommandChain::new(cmds, self.request_timeout))
    }

    pub(crate) fn settle_isolated_world_registration(
        &mut self,
        session_id: &SessionId,
        world_name: &str,
        succeeded: bool,
    ) {
        let key = (session_id.clone(), world_name.to_owned());
        if succeeded {
            if let Some(state) = self.isolated_worlds.get_mut(&key) {
                if *state == IsolatedWorldState::Pending {
                    *state = IsolatedWorldState::Confirmed;
                }
            }
            // A missing entry, whether never tracked or removed by
            // owning-session teardown, is not recreated by a late success.
        } else if self
            .isolated_worlds
            .get(&key)
            .is_some_and(|state| *state == IsolatedWorldState::Pending)
        {
            // An explicit protocol failure removes local registration state.
            // It cannot override a context Chrome has already exposed.
            self.isolated_worlds.remove(&key);
        }
    }

    /// Drop named-world registration bookkeeping when its owning target is gone.
    pub(crate) fn clear_isolated_world_registrations(&mut self) {
        self.isolated_worlds.clear();
    }
}

#[cfg(test)]
impl FrameManager {
    pub(crate) fn test_set_frame_parent(&mut self, frame_id: &FrameId, parent_id: Option<FrameId>) {
        self.frames
            .get_mut(frame_id)
            .expect("test frame exists")
            .parent_frame = parent_id;
    }

    pub(crate) fn test_unbind_frame(&mut self, frame_id: &FrameId) {
        self.frames
            .get_mut(frame_id)
            .expect("test frame exists")
            .session_id = None;
    }

    pub(crate) fn isolated_world_state(
        &self,
        session_id: &SessionId,
        world_name: &str,
    ) -> Option<IsolatedWorldState> {
        self.isolated_worlds
            .get(&(session_id.clone(), world_name.to_owned()))
            .copied()
    }

    pub(crate) fn test_remove_frame_without_descendants(&mut self, frame_id: &FrameId) {
        self.frames.remove(frame_id).expect("test frame exists");
    }
}

#[derive(Debug)]
pub enum FrameEvent {
    /// A previously submitted navigation has finished
    NavigationResult(std::result::Result<NavigationOk, NavigationError>),
    /// A new navigation request needs to be submitted
    NavigationRequest(NavigationId, Request),
    /* /// The initial page of the target has been loaded
     * InitialPageLoadFinished */
}

#[derive(Debug)]
pub enum NavigationError {
    Timeout {
        id: NavigationId,
        err: DeadlineExceeded,
    },
    FrameNotFound {
        id: NavigationId,
        frame: FrameId,
    },
}

impl NavigationError {
    pub fn navigation_id(&self) -> &NavigationId {
        match self {
            NavigationError::Timeout { id, .. } => id,
            NavigationError::FrameNotFound { id, .. } => id,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum NavigationOk {
    SameDocumentNavigation(NavigationId),
    NewDocumentNavigation(NavigationId),
}

impl NavigationOk {
    pub fn navigation_id(&self) -> &NavigationId {
        match self {
            NavigationOk::SameDocumentNavigation(id) => id,
            NavigationOk::NewDocumentNavigation(id) => id,
        }
    }
}

/// Tracks the progress of an issued `Page.navigate` request until completion.
#[derive(Debug)]
pub struct NavigationWatcher {
    /// `None` denotes an anticipated waiter registered before a concrete
    /// `Page.navigate` command exists.
    id: Option<NavigationId>,
    expected_lifecycle: HashSet<MethodId>,
    frame_id: FrameId,
    loader_id: Option<LoaderId>,
    /// Once we receive the response to the issued `Page.navigate` request we
    /// can detect whether we were navigating withing the same document or were
    /// navigating to a new document by checking if a loader was included in the
    /// response.
    same_document_navigation: bool,
}

impl NavigationWatcher {
    pub fn until_page_load(id: NavigationId, frame: FrameId, loader_id: Option<LoaderId>) -> Self {
        Self::new(Some(id), frame, loader_id)
    }

    fn anticipated_page_load(frame: FrameId, loader_id: Option<LoaderId>) -> Self {
        Self::new(None, frame, loader_id)
    }

    fn new(id: Option<NavigationId>, frame: FrameId, loader_id: Option<LoaderId>) -> Self {
        Self {
            id,
            expected_lifecycle: std::iter::once("load".into()).collect(),
            loader_id,
            frame_id: frame,
            same_document_navigation: false,
        }
    }

    /// Checks whether the navigation was completed
    pub fn is_lifecycle_complete(&self) -> bool {
        self.expected_lifecycle.is_empty()
    }

    fn on_frame_navigated_within_document(&mut self, ev: &EventNavigatedWithinDocument) {
        if self.frame_id == ev.frame_id {
            self.same_document_navigation = true;
        }
    }
}

/// An identifier for an ongoing navigation
#[derive(Debug, Copy, Clone, Hash, Eq, PartialEq)]
pub struct NavigationId(pub usize);

/// Stable index of a preload script tracked for replay into child sessions.
///
/// Entries are not recycled, so an identifier never changes meaning while the
/// target is alive.
pub(crate) type PreloadId = usize;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum PreloadState {
    Live,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PreloadScript {
    pub(crate) id: PreloadId,
    pub(crate) params: AddScriptToEvaluateOnNewDocumentParams,
    #[allow(dead_code)]
    pub(crate) main_id: ScriptIdentifier,
    pub(crate) per_session_ids: HashMap<SessionId, ScriptIdentifier>,
    state: PreloadState,
}

/// Why a frame-scoped navigation waiter stopped waiting.
///
/// These errors intentionally do not carry a [`NavigationId`]: anticipated
/// waiters can be registered before a concrete navigation command exists.
#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
#[allow(dead_code)]
pub(crate) enum FrameWaitError {
    #[error("The frame navigation waiter timed out.")]
    Timeout,
    #[error("The frame changed sessions or was detached while waiting.")]
    FrameSwappedOrDetached,
    #[error("FrameId {frame:?} not found.")]
    FrameNotFound { frame: FrameId },
    #[error("The frame navigation failed: {0}")]
    NavigationFailed(String),
}

/// Represents a the request for a navigation
#[derive(Debug)]
pub struct FrameNavigationRequest {
    /// The internal identifier
    pub id: NavigationId,
    /// the cdp request that will trigger the navigation
    pub req: Request,
    /// The timeout after which the request will be considered timed out
    pub timeout: Duration,
}

impl FrameNavigationRequest {
    pub fn new(id: NavigationId, req: Request) -> Self {
        Self {
            id,
            req,
            timeout: Duration::from_millis(REQUEST_TIMEOUT),
        }
    }

    /// This will set the id of the frame into the `params` `frameId` field.
    pub fn set_frame_id(&mut self, frame_id: FrameId) {
        if let Some(params) = self.req.params.as_object_mut() {
            if let Entry::Vacant(entry) = params.entry("frameId") {
                entry.insert(serde_json::Value::String(frame_id.into()));
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LifecycleEvent {
    #[default]
    Load,
    DomcontentLoaded,
    NetworkIdle,
    NetworkAlmostIdle,
}

impl AsRef<str> for LifecycleEvent {
    fn as_ref(&self) -> &str {
        match self {
            LifecycleEvent::Load => "load",
            LifecycleEvent::DomcontentLoaded => "DOMContentLoaded",
            LifecycleEvent::NetworkIdle => "networkIdle",
            LifecycleEvent::NetworkAlmostIdle => "networkAlmostIdle",
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::channel::oneshot;
    use futures::executor::block_on;
    use serde_json::json;

    use chromiumoxide_cdp::cdp::browser_protocol::page::{
        CrossOriginIsolatedContextType, GatedApiFeatures, NavigatedWithinDocumentNavigationType,
        SecureContextType,
    };
    use chromiumoxide_cdp::cdp::browser_protocol::target::{TargetId, TargetInfo};
    use chromiumoxide_cdp::cdp::js_protocol::runtime::ExecutionContextDescription;

    use super::*;

    fn session(id: &str) -> SessionId {
        SessionId::new(id)
    }

    fn ctx_key(session_id: &SessionId, context_id: i64) -> ContextKey {
        ContextKey {
            session_id: session_id.clone(),
            context_id: ExecutionContextId::new(context_id),
        }
    }

    fn cdp_frame(id: &str, parent_id: Option<&str>, loader_id: &str) -> CdpFrame {
        let mut builder = CdpFrame::builder()
            .id(FrameId::new(id))
            .loader_id(LoaderId::new(loader_id))
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

    fn target_info(frame_id: &str, parent_id: &str) -> TargetInfo {
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

    fn attached_event(
        frame_id: &str,
        parent_id: &str,
        child_session: &SessionId,
    ) -> EventAttachedToTarget {
        EventAttachedToTarget {
            session_id: child_session.clone(),
            target_info: target_info(frame_id, parent_id),
            waiting_for_debugger: true,
        }
    }

    fn navigation_request(id: usize) -> FrameNavigationRequest {
        FrameNavigationRequest::new(
            NavigationId(id),
            Request::new(
                "Page.navigate".into(),
                json!({ "url": format!("https://nav{id}.example/") }),
            ),
        )
    }

    fn manager_with_main() -> (FrameManager, SessionId, FrameId) {
        let main_session = session("main");
        let main_frame_id = FrameId::new("main-frame");
        let mut manager = FrameManager::new(Duration::from_millis(100));
        manager.set_main_session_id(main_session.clone());
        manager.on_frame_navigated_in_session(
            &cdp_frame("main-frame", None, "main-loader"),
            main_session.clone(),
        );
        let frame = manager
            .frames
            .get_mut(&main_frame_id)
            .expect("main frame is tracked");
        frame.loader_id = Some(LoaderId::new("main-loader"));
        frame.on_loading_stopped();
        (manager, main_session, main_frame_id)
    }

    fn add_child(
        manager: &mut FrameManager,
        frame_id: &str,
        parent_id: &str,
        session_id: &SessionId,
    ) {
        assert_eq!(
            manager.on_frame_attached_in_session(
                FrameId::new(frame_id),
                Some(FrameId::new(parent_id)),
                session_id.clone(),
            ),
            None
        );
        manager.on_frame_navigated_in_session(
            &cdp_frame(frame_id, Some(parent_id), &format!("{frame_id}-loader")),
            session_id.clone(),
        );
        let frame = manager
            .frames
            .get_mut(&FrameId::new(frame_id))
            .expect("child frame is tracked");
        frame.loader_id = Some(LoaderId::new(format!("{frame_id}-loader")));
        frame.on_loading_stopped();
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

    /// A third-class isolated world: neither the default main world nor the
    /// utility world chromiumoxide creates. Chrome emits these for injected /
    /// extension content scripts and `about:blank` isolated worlds.
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
                .expect("context fixture has all mandatory fields"),
        }
    }

    fn destroyed(unique_id: &str) -> EventExecutionContextDestroyed {
        EventExecutionContextDestroyed {
            execution_context_unique_id: unique_id.to_owned(),
        }
    }

    #[test]
    fn isolated_world_registration_tracks_pending_confirmed_and_absent() {
        let (mut manager, main_session, _) = manager_with_main();

        assert!(
            manager
                .ensure_isolated_world_in_session(UTILITY_WORLD_NAME, main_session.clone())
                .is_some()
        );
        assert_eq!(
            manager.isolated_world_state(&main_session, UTILITY_WORLD_NAME),
            Some(IsolatedWorldState::Pending)
        );
        assert!(
            manager
                .ensure_isolated_world_in_session(UTILITY_WORLD_NAME, main_session.clone())
                .is_none(),
            "an in-flight registration must be deduplicated"
        );

        manager.settle_isolated_world_registration(&main_session, UTILITY_WORLD_NAME, false);
        assert_eq!(
            manager.isolated_world_state(&main_session, UTILITY_WORLD_NAME),
            None
        );
        assert!(
            manager
                .ensure_isolated_world_in_session(UTILITY_WORLD_NAME, main_session.clone())
                .is_some(),
            "an explicit protocol failure restores the absent state"
        );

        manager.settle_isolated_world_registration(&main_session, UTILITY_WORLD_NAME, true);
        assert_eq!(
            manager.isolated_world_state(&main_session, UTILITY_WORLD_NAME),
            Some(IsolatedWorldState::Confirmed)
        );
        assert!(
            manager
                .ensure_isolated_world_in_session(UTILITY_WORLD_NAME, main_session.clone())
                .is_none()
        );

        let child_session = session("child");
        assert!(
            manager
                .ensure_isolated_world_on_next_document_in_session(
                    UTILITY_WORLD_NAME,
                    child_session.clone(),
                )
                .is_some()
        );
        manager.on_detached_from_target(&child_session, &main_session);
        assert_eq!(
            manager.isolated_world_state(&child_session, UTILITY_WORLD_NAME),
            None
        );
    }

    #[test]
    fn isolated_world_late_success_does_not_recreate_cleared_registration() {
        let (mut manager, main_session, _) = manager_with_main();
        assert!(
            manager
                .ensure_isolated_world_in_session(UTILITY_WORLD_NAME, main_session.clone())
                .is_some()
        );
        manager.clear_isolated_world_registrations();
        manager.settle_isolated_world_registration(&main_session, UTILITY_WORLD_NAME, true);
        assert_eq!(
            manager.isolated_world_state(&main_session, UTILITY_WORLD_NAME),
            None,
            "whole-target cleanup must not be undone by a late success"
        );

        let child_session = session("child");
        assert!(
            manager
                .ensure_isolated_world_on_next_document_in_session(
                    UTILITY_WORLD_NAME,
                    child_session.clone(),
                )
                .is_some()
        );
        manager.on_detached_from_target(&child_session, &main_session);
        manager.settle_isolated_world_registration(&child_session, UTILITY_WORLD_NAME, true);
        assert_eq!(
            manager.isolated_world_state(&child_session, UTILITY_WORLD_NAME),
            None,
            "child detach cleanup must not be undone by a late success"
        );
    }

    #[test]
    fn isolated_world_context_then_protocol_error_stays_confirmed() {
        let (mut manager, main_session, _) = manager_with_main();
        assert!(
            manager
                .ensure_isolated_world_in_session(UTILITY_WORLD_NAME, main_session.clone())
                .is_some()
        );

        manager.on_frame_execution_context_created_in_session(
            &execution_context("main-frame", 90, "default-context"),
            main_session.clone(),
        );
        assert_eq!(
            manager.isolated_world_state(&main_session, UTILITY_WORLD_NAME),
            Some(IsolatedWorldState::Pending),
            "a default context must not confirm the named utility world"
        );

        manager.on_frame_execution_context_created_in_session(
            &isolated_execution_context("main-frame", 93, "extension-context", "extension"),
            main_session.clone(),
        );
        assert_eq!(
            manager.isolated_world_state(&main_session, UTILITY_WORLD_NAME),
            Some(IsolatedWorldState::Pending),
            "a differently named isolated context must not confirm the utility key"
        );
        assert_eq!(
            manager.isolated_world_state(&main_session, "extension"),
            Some(IsolatedWorldState::Confirmed),
            "the observed extension world is tracked under its own real name"
        );

        manager.on_frame_execution_context_created_in_session(
            &isolated_execution_context("main-frame", 91, "utility-context", UTILITY_WORLD_NAME),
            main_session.clone(),
        );
        manager.settle_isolated_world_registration(&main_session, UTILITY_WORLD_NAME, false);

        assert_eq!(
            manager.isolated_world_state(&main_session, UTILITY_WORLD_NAME),
            Some(IsolatedWorldState::Confirmed)
        );
    }

    #[test]
    fn isolated_world_protocol_error_then_context_becomes_confirmed() {
        let (mut manager, main_session, _) = manager_with_main();
        assert!(
            manager
                .ensure_isolated_world_in_session(UTILITY_WORLD_NAME, main_session.clone())
                .is_some()
        );
        manager.settle_isolated_world_registration(&main_session, UTILITY_WORLD_NAME, false);
        assert_eq!(
            manager.isolated_world_state(&main_session, UTILITY_WORLD_NAME),
            None
        );

        manager.on_frame_execution_context_created_in_session(
            &isolated_execution_context(
                "main-frame",
                92,
                "late-utility-context",
                UTILITY_WORLD_NAME,
            ),
            main_session.clone(),
        );

        assert_eq!(
            manager.isolated_world_state(&main_session, UTILITY_WORLD_NAME),
            Some(IsolatedWorldState::Confirmed)
        );
    }

    #[test]
    fn destroying_a_third_class_isolated_context_clears_both_indices() {
        let (mut manager, main_session, _) = manager_with_main();
        manager.on_frame_execution_context_created_in_session(
            &isolated_execution_context("main-frame", 5, "extension-context", "extension"),
            main_session.clone(),
        );
        assert!(manager.context_by_unique.contains_key("extension-context"));
        assert!(
            manager
                .context_to_frame
                .contains_key(&ctx_key(&main_session, 5))
        );

        manager.on_frame_execution_context_destroyed_in_session(
            &destroyed("extension-context"),
            main_session.clone(),
        );

        // Historically this forward entry leaked because destroy could only
        // resolve the numeric id for the main/utility worlds.
        assert!(!manager.context_by_unique.contains_key("extension-context"));
        assert!(
            !manager
                .context_to_frame
                .contains_key(&ctx_key(&main_session, 5))
        );
    }

    #[test]
    fn numeric_context_id_reuse_across_frames_does_not_misroute() {
        let (mut manager, main_session, _) = manager_with_main();
        add_child(&mut manager, "child", "main-frame", &main_session);
        // Two frames each get numeric context id 3 in the same session.
        manager.on_frame_execution_context_created_in_session(
            &isolated_execution_context("main-frame", 3, "main-iso", "iso"),
            main_session.clone(),
        );
        manager.on_frame_execution_context_created_in_session(
            &isolated_execution_context("child", 3, "child-iso", "iso"),
            main_session.clone(),
        );

        // The later insert owns numeric key (session, 3); the forward map must
        // point at the frame that created it last, not the earlier one.
        let binding = manager
            .context_to_frame
            .get(&ctx_key(&main_session, 3))
            .expect("numeric key is bound");
        assert_eq!(binding.frame_id, FrameId::new("child"));
        assert_eq!(binding.unique_id, "child-iso");

        // Destroying the earlier context must not touch the reused forward key.
        manager.on_frame_execution_context_destroyed_in_session(
            &destroyed("main-iso"),
            main_session.clone(),
        );
        let binding = manager
            .context_to_frame
            .get(&ctx_key(&main_session, 3))
            .expect("reused key survives the stale destroy");
        assert_eq!(binding.frame_id, FrameId::new("child"));
        assert!(!manager.context_by_unique.contains_key("main-iso"));
    }

    #[test]
    fn late_destroy_after_same_frame_numeric_reuse_keeps_current_binding() {
        let (mut manager, main_session, _) = manager_with_main();
        // First context on the frame with numeric id 4.
        manager.on_frame_execution_context_created_in_session(
            &isolated_execution_context("main-frame", 4, "first-iso", "iso"),
            main_session.clone(),
        );
        // Numeric id 4 is reused on the same frame by a newer context before the
        // first one's destroy is processed.
        manager.on_frame_execution_context_created_in_session(
            &isolated_execution_context("main-frame", 4, "second-iso", "iso"),
            main_session.clone(),
        );

        // The forward key now belongs to the newer context.
        assert_eq!(
            manager
                .context_to_frame
                .get(&ctx_key(&main_session, 4))
                .map(|binding| binding.unique_id.clone()),
            Some("second-iso".to_owned())
        );

        // Late destroy of the first context must not evict the newer binding.
        manager.on_frame_execution_context_destroyed_in_session(
            &destroyed("first-iso"),
            main_session.clone(),
        );
        assert_eq!(
            manager
                .context_to_frame
                .get(&ctx_key(&main_session, 4))
                .map(|binding| binding.unique_id.clone()),
            Some("second-iso".to_owned())
        );
        assert!(!manager.context_by_unique.contains_key("first-iso"));
        assert!(manager.context_by_unique.contains_key("second-iso"));
    }

    #[test]
    fn bulk_clear_of_old_frame_preserves_reused_key_binding_of_another_frame() {
        let (mut manager, main_session, _) = manager_with_main();
        add_child(&mut manager, "childA", "main-frame", &main_session);
        add_child(&mut manager, "childB", "main-frame", &main_session);
        // Both frames use numeric context id 3 in the same session; childB is
        // created last, so it owns the forward (session, 3) key. childA leaves a
        // stale reverse entry that still points at the reused key.
        manager.on_frame_execution_context_created_in_session(
            &isolated_execution_context("childA", 3, "A-iso", "iso"),
            main_session.clone(),
        );
        manager.on_frame_execution_context_created_in_session(
            &isolated_execution_context("childB", 3, "B-iso", "iso"),
            main_session.clone(),
        );
        assert_eq!(
            manager
                .context_to_frame
                .get(&ctx_key(&main_session, 3))
                .map(|binding| binding.unique_id.clone()),
            Some("B-iso".to_owned())
        );

        // Drive a REAL bulk path (recursive frame removal), not the helper
        // directly: detaching childA must not delete childB's live forward
        // binding on the reused key.
        manager.on_frame_detached(&EventFrameDetached {
            frame_id: FrameId::new("childA"),
            reason: FrameDetachedReason::Remove,
        });

        assert_eq!(
            manager
                .context_to_frame
                .get(&ctx_key(&main_session, 3))
                .map(|binding| binding.unique_id.clone()),
            Some("B-iso".to_owned()),
            "childB's forward binding must survive childA's bulk clear"
        );
        assert!(!manager.context_by_unique.contains_key("A-iso"));
        assert!(manager.context_by_unique.contains_key("B-iso"));
    }

    #[test]
    fn session_detach_leaves_another_sessions_same_numeric_key_intact() {
        let (mut manager, main_session, _) = manager_with_main();
        // childA lives in a child session; childB stays on the main session.
        add_child(&mut manager, "childA", "main-frame", &main_session);
        let child_session = session("child-session");
        manager.on_attached_to_target_in_session(
            &attached_event("childA", "main-frame", &child_session),
            child_session.clone(),
        );
        add_child(&mut manager, "childB", "main-frame", &main_session);
        // Reuse numeric id 5 across the two sessions. The keys differ by session
        // (`(child_session, 5)` vs `(main_session, 5)`), so this exercises
        // per-session scoping of the bulk clear, NOT the same-key `still_ours`
        // guard (which the same-session tests below cover).
        manager.on_frame_execution_context_created_in_session(
            &isolated_execution_context("childA", 5, "A-iso", "iso"),
            child_session.clone(),
        );
        manager.on_frame_execution_context_created_in_session(
            &isolated_execution_context("childB", 5, "B-iso", "iso"),
            main_session.clone(),
        );

        manager.on_detached_from_target(&child_session, &main_session);

        // childB's main-session binding on numeric id 5 must be untouched.
        assert_eq!(
            manager
                .context_to_frame
                .get(&ctx_key(&main_session, 5))
                .map(|binding| binding.unique_id.clone()),
            Some("B-iso".to_owned())
        );
        assert!(!manager.context_by_unique.contains_key("A-iso"));
        assert_no_orphan_forward_entries(&manager);
    }

    #[test]
    fn same_session_swap_clear_preserves_another_frames_reused_key_binding() {
        let (mut manager, main_session, _) = manager_with_main();
        add_child(&mut manager, "childA", "main-frame", &main_session);
        add_child(&mut manager, "childB", "main-frame", &main_session);
        // Same session, same numeric id 7; childB is created last and owns the
        // forward key, leaving childA with a stale reverse entry.
        manager.on_frame_execution_context_created_in_session(
            &isolated_execution_context("childA", 7, "A-iso", "iso"),
            main_session.clone(),
        );
        manager.on_frame_execution_context_created_in_session(
            &isolated_execution_context("childB", 7, "B-iso", "iso"),
            main_session.clone(),
        );

        // Swap-detaching childA clears its contexts through `clear_frame_contexts`
        // (a different bulk path than recursive removal). The `still_ours` guard
        // must keep childB's live binding on the reused key.
        manager.on_frame_detached(&EventFrameDetached {
            frame_id: FrameId::new("childA"),
            reason: FrameDetachedReason::Swap,
        });

        assert_eq!(
            manager
                .context_to_frame
                .get(&ctx_key(&main_session, 7))
                .map(|binding| binding.unique_id.clone()),
            Some("B-iso".to_owned()),
            "childB's forward binding must survive childA's swap-clear"
        );
        assert!(!manager.context_by_unique.contains_key("A-iso"));
        assert_no_orphan_forward_entries(&manager);
    }

    /// Every `context_to_frame` (forward) entry must have a matching
    /// `context_by_unique` (reverse) entry carrying the same unique id. The
    /// forward map is driven entirely by the reverse map, so an orphaned forward
    /// entry would mean an insert path wrote only one map — a drift the bulk
    /// eviction can no longer clean up. Asserting it turns the "forward always
    /// has a reverse" invariant into a tested contract.
    fn assert_no_orphan_forward_entries(manager: &FrameManager) {
        for (key, binding) in &manager.context_to_frame {
            let reverse = manager.context_by_unique.get(&binding.unique_id);
            assert!(
                reverse.is_some_and(|context| context.key.as_ref() == Some(key)),
                "forward entry {key:?} -> {binding:?} has no matching reverse entry",
            );
        }
    }

    #[test]
    fn recursive_removal_leaves_no_orphan_forward_entries() {
        let (mut manager, main_session, _) = manager_with_main();
        add_child(&mut manager, "child", "main-frame", &main_session);
        add_child(&mut manager, "grandchild", "child", &main_session);
        manager.on_frame_execution_context_created_in_session(
            &execution_context("child", 1, "child-default"),
            main_session.clone(),
        );
        manager.on_frame_execution_context_created_in_session(
            &isolated_execution_context("grandchild", 2, "grand-iso", "iso"),
            main_session.clone(),
        );

        manager.on_frame_detached(&EventFrameDetached {
            frame_id: FrameId::new("child"),
            reason: FrameDetachedReason::Remove,
        });

        assert_no_orphan_forward_entries(&manager);
    }

    #[test]
    fn fresh_frames_bind_to_the_event_session() {
        let (mut manager, main_session, _) = manager_with_main();
        add_child(&mut manager, "child", "main-frame", &main_session);

        assert_eq!(
            manager
                .frame(&FrameId::new("child"))
                .and_then(Frame::session_id),
            Some(&main_session)
        );
    }

    #[test]
    fn swap_back_uses_the_parent_session_and_rejects_a_stale_old_attach() {
        let (mut manager, main_session, _) = manager_with_main();
        add_child(&mut manager, "child", "main-frame", &main_session);
        let child_session = session("child-session");
        manager.on_attached_to_target_in_session(
            &attached_event("child", "main-frame", &child_session),
            child_session.clone(),
        );
        assert!(manager.is_child_session(&child_session));

        let swapped = manager.on_frame_attached_in_session(
            FrameId::new("child"),
            Some(FrameId::new("main-frame")),
            main_session.clone(),
        );
        assert_eq!(swapped, Some(child_session.clone()));
        assert_eq!(
            manager
                .frame(&FrameId::new("child"))
                .and_then(Frame::session_id),
            Some(&main_session)
        );

        let stale = manager.on_frame_attached_in_session(
            FrameId::new("child"),
            Some(FrameId::new("main-frame")),
            child_session,
        );
        assert_eq!(stale, None);
        assert_eq!(
            manager
                .frame(&FrameId::new("child"))
                .and_then(Frame::session_id),
            Some(&main_session)
        );
    }

    #[test]
    fn nested_swap_back_rebinds_s2_to_parent_s1_not_to_main() {
        let (mut manager, main_session, _) = manager_with_main();
        add_child(&mut manager, "outer", "main-frame", &main_session);
        let session_one = session("s1");
        manager.on_attached_to_target_in_session(
            &attached_event("outer", "main-frame", &session_one),
            session_one.clone(),
        );
        add_child(&mut manager, "inner", "outer", &session_one);
        let session_two = session("s2");
        manager.on_attached_to_target_in_session(
            &attached_event("inner", "outer", &session_two),
            session_two.clone(),
        );

        let swapped = manager.on_frame_attached_in_session(
            FrameId::new("inner"),
            Some(FrameId::new("outer")),
            session_one.clone(),
        );
        assert_eq!(swapped, Some(session_two));
        let inner = manager.frame(&FrameId::new("inner")).expect("inner frame");
        assert_eq!(inner.session_id(), Some(&session_one));
        assert!(inner.is_out_of_process(&main_session));
    }

    #[test]
    fn execution_context_ids_are_scoped_by_session() {
        let (mut manager, main_session, _) = manager_with_main();
        add_child(&mut manager, "child", "main-frame", &main_session);
        let child_session = session("child-session");
        manager.rebind_frame_session(&FrameId::new("child"), child_session.clone());

        manager.on_frame_execution_context_created_in_session(
            &execution_context("main-frame", 7, "main-unique"),
            main_session.clone(),
        );
        manager.on_frame_execution_context_created_in_session(
            &execution_context("child", 7, "child-unique"),
            child_session.clone(),
        );

        assert_eq!(manager.context_to_frame.len(), 2);
        assert_eq!(
            manager
                .context_to_frame
                .get(&ctx_key(&main_session, 7))
                .map(|binding| binding.frame_id.clone()),
            Some(FrameId::new("main-frame"))
        );
        assert_eq!(
            manager
                .context_to_frame
                .get(&ctx_key(&child_session, 7))
                .map(|binding| binding.frame_id.clone()),
            Some(FrameId::new("child"))
        );

        manager.on_execution_contexts_cleared_in_session(main_session.clone());
        assert!(
            manager
                .frame(&FrameId::new("main-frame"))
                .expect("main frame")
                .main_world()
                .execution_context()
                .is_none()
        );
        assert_eq!(
            manager
                .frame(&FrameId::new("child"))
                .expect("child frame")
                .main_world()
                .execution_context(),
            Some(ExecutionContextId::new(7))
        );
        assert!(
            !manager
                .context_to_frame
                .contains_key(&ctx_key(&main_session, 7))
        );
        assert!(
            manager
                .context_to_frame
                .contains_key(&ctx_key(&child_session, 7))
        );
    }

    #[test]
    fn binding_change_evicts_waiters_and_submitted_navigation_lanes() {
        let (mut manager, main_session, _) = manager_with_main();
        add_child(&mut manager, "child", "main-frame", &main_session);
        let child_frame = FrameId::new("child");
        let (wait_tx, wait_rx) = oneshot::channel();
        manager.wait_for_navigation(main_session.clone(), child_frame.clone(), wait_tx);

        let next_session = session("next");
        manager.rebind_frame_session(&child_frame, next_session.clone());
        assert_eq!(
            block_on(wait_rx).expect("waiter resolves"),
            Err(FrameWaitError::FrameSwappedOrDetached)
        );

        manager.navigate_frame_in_session(
            next_session.clone(),
            child_frame.clone(),
            navigation_request(11),
        );
        assert!(matches!(
            manager.poll(Instant::now()),
            Some(FrameEvent::NavigationRequest(NavigationId(11), _))
        ));
        manager.rebind_frame_session(&child_frame, session("third"));
        assert!(matches!(
            manager.poll(Instant::now()),
            Some(FrameEvent::NavigationResult(Err(
                NavigationError::FrameNotFound {
                    id: NavigationId(11),
                    ..
                }
            )))
        ));
    }

    #[test]
    fn cross_session_navigation_updates_metadata_without_removing_descendants() {
        let (mut manager, main_session, _) = manager_with_main();
        add_child(&mut manager, "child", "main-frame", &main_session);
        let child_session = session("child-session");
        manager.rebind_frame_session(&FrameId::new("child"), child_session.clone());
        add_child(&mut manager, "grandchild", "child", &child_session);

        let mut event = cdp_frame("child", Some("main-frame"), "foreign-loader");
        event.url = "https://updated.example/path".to_owned();
        event.name = Some("updated-name".to_owned());
        manager.on_frame_navigated_in_session(&event, main_session);

        let child = manager.frame(&FrameId::new("child")).expect("child frame");
        assert_eq!(child.url(), Some("https://updated.example/path"));
        assert_eq!(child.name(), Some("updated-name"));
        assert_eq!(child.session_id(), Some(&child_session));
        assert_eq!(child.loader_id, Some(LoaderId::new("child-loader")));
        assert!(child.child_frames.contains(&FrameId::new("grandchild")));
        assert!(manager.frames.contains_key(&FrameId::new("grandchild")));
    }

    #[test]
    fn anticipated_goto_merge_preserves_waiter_and_old_loader_snapshot() {
        let (mut manager, main_session, main_frame_id) = manager_with_main();
        let (wait_tx, _wait_rx) = oneshot::channel();
        manager.wait_for_navigation(main_session.clone(), main_frame_id.clone(), wait_tx);
        manager.navigate_frame_in_session(
            main_session.clone(),
            main_frame_id.clone(),
            navigation_request(12),
        );

        assert!(matches!(
            manager.poll(Instant::now()),
            Some(FrameEvent::NavigationRequest(NavigationId(12), _))
        ));
        let entry = manager
            .navigation
            .get(&(Some(main_session), main_frame_id))
            .expect("anticipated entry was upgraded in place");
        assert_eq!(entry.waiters.len(), 1);
        assert_eq!(entry.pre_loader_id, Some(LoaderId::new("main-loader")));
        assert_eq!(
            entry
                .watcher
                .as_ref()
                .and_then(|watcher| watcher.loader_id.clone()),
            Some(LoaderId::new("main-loader"))
        );
    }

    #[test]
    fn none_and_session_navigation_lanes_evict_each_other() {
        let frame_id = FrameId::new("standalone");
        let mut manager = FrameManager::new(Duration::from_millis(100));
        manager
            .frames
            .insert(frame_id.clone(), Frame::new(frame_id.clone()));
        manager.navigate_frame(frame_id.clone(), navigation_request(1));
        manager.rebind_frame_session(&frame_id, session("s1"));
        assert!(matches!(
            manager.poll(Instant::now()),
            Some(FrameEvent::NavigationResult(Err(
                NavigationError::FrameNotFound {
                    id: NavigationId(1),
                    ..
                }
            )))
        ));

        manager.navigate_frame_in_session(session("s1"), frame_id.clone(), navigation_request(2));
        manager.register_pending(None, frame_id, navigation_request(3));
        assert!(matches!(
            manager.poll(Instant::now()),
            Some(FrameEvent::NavigationResult(Err(
                NavigationError::FrameNotFound {
                    id: NavigationId(2),
                    ..
                }
            )))
        ));
    }

    #[test]
    fn explicit_same_document_navigation_completes_the_correct_lane() {
        let (mut manager, main_session, main_frame_id) = manager_with_main();
        manager.navigate_frame_in_session(
            main_session.clone(),
            main_frame_id.clone(),
            navigation_request(21),
        );
        let request = manager.poll(Instant::now()).expect("navigation request");
        assert!(matches!(
            request,
            FrameEvent::NavigationRequest(NavigationId(21), _)
        ));

        manager.on_frame_navigated_within_document_in_session(
            &EventNavigatedWithinDocument {
                frame_id: main_frame_id,
                url: "https://main-frame.example/#hash".to_owned(),
                navigation_type: NavigatedWithinDocumentNavigationType::Fragment,
            },
            main_session,
        );
        assert!(matches!(
            manager.poll(Instant::now()),
            Some(FrameEvent::NavigationResult(Ok(
                NavigationOk::SameDocumentNavigation(NavigationId(21))
            )))
        ));
    }

    #[test]
    fn concurrent_frame_sessions_each_submit_one_navigation() {
        let (mut manager, main_session, main_frame_id) = manager_with_main();
        add_child(&mut manager, "child", "main-frame", &main_session);
        let child_session = session("child-session");
        manager.rebind_frame_session(&FrameId::new("child"), child_session.clone());

        manager.navigate_frame_in_session(
            main_session.clone(),
            main_frame_id,
            navigation_request(31),
        );
        manager.navigate_frame_in_session(
            child_session.clone(),
            FrameId::new("child"),
            navigation_request(32),
        );

        let first = manager.poll(Instant::now()).expect("first request");
        let second = manager.poll(Instant::now()).expect("second request");
        let sessions = [first, second]
            .into_iter()
            .map(|event| match event {
                FrameEvent::NavigationRequest(_, request) => request.session_id,
                other => panic!("expected navigation request, got {other:?}"),
            })
            .collect::<HashSet<_>>();
        assert_eq!(
            sessions,
            HashSet::from([Some(main_session.into()), Some(child_session.into()),])
        );
    }

    #[test]
    fn poll_completes_all_waiter_only_entries_in_one_full_scan() {
        let (mut manager, main_session, _) = manager_with_main();
        add_child(&mut manager, "child-one", "main-frame", &main_session);
        add_child(&mut manager, "child-two", "main-frame", &main_session);
        let (first_tx, first_rx) = oneshot::channel();
        let (second_tx, second_rx) = oneshot::channel();
        manager.wait_for_navigation(main_session.clone(), FrameId::new("child-one"), first_tx);
        manager.wait_for_navigation(main_session.clone(), FrameId::new("child-two"), second_tx);

        manager.on_frame_navigated_in_session(
            &cdp_frame("child-one", Some("main-frame"), "child-one-loader-new"),
            main_session.clone(),
        );
        manager.on_frame_navigated_in_session(
            &cdp_frame("child-two", Some("main-frame"), "child-two-loader-new"),
            main_session,
        );
        for (frame_id, loader_id) in [
            (
                FrameId::new("child-one"),
                LoaderId::new("child-one-loader-new"),
            ),
            (
                FrameId::new("child-two"),
                LoaderId::new("child-two-loader-new"),
            ),
        ] {
            let frame = manager.frames.get_mut(&frame_id).expect("tracked frame");
            frame.loader_id = Some(loader_id);
            frame.on_loading_stopped();
        }

        assert!(manager.poll(Instant::now()).is_none());
        assert_eq!(block_on(first_rx).expect("first waiter resolves"), Ok(()));
        assert_eq!(block_on(second_rx).expect("second waiter resolves"), Ok(()));
    }

    #[test]
    fn recursive_removal_cleans_context_and_navigation_state() {
        let (mut manager, main_session, _) = manager_with_main();
        add_child(&mut manager, "child", "main-frame", &main_session);
        add_child(&mut manager, "grandchild", "child", &main_session);
        manager.insert_context(
            Some(&main_session),
            ExecutionContextId::new(9),
            "grandchild-context".to_owned(),
            FrameId::new("grandchild"),
        );
        let (wait_tx, wait_rx) = oneshot::channel();
        manager.wait_for_navigation(main_session, FrameId::new("grandchild"), wait_tx);

        manager.on_frame_detached(&EventFrameDetached {
            frame_id: FrameId::new("child"),
            reason: FrameDetachedReason::Remove,
        });

        assert!(!manager.frames.contains_key(&FrameId::new("child")));
        assert!(!manager.frames.contains_key(&FrameId::new("grandchild")));
        assert!(!manager.context_by_unique.contains_key("grandchild-context"));
        assert!(
            !manager
                .context_to_frame
                .values()
                .any(|binding| binding.frame_id == FrameId::new("grandchild"))
        );
        assert_eq!(
            block_on(wait_rx).expect("waiter resolves"),
            Err(FrameWaitError::FrameNotFound {
                frame: FrameId::new("grandchild")
            })
        );
    }

    #[test]
    fn swap_detach_preserves_frame_identity_but_clears_its_contexts() {
        let (mut manager, main_session, _) = manager_with_main();
        add_child(&mut manager, "child", "main-frame", &main_session);
        manager.on_frame_execution_context_created_in_session(
            &execution_context("child", 12, "child-context"),
            main_session,
        );

        manager.on_frame_detached(&EventFrameDetached {
            frame_id: FrameId::new("child"),
            reason: FrameDetachedReason::Swap,
        });

        let child = manager
            .frame(&FrameId::new("child"))
            .expect("frame survives swap");
        assert!(child.main_world().execution_context().is_none());
        assert!(!manager.context_by_unique.contains_key("child-context"));
    }

    #[test]
    fn child_session_detach_cleans_session_state_and_rebinds_descendants() {
        let (mut manager, main_session, _) = manager_with_main();
        add_child(&mut manager, "child", "main-frame", &main_session);
        let child_session = session("child-session");
        manager.on_attached_to_target_in_session(
            &attached_event("child", "main-frame", &child_session),
            child_session.clone(),
        );
        add_child(&mut manager, "grandchild", "child", &child_session);
        manager.on_frame_execution_context_created_in_session(
            &execution_context("grandchild", 14, "grandchild-context"),
            child_session.clone(),
        );
        let (wait_tx, wait_rx) = oneshot::channel();
        manager.wait_for_navigation(child_session.clone(), FrameId::new("grandchild"), wait_tx);

        manager.on_detached_from_target(&child_session, &main_session);

        assert!(!manager.is_child_session(&child_session));
        for frame_id in [FrameId::new("child"), FrameId::new("grandchild")] {
            assert_eq!(
                manager.frame(&frame_id).and_then(Frame::session_id),
                Some(&main_session)
            );
        }
        assert!(!manager.context_by_unique.contains_key("grandchild-context"));
        assert!(
            !manager
                .context_to_frame
                .keys()
                .any(|key| key.session_id == child_session)
        );
        assert_eq!(
            block_on(wait_rx).expect("waiter resolves"),
            Err(FrameWaitError::FrameSwappedOrDetached)
        );
    }

    #[test]
    fn anticipated_waiter_timeouts_are_all_settled_before_poll_returns_none() {
        let (mut manager, main_session, main_frame_id) = manager_with_main();
        let (first_tx, first_rx) = oneshot::channel();
        let (second_tx, second_rx) = oneshot::channel();
        manager.wait_for_navigation(main_session.clone(), main_frame_id.clone(), first_tx);
        manager
            .navigation
            .get_mut(&(Some(main_session.clone()), main_frame_id.clone()))
            .expect("first navigation entry")
            .deadline = Instant::now() - Duration::from_millis(1);

        add_child(&mut manager, "child", "main-frame", &main_session);
        manager.wait_for_navigation(main_session.clone(), FrameId::new("child"), second_tx);
        manager
            .navigation
            .get_mut(&(Some(main_session), FrameId::new("child")))
            .expect("second navigation entry")
            .deadline = Instant::now() - Duration::from_millis(1);

        assert!(manager.poll(Instant::now()).is_none());
        assert_eq!(
            block_on(first_rx).expect("first waiter resolves"),
            Err(FrameWaitError::Timeout)
        );
        assert_eq!(
            block_on(second_rx).expect("second waiter resolves"),
            Err(FrameWaitError::Timeout)
        );
    }
}
