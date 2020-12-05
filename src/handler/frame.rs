use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use fnv::FnvHashMap;

use chromiumoxid_types::{CdpJsonEventMessage, Method};

use crate::cdp::browser_protocol::network::LoaderId;
use crate::cdp::browser_protocol::page::*;
use crate::cdp::browser_protocol::target::EventAttachedToTarget;
use crate::cdp::js_protocol::runtime::*;
use crate::cdp::{
    browser_protocol::page::{self, FrameId},
    events::CdpEventMessage,
    js_protocol::runtime,
};
use crate::handler::cmd::CommandChain;
use crate::handler::handler2::NAVIGATION_TIMEOUT;
use std::collections::VecDeque;

/// TODO FrameId could optimized by rolling usize based id setup, or find better
/// design for tracking child/parent
#[derive(Debug)]
pub struct Frame {
    pub parent_frame: Option<FrameId>,
    pub id: FrameId,
    pub loader_id: Option<LoaderId>,
    pub url: Option<String>,
    pub child_frames: HashSet<FrameId>,
    pub name: Option<String>,
    pub lifecycle_events: HashSet<Cow<'static, str>>,
}

impl Frame {
    pub fn new(id: FrameId) -> Self {
        Self {
            parent_frame: None,
            id,
            loader_id: None,
            url: None,
            child_frames: Default::default(),
            name: None,
            lifecycle_events: Default::default(),
        }
    }

    pub fn with_parent(id: FrameId, parent: &mut Frame) -> Self {
        parent.child_frames.insert(id.clone());
        Self {
            parent_frame: Some(parent.id.clone()),
            id,
            loader_id: None,
            url: None,
            child_frames: Default::default(),
            name: None,
            lifecycle_events: Default::default(),
        }
    }

    fn navigated(&mut self, frame: &page::Frame) {
        self.name = frame.name.clone();
        let url = if let Some(ref fragment) = frame.url_fragment {
            format!("{}{}", frame.url, fragment)
        } else {
            frame.url.clone()
        };
        self.url = Some(url);
    }

    fn navigated_within_url(&mut self, url: String) {
        self.url = Some(url)
    }

    fn on_loading_stopped(&mut self) {
        self.lifecycle_events
            .insert(EventDomContentEventFired::IDENTIFIER.into());
        self.lifecycle_events
            .insert(EventLoadEventFired::IDENTIFIER.into());
    }
}

/// Maintains the state of the pages frame and listens to events produced by
/// chromium targeting the `Target`. Also listens for events that indicate that
/// a navigation was completed
#[derive(Debug)]
pub struct FrameManager {
    main_frame: Option<FrameId>,
    frames: HashMap<FrameId, Frame>,
    /// Timeout after which an anticipated event (related to navigation) doesn't
    /// arrive results in an error
    timeout: Duration,
    /// Track currently in progress navigation
    pending_navigations: VecDeque<(NavigationRequest, NavigationWatcher)>,
    /// The currently ongoing navigation
    navigation: Option<(NavigationWatcher, Instant)>,
}

impl FrameManager {
    /// The commands to execute in order to initialize this framemanager
    pub fn init_commands() -> CommandChain {
        let enable = page::EnableParams::default();
        let get_tree = page::GetFrameTreeParams::default();
        let set_lifecycle = page::SetLifecycleEventsEnabledParams::new(true);
        let enable_runtime = runtime::EnableParams::default();
        CommandChain::new(vec![
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
        ])
    }

    pub fn main_frame(&self) -> Option<&Frame> {
        self.main_frame.as_ref().and_then(|id| self.frames.get(id))
    }

