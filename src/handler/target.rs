use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use futures::channel::oneshot::Sender;
use futures::stream::Stream;
use futures::task::{Context, Poll};

use chromiumoxid_types::{Method, Request, Response};

use crate::browser::CommandMessage;
use crate::cdp::browser_protocol::{
    browser::BrowserContextId,
    log, performance,
    target::{SessionId, SetAutoAttachParams, TargetId, TargetInfo},
};
use crate::cdp::events::CdpEvent;
use crate::cdp::CdpEventMessage;
use crate::error::{CdpError, DeadlineExceeded};
use crate::handler::cmd::CommandChain;
use crate::handler::emulation::EmulationManager;
use crate::handler::frame::FrameNavigationRequest;
use crate::handler::frame::{
    FrameEvent, FrameManager, NavigationError, NavigationId, NavigationOk,
};
use crate::handler::network::NetworkManager;
use crate::handler::page::PageHandle;
use crate::handler::viewport::Viewport;
use crate::handler::PageInner;
use crate::page::Page;

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
                Some(Err(err)) => Some(TargetEvent::RequestTimeout(err)),
            };
        }
    }};
}

pub struct Target {
    info: TargetInfo,
    is_closed: bool,
    frame_manager: FrameManager,
    network_manager: NetworkManager,
    emulation_manager: EmulationManager,
    viewport: Viewport,
    session_id: Option<SessionId>,
    page: Option<PageHandle>,
    init_state: TargetInit,
    queued_events: VecDeque<TargetEvent>,
    /// The sender who initiated the creation of a page.
    initiator: Option<Sender<Result<Page, CdpError>>>,
}

impl Target {
    /// Create a new target instance with `TargetInfo` after a
    /// `CreateTargetParams` request.
    pub fn new(info: TargetInfo) -> Self {
        Self {
            info,
            is_closed: false,
            frame_manager: Default::default(),
            network_manager: Default::default(),
            emulation_manager: Default::default(),
            viewport: Default::default(),
            session_id: None,
            page: None,
            init_state: TargetInit::InitializingFrame(FrameManager::init_commands()),
            queued_events: Default::default(),
            initiator: None,
        }
    }

    pub fn set_session_id(&mut self, id: SessionId) {
        self.session_id = Some(id)
    }

    pub fn session_id(&self) -> Option<&SessionId> {
        self.session_id.as_ref()
    }

    pub fn session_id_mut(&mut self) -> &mut Option<SessionId> {
        &mut self.session_id
    }

    /// The identifier for this target
    pub fn target_id(&self) -> &TargetId {
        &self.info.target_id
    }

    /// Whether this target is already initialized
    pub fn is_initialized(&self) -> bool {
        matches!(self.init_state, TargetInit::Initialized)
    }

    pub fn goto(&mut self, req: FrameNavigationRequest) {
        self.frame_manager.goto(req)
    }

    fn create_page(&mut self) {
        if self.page.is_none() {
            if let Some(session) = self.session_id.clone() {
                let handle = PageHandle::new(self.target_id().clone(), session);
                self.page = Some(handle);
            }
        }
    }

    pub(crate) fn get_or_create_page(&mut self) -> Option<&Arc<PageInner>> {
        self.create_page();
        self.page.as_ref().map(|p| p.inner())
    }

    pub fn is_page(&self) -> bool {
        todo!()
    }

    pub fn browser_context_id(&self) -> Option<&BrowserContextId> {
        self.info.browser_context_id.as_ref()
    }

    pub fn info(&self) -> &TargetInfo {
        &self.info
    }

    /// Get the target that opened this target. Top-level targets return `None`.
    pub fn opener(&self) -> Option<&TargetId> {
        self.info.opener_id.as_ref()
    }

    pub fn frame_manager_mut(&mut self) -> &mut FrameManager {
        &mut self.frame_manager
    }

    /// Received a response to a command issued by this target
    pub fn on_response(&mut self, resp: Response) {
        if let Some(cmds) = self.init_state.commands_mut() {
            cmds.received_response(resp.method.as_ref());
        }
    }

