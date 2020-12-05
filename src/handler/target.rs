use std::borrow::Cow;
use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use futures::channel::oneshot::Sender;
use futures::task::Poll;
use serde_json::Value;

use chromiumoxid_types::{Method, Request, Response};

use crate::cdp::browser_protocol::browser::BrowserContextId;
use crate::cdp::browser_protocol::log;
use crate::cdp::browser_protocol::page::*;
use crate::cdp::browser_protocol::performance;
use crate::cdp::browser_protocol::target::{SessionId, SetAutoAttachParams, TargetId, TargetInfo};
use crate::cdp::events::CdpEvent;
use crate::cdp::CdpEventMessage;
use crate::error::CdpError;
use crate::handler::cmd::CommandChain;
use crate::handler::emulation::EmulationManager;
use crate::handler::frame::FrameManager;
use crate::handler::network::NetworkManager;
use crate::handler::viewport::Viewport;
use crate::page::{Page, PageInner};

pub(crate) struct Target {
    info: TargetInfo,
    is_closed: bool,
    frame_manager: FrameManager,
    network_manager: NetworkManager,
    emulation_manager: EmulationManager,
    viewport: Viewport,
    session_id: Option<SessionId>,
    page: Option<PageInner>,
    state: TargetState,
    /// The sender who initiated the creation of a page.
    initiator: Option<Sender<Result<Page, CdpError>>>,
    pending_navigations: VecDeque<(Option<Sender<Response>>, Request)>,
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
            state: TargetState::InitializingFrame(FrameManager::init_commands()),
            initiator: None,
            pending_navigations: Default::default(),
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

    pub fn is_page(&self) -> bool {
        todo!()
    }

    pub fn browser_context_id(&self) -> Option<&BrowserContextId> {
        self.info.browser_context_id.as_ref()
    }