    pub fn frames(&self) -> impl Iterator<Item = &Frame> + '_ {
        self.frames.values()
    }

    pub fn frame(&self, id: &FrameId) -> Option<&Frame> {
        self.frames.get(id)
    }

    fn check_lifecycle(&self, watcher: &NavigationWatcher, frame: &Frame) -> bool {
        watcher
            .expected_lifecycle
            .iter()
            .all(|ev| frame.lifecycle_events.contains(ev))
            && frame
                .child_frames
                .iter()
                .filter_map(|f| self.frames.get(f))
                .all(|f| self.check_lifecycle(watcher, f))
    }

    fn check_lifecycle_complete(
        &self,
        watcher: &NavigationWatcher,
        frame: &Frame,
    ) -> Option<NavigationOk> {
        if !self.check_lifecycle(watcher, frame) {
            return None;
        }
        if frame.loader_id == watcher.loader_id && !watcher.same_document_navigation {
            return None;
        }
        if watcher.same_document_navigation {
            return Some(NavigationOk::SameDocumentNavigation(watcher.id));
        }
        if frame.loader_id != watcher.loader_id {
            return Some(NavigationOk::NewDocumentNavigation(watcher.id));
        }
        None
    }

    pub fn poll(&mut self, now: Instant) -> Option<FrameEvent> {
        if let Some((watcher, deadline)) = self.navigation.take() {
            if now > deadline {
                return Some(FrameEvent::NavigationResult(Err(
                    NavigationError::Timeout {
                        now,
                        deadline,
                        id: watcher.id,
                    },
                )));
            }
            if let Some(frame) = self.frames.get(&watcher.frame_id) {
                if let Some(nav) = self.check_lifecycle_complete(&watcher, frame) {
                    return Some(FrameEvent::NavigationResult(Ok(nav)));
                } else {
                    self.navigation = Some((watcher, deadline));
                }
            } else {
                return Some(FrameEvent::NavigationResult(Err(
                    NavigationError::FrameNotFound {
                        frame: watcher.frame_id,
                        id: watcher.id,
                    },
                )));
            }
        } else {
            // TODO queue in new nav if pending
            if let Some((req, watcher)) = self.pending_navigations.pop_front() {
                let mut builder = NavigateParams::builder()
                    .url(req.url)
                    .frame_id(watcher.frame_id.clone());
                if let Some(referer) = req.referer {
                    builder = builder.referrer(referer);
                }
                return Some(FrameEvent::NavigationRequest(builder.build().unwrap()));
            }
        }
        None
    }

    /// entrypoint for page navigation
    pub fn goto(&mut self, req: NavigationRequest) {
        if let Some(frame_id) = self.main_frame.clone() {
            self.navigate_frame(frame_id, req);
        }
    }

    /// Navigate a specific frame
    pub fn navigate_frame(&mut self, frame_id: FrameId, req: NavigationRequest) {
        let loader_id = self.frames.get(&frame_id).and_then(|f| f.loader_id.clone());
        let watcher = NavigationWatcher::until_page_load(req.id, frame_id, loader_id);
        self.pending_navigations.push_back((req, watcher))
    }

    /// Fired when a frame moved to another session
    pub fn on_attached_to_target(&mut self, event: &EventAttachedToTarget) {
        // _onFrameMoved
    }

    pub fn on_frame_attached(&mut self, event: &EventFrameAttached) {
        if self.frames.contains_key(&event.frame_id) {
            return;
        }
        if let Some(parent_frame) = self.frames.get_mut(&event.parent_frame_id) {
            let frame = Frame::with_parent(event.frame_id.clone(), parent_frame);
            self.frames.insert(event.frame_id.clone(), frame);
        }
    }

    pub fn on_frame_detached(&mut self, event: &EventFrameDetached) {
        self.remove_frames_recursively(&event.frame_id);
    }

    pub fn on_frame_navigated(&mut self, event: &EventFrameNavigated) {
        if event.frame.parent_id.is_some() {
            if let Some((id, mut frame)) = self.frames.remove_entry(&event.frame.id) {
                for child in &frame.child_frames {
                    self.remove_frames_recursively(child);
                }
                // this is necessary since we can't borrow mut and then remove recursively
                frame.child_frames.clear();
                frame.navigated(&event.frame);
                self.frames.insert(id, frame);
            }
        } else {
            let mut frame = if let Some(main) = self.main_frame.take() {
                // update main frame
                let mut main_frame = self.frames.remove(&main).expect("Main frame is tracked.");
                for child in &main_frame.child_frames {
                    self.remove_frames_recursively(child);
                }
                // this is necessary since we can't borrow mut and then remove recursively
                main_frame.child_frames.clear();
                main_frame.id = event.frame.id.clone();
                main_frame
            } else {
                // initial main frame navigation
                let frame = Frame::new(event.frame.id.clone());
                frame
            };
            frame.navigated(&event.frame);
            self.main_frame = Some(frame.id.clone());
            self.frames.insert(frame.id.clone(), frame);
        }
    }

    pub fn on_frame_navigated_within_document(&mut self, event: &EventNavigatedWithinDocument) {
        if let Some(frame) = self.frames.get_mut(&event.frame_id) {
            frame.navigated_within_url(event.url.clone());
        }
    }

    pub fn on_frame_stopped_loading(&mut self, event: &EventFrameStoppedLoading) {
        if let Some(frame) = self.frames.get_mut(&event.frame_id) {
            frame.on_loading_stopped();
        }
    }

    pub fn on_frame_execution_context_created(&mut self, event: &EventExecutionContextCreated) {}

    pub fn on_frame_execution_context_destroyed(&mut self, event: &EventExecutionContextDestroyed) {
    }

    pub fn on_execution_context_cleared(&mut self, event: &EventExecutionContextsCleared) {}

    /// Fired for top level page lifecycle events (nav, load, paint, etc.)
    pub fn on_page_lifecycle_event(&mut self, event: &EventLifecycleEvent) {
        if let Some(frame) = self.frames.get_mut(&event.frame_id) {
            if event.name == "init" {
                frame.loader_id = Some(event.loader_id.clone());
                frame.lifecycle_events.clear();
            }
            frame.lifecycle_events.insert(event.name.clone().into());
        }
    }

    /// Detach all child frames
    fn remove_frames_recursively(&mut self, id: &FrameId) -> Option<Frame> {
        if let Some(mut frame) = self.frames.remove(id) {
            for child in &frame.child_frames {
                self.remove_frames_recursively(child);
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
}

impl Default for FrameManager {
    fn default() -> Self {
        FrameManager {
            main_frame: None,
            frames: Default::default(),
            timeout: Duration::from_millis(NAVIGATION_TIMEOUT),
            pending_navigations: Default::default(),
            navigation: None,
        }
    }
}

#[derive(Debug)]
pub enum FrameEvent {
    NavigationResult(Result<NavigationOk, NavigationError>),
    NavigationRequest(NavigateParams),
}

#[derive(Debug)]
pub enum NavigationError {
    Timeout {
        id: NavigationId,
        now: Instant,
        deadline: Instant,
    },
    FrameNotFound {
        id: NavigationId,
        frame: FrameId,
    },
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum NavigationOk {
    SameDocumentNavigation(NavigationId),
    NewDocumentNavigation(NavigationId),
}

/// Tracks the progress of an issued `Page.navigate` request until completion.
#[derive(Debug)]
pub struct NavigationWatcher {
    id: NavigationId,
    expected_lifecycle: HashSet<Cow<'static, str>>,
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
        Self {
            id,
            expected_lifecycle: std::iter::once(EventLoadEventFired::IDENTIFIER.into()).collect(),
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

    fn on_network_request(&mut self, ev: ()) {}
}

#[derive(Debug, Copy, Clone, Hash, Eq, PartialEq)]
pub struct NavigationId(usize);

#[derive(Debug)]
pub struct NavigationRequest {
    pub id: NavigationId,
    pub referer: Option<String>,
    pub url: String,
    pub timeout: Duration,
}