    pub fn on_event(&mut self, event: CdpEventMessage) {
        match event.params {
            // `FrameManager` events
            CdpEvent::PageFrameAttached(ev) => self.frame_manager.on_frame_attached(&ev),
            CdpEvent::PageFrameDetached(ev) => self.frame_manager.on_frame_detached(&ev),
            CdpEvent::PageFrameNavigated(ev) => self.frame_manager.on_frame_navigated(&*ev),
            CdpEvent::PageNavigatedWithinDocument(ev) => {
                self.frame_manager.on_frame_navigated_within_document(&ev)
            }
            CdpEvent::RuntimeExecutionContextCreated(ev) => {
                self.frame_manager.on_frame_execution_context_created(&ev)
            }
            CdpEvent::RuntimeExecutionContextDestroyed(ev) => {
                self.frame_manager.on_frame_execution_context_destroyed(&ev)
            }
            CdpEvent::RuntimeExecutionContextsCleared(ev) => {
                self.frame_manager.on_execution_context_cleared(&ev)
            }
            CdpEvent::PageLifecycleEvent(ev) => self.frame_manager.on_page_lifecycle_event(&ev),

            // `NetworkManager` events
            CdpEvent::FetchRequestPaused(ev) => self.network_manager.on_fetch_request_paused(&*ev),
            CdpEvent::FetchAuthRequired(ev) => self.network_manager.on_fetch_auth_required(&*ev),
            CdpEvent::NetworkRequestWillBeSent(ev) => {
                self.network_manager.on_request_will_be_sent(&*ev)
            }
            CdpEvent::NetworkRequestServedFromCache(ev) => {
                self.network_manager.on_request_served_from_cache(&ev)
            }
            CdpEvent::NetworkResponseReceived(ev) => {
                self.network_manager.on_response_received(&*ev)
            }
            CdpEvent::NetworkLoadingFinished(ev) => {
                self.network_manager.on_network_loading_finished(&ev)
            }
            CdpEvent::NetworkLoadingFailed(ev) => {
                self.network_manager.on_network_loading_failed(&ev)
            }
            // Other
            // CdpEvent::PageLoadEventFired(ev) => self.frame_manager.on_load_event_fired(&ev),
            _ => {}
        }
    }

    /// Advance that target's state
    pub(crate) fn poll(&mut self, cx: &mut Context<'_>, now: Instant) -> Option<TargetEvent> {
        match &mut self.init_state {
            TargetInit::InitializingFrame(cmds) => {
                advance_state!(
                    self,
                    cx,
                    now,
                    cmds,
                    TargetInit::InitializingNetwork(self.network_manager.init_commands())
                );
            }
            TargetInit::InitializingNetwork(cmds) => {
                advance_state!(
                    self,
                    cx,
                    now,
                    cmds,
                    TargetInit::InitializingPage(Self::page_init_commands())
                );
            }
            TargetInit::InitializingPage(cmds) => {
                advance_state!(
                    self,
                    cx,
                    now,
                    cmds,
                    TargetInit::InitializingEmulation(
                        self.emulation_manager.init_commands(&self.viewport),
                    )
                );
            }
            TargetInit::InitializingEmulation(cmds) => {
                advance_state!(self, cx, now, cmds, TargetInit::Initialized);
            }
            TargetInit::Initialized => {
                if let Some(initiator) = self.initiator.take() {
                    if let Some(page) = self.get_or_create_page() {
                        let _ = initiator.send(Ok(page.clone().into()));
                    }
                }
            }
        };
        loop {
            // Drain queued messages first.
            if let Some(ev) = self.queued_events.pop_front() {
                return Some(ev);
            }

            if let Some(handle) = self.page.as_mut() {
                while let Poll::Ready(Some(msg)) = Pin::new(&mut handle.rx).poll_next(cx) {
                    self.queued_events.push_back(TargetEvent::Message(msg));
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

    pub fn set_initiator(&mut self, tx: Sender<Result<Page, CdpError>>) {
        self.initiator = Some(tx)
    }

    // TODO move to other location
    pub(crate) fn page_init_commands() -> CommandChain {
        let attach = SetAutoAttachParams::builder()
            .flatten(true)
            .auto_attach(true)
            .wait_for_debugger_on_start(true)
            .build()
            .unwrap();
        let enable_performance = performance::EnableParams::default();
        let enable_log = log::EnableParams::default();
        CommandChain::new(vec![
            (attach.identifier(), serde_json::to_value(attach).unwrap()),
            (
                enable_performance.identifier(),
                serde_json::to_value(enable_performance).unwrap(),
            ),
            (
                enable_log.identifier(),
                serde_json::to_value(enable_log).unwrap(),
            ),
        ])
    }
}

#[derive(Debug)]
pub(crate) enum TargetEvent {
    /// An internal request
    Request(Request),
    /// An internal navigation request
    NavigationRequest(NavigationId, Request),
    /// Indicates that a previous requested navigation has finished
    NavigationResult(Result<NavigationOk, NavigationError>),
    /// An internal request timed out
    RequestTimeout(DeadlineExceeded),
    /// A new message arrived via a channel
    Message(TargetMessage),
}

// TODO this can be moved into the classes?
#[derive(Debug)]
pub enum TargetInit {
    InitializingFrame(CommandChain),
    InitializingNetwork(CommandChain),
    InitializingPage(CommandChain),
    InitializingEmulation(CommandChain),
    Initialized,
}

impl TargetInit {
    fn commands_mut(&mut self) -> Option<&mut CommandChain> {
        match self {
            TargetInit::InitializingFrame(cmd) => Some(cmd),
            TargetInit::InitializingNetwork(cmd) => Some(cmd),
            TargetInit::InitializingPage(cmd) => Some(cmd),
            TargetInit::InitializingEmulation(cmd) => Some(cmd),
            TargetInit::Initialized => None,
        }
    }
}

#[derive(Debug)]
pub(crate) enum TargetMessage {
    Command(CommandMessage),
}