    pub fn r#type(&self) {
        // self.info.type
    }

    fn goto(&mut self) {
        // queue in command
    }

    pub fn set_viewport(&mut self) {}

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
        if let Some(cmds) = self.state.commands_mut() {
            cmds.received_response(resp.method.as_ref());
        }
    }

    pub fn on_event(&mut self, event: CdpEventMessage) {
        match event.params {
            // `FrameManager` events
            CdpEvent::PageFrameAttached(ev) => self.frame_manager.on_frame_attached(&*ev),
            CdpEvent::PageFrameDetached(ev) => self.frame_manager.on_frame_detached(&*ev),
            CdpEvent::PageFrameNavigated(ev) => self.frame_manager.on_frame_navigated(&ev),
            CdpEvent::PageNavigatedWithinDocument(ev) => {
                self.frame_manager.on_frame_navigated_within_document(&*ev)
            }
            CdpEvent::RuntimeExecutionContextCreated(ev) => {
                self.frame_manager.on_frame_execution_context_created(&*ev)
            }
            CdpEvent::RuntimeExecutionContextDestroyed(ev) => self
                .frame_manager
                .on_frame_execution_context_destroyed(&*ev),
            CdpEvent::RuntimeExecutionContextsCleared(ev) => {
                self.frame_manager.on_execution_context_cleared(&*ev)
            }
            CdpEvent::PageLifecycleEvent(ev) => self.frame_manager.on_page_lifecycle_event(&*ev),

            // `NetworkManager` events
            CdpEvent::FetchRequestPaused(ev) => self.network_manager.on_fetch_request_paused(&ev),
            CdpEvent::FetchAuthRequired(ev) => self.network_manager.on_fetch_auth_required(&ev),
            CdpEvent::NetworkRequestWillBeSent(ev) => {
                self.network_manager.on_request_will_be_sent(&ev)
            }
            CdpEvent::NetworkRequestServedFromCache(ev) => {
                self.network_manager.on_request_served_from_cache(&*ev)
            }
            CdpEvent::NetworkResponseReceived(ev) => self.network_manager.on_response_received(&ev),
            CdpEvent::NetworkLoadingFinished(ev) => {
                self.network_manager.on_network_loading_finished(&*ev)
            }
            CdpEvent::NetworkLoadingFailed(ev) => {
                self.network_manager.on_network_loading_failed(&*ev)
            }

            _ => {}
        }
    }

    /// Advance its state towards a completed `Target`
    pub fn poll(&mut self) -> Poll<Option<Request>> {
        todo!()
        // match &mut self.state {
        //     TargetState::InitializingFrame(cmds) => match cmds.poll() {
        //         Poll::Ready(Some((method, params))) => Poll::Ready(Request {
        //             method,
        //             session_id: self.session_id.clone().map(Into::into),
        //             params,
        //         }),
        //         Poll::Ready(None) => {
        //             self.state =
        //
        // TargetState::InitializingNetwork(self.network_manager.
        // init_commands());            return self.poll()
        //         }
        //         _ => Poll::Pending,
        //     },
        //     TargetState::InitializingNetwork(cmds) => match cmds.poll() {
        //         Poll::Ready(Some((method, params))) => Poll::Ready(Request {
        //             method,
        //             session_id: self.session_id.clone().map(Into::into),
        //             params,
        //         }),
        //         Poll::Ready(None) => {
        //             self.state =
        // TargetState::InitializingPage(Self::page_init_commands());
        //             return self.poll()
        //         }
        //         _ => Poll::Pending,
        //     },
        //     TargetState::InitializingPage(cmds) => match cmds.poll() {
        //         Poll::Ready(Some((method, params))) => Poll::Ready(Request {
        //             method,
        //             session_id: self.session_id.clone().map(Into::into),
        //             params,
        //         }),
        //         Poll::Ready(None) => {
        //             self.state = TargetState::InitializingEmulation(
        //                 self.emulation_manager.init_commands(&self.viewport),
        //             );
        //             return self.poll()
        //         }
        //         _ => Poll::Pending,
        //     },
        //     TargetState::InitializingEmulation(cmds) => match cmds.poll() {
        //         Poll::Ready(Some((method, params))) => Poll::Ready(Request {
        //             method,
        //             session_id: self.session_id.clone().map(Into::into),
        //             params,
        //         }),
        //         Poll::Ready(None) => {
        //             if self.emulation_manager.needs_reload {
        //                 // TODO start navigation
        //                 panic!("");
        //             }
        //         }
        //         _ => Poll::Pending,
        //     },
        //     _ => panic!(),
        // }
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

// TODO this can be moved into the classes?
#[derive(Debug)]
pub enum TargetState {
    Idle,
    InitializingFrame(CommandChain),
    InitializingNetwork(CommandChain),
    InitializingPage(CommandChain),
    InitializingEmulation(CommandChain),
    Navigating(
        // framemanager waitForFrameNavigation
        Navigating,
    ),
}

impl TargetState {
    fn commands_mut(&mut self) -> Option<&mut CommandChain> {
        match self {
            TargetState::Idle => None,
            TargetState::InitializingFrame(cmd) => Some(cmd),
            TargetState::InitializingNetwork(cmd) => Some(cmd),
            TargetState::InitializingPage(cmd) => Some(cmd),
            TargetState::InitializingEmulation(cmd) => Some(cmd),
            TargetState::Navigating(_) => None,
        }
    }

    fn on_response(&mut self, resp: &Response) {
        todo!()
    }
}

pub struct NavigationRequest {
    pub referer: Vec<String>,
    pub url: String,
    pub wait_until: WaitUntil,
}

#[derive(Debug)]
pub struct Navigating {
    /// Stores the command that triggered a page navigation until the response
    /// clears it.
    command: Option<Cow<'static, str>>,
    /// The deadline when the navigation is considered failed
    deadline: Instant,
    /// If this navigation was triggered via a channel
    sender: Option<Sender<Response>>,

    navigation_id: NavigationId,
}

#[derive(Debug, Copy, Clone, Hash, Eq, PartialEq)]
pub struct NavigationId(usize);

pub struct WaitUntil {
    pub started: Instant,
    pub events: HashSet<Cow<'static, str>>,
    pub timeout: Option<Duration>,
}

impl WaitUntil {
    pub fn new<I, S>(started: Instant, events: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Cow<'static, str>>,
    {
        Self {
            started,
            events: events.into_iter().map(Into::into).collect(),
            timeout: None,
        }
    }
}
