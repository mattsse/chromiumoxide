use std::time::Duration;

use chromiumoxide::auth::Credentials;
use chromiumoxide::cdp::browser_protocol::dom::NodeId;
use chromiumoxide::cdp::browser_protocol::fetch::{EventAuthRequired, EventRequestPaused};
use chromiumoxide::cdp::browser_protocol::network::{
    EventLoadingFailed, EventLoadingFinished, EventRequestServedFromCache, EventRequestWillBeSent,
    EventResponseReceived, InterceptionId, LoaderId, RequestId,
};
use chromiumoxide::cdp::browser_protocol::page::{
    CrossOriginIsolatedContextType, EventFrameStartedLoading, EventFrameStoppedLoading,
    EventLifecycleEvent, EventNavigatedWithinDocument, Frame as CdpFrame, FrameId, FrameTree,
    GatedApiFeatures, SecureContextType,
};
use chromiumoxide::cdp::browser_protocol::target::{EventAttachedToTarget, TargetId, TargetInfo};
use chromiumoxide::cdp::js_protocol::runtime::{
    EventBindingCalled, EventExecutionContextCreated, EventExecutionContextDestroyed,
    ExecutionContextId,
};
use chromiumoxide::cmd::{CommandChain, CommandMessage};
use chromiumoxide::error::Result;
use chromiumoxide::handler::browser::BrowserContext;
use chromiumoxide::handler::domworld::DOMWorldKind;
use chromiumoxide::handler::frame::{
    FrameManager, FrameNavigationRequest, NavigationId, NavigationWatcher,
};
use chromiumoxide::handler::http::HttpRequest;
use chromiumoxide::handler::network::{NetworkEvent, NetworkManager};
use chromiumoxide::handler::target::{GetExecutionContext, Target, TargetConfig, TargetMessage};
use chromiumoxide::types::{CallId, Request, Response};
use chromiumoxide::{ContinueRequestOverrides, Element, FulfillResponse, PausedRequest};
use futures::channel::oneshot;
use serde_json::json;
use static_assertions::assert_not_impl_any;

assert_not_impl_any!(PausedRequest: Clone);

// Kept as one alias because the exact five-argument constructor signature is
// the compatibility contract being checked here.
#[allow(clippy::type_complexity)]
type HttpRequestConstructor =
    fn(RequestId, Option<FrameId>, Option<InterceptionId>, bool, Vec<HttpRequest>) -> HttpRequest;

fn assert_element_node_id_is_optional(element: &Element) {
    let _: &Option<NodeId> = &element.node_id;
}

fn exhaustively_match_target_message(message: TargetMessage) {
    match message {
        TargetMessage::Command(_) => {}
        TargetMessage::MainFrame(_) => {}
        TargetMessage::AllFrames(_) => {}
        TargetMessage::Url(_) => {}
        TargetMessage::Name(_) => {}
        TargetMessage::Parent(_) => {}
        TargetMessage::WaitForNavigation(_) => {}
        TargetMessage::AddEventListener(_) => {}
        TargetMessage::GetExecutionContext(_) => {}
        TargetMessage::Authenticate(_) => {}
    }
}

fn exhaustively_match_network_event(event: NetworkEvent) {
    match event {
        NetworkEvent::SendCdpRequest(_) => {}
        NetworkEvent::Request(_) => {}
        NetworkEvent::Response(_) => {}
        NetworkEvent::RequestFailed(_) => {}
        NetworkEvent::RequestFinished(_) => {}
    }
}

#[test]
fn legacy_public_contracts_compile() {
    let _continue_overrides = ContinueRequestOverrides::default();
    let _fulfill_response = FulfillResponse::new(200);
    let (context_tx, _context_rx) = oneshot::channel::<Option<ExecutionContextId>>();
    let context = GetExecutionContext {
        dom_world: DOMWorldKind::Main,
        frame_id: None,
        tx: context_tx,
    };
    let message = TargetMessage::GetExecutionContext(context);
    exhaustively_match_target_message(message);

    let _target_message_match: fn(TargetMessage) = exhaustively_match_target_message;
    let _network_event_match: fn(NetworkEvent) = exhaustively_match_network_event;
    let _element_node_id_contract: fn(&Element) = assert_element_node_id_is_optional;

    let _navigate_frame: fn(&mut FrameManager, FrameId, FrameNavigationRequest) =
        FrameManager::navigate_frame;
    let _until_page_load: fn(NavigationId, FrameId, Option<LoaderId>) -> NavigationWatcher =
        NavigationWatcher::until_page_load;
    let _attached_to_target: fn(&mut FrameManager, &EventAttachedToTarget) =
        FrameManager::on_attached_to_target;
    let _frame_tree: fn(&mut FrameManager, FrameTree) = FrameManager::on_frame_tree;
    let _frame_attached: fn(&mut FrameManager, FrameId, Option<FrameId>) =
        FrameManager::on_frame_attached;
    let _frame_navigated: fn(&mut FrameManager, &CdpFrame) = FrameManager::on_frame_navigated;
    let _within_document: fn(&mut FrameManager, &EventNavigatedWithinDocument) =
        FrameManager::on_frame_navigated_within_document;
    let _frame_stopped: fn(&mut FrameManager, &EventFrameStoppedLoading) =
        FrameManager::on_frame_stopped_loading;
    let _frame_started: fn(&mut FrameManager, &EventFrameStartedLoading) =
        FrameManager::on_frame_started_loading;
    let _binding_called: fn(&mut FrameManager, &EventBindingCalled) =
        FrameManager::on_runtime_binding_called;
    let _context_created: fn(&mut FrameManager, &EventExecutionContextCreated) =
        FrameManager::on_frame_execution_context_created;
    let _context_destroyed: fn(&mut FrameManager, &EventExecutionContextDestroyed) =
        FrameManager::on_frame_execution_context_destroyed;
    let _contexts_cleared: fn(&mut FrameManager) = FrameManager::on_execution_contexts_cleared;
    let _lifecycle: fn(&mut FrameManager, &EventLifecycleEvent) =
        FrameManager::on_page_lifecycle_event;
    let _isolated_world: fn(&mut FrameManager, &str) -> Option<CommandChain> =
        FrameManager::ensure_isolated_world;

    let _fetch_paused: fn(&mut NetworkManager, &EventRequestPaused) =
        NetworkManager::on_fetch_request_paused;
    let _auth_required: fn(&mut NetworkManager, &EventAuthRequired) =
        NetworkManager::on_fetch_auth_required;
    let _request_will_be_sent: fn(&mut NetworkManager, &EventRequestWillBeSent) =
        NetworkManager::on_request_will_be_sent;
    let _served_from_cache: fn(&mut NetworkManager, &EventRequestServedFromCache) =
        NetworkManager::on_request_served_from_cache;
    let _response_received: fn(&mut NetworkManager, &EventResponseReceived) =
        NetworkManager::on_response_received;
    let _loading_finished: fn(&mut NetworkManager, &EventLoadingFinished) =
        NetworkManager::on_network_loading_finished;
    let _loading_failed: fn(&mut NetworkManager, &EventLoadingFailed) =
        NetworkManager::on_network_loading_failed;

    let _target_response: fn(&mut Target, Response, &str) = Target::on_response;
    let _http_request_new: HttpRequestConstructor = HttpRequest::new;

    let (command_tx, _command_rx) = oneshot::channel::<Result<Response>>();
    let command = CommandMessage {
        method: "Runtime.evaluate".into(),
        session_id: None,
        params: json!({}),
        sender: command_tx,
    };
    exhaustively_match_target_message(TargetMessage::Command(command));
    exhaustively_match_target_message(TargetMessage::Authenticate(Credentials {
        username: String::new(),
        password: String::new(),
    }));
}

#[test]
fn legacy_standalone_handlers_do_not_require_session_binding() {
    let mut frames = FrameManager::new(Duration::from_millis(100));
    let cdp_frame = CdpFrame::builder()
        .id(FrameId::new("frame"))
        .loader_id(LoaderId::new("loader"))
        .url("about:blank")
        .domain_and_registry("")
        .security_origin("://")
        .mime_type("text/html")
        .secure_context_type(SecureContextType::SecureLocalhost)
        .cross_origin_isolated_context_type(CrossOriginIsolatedContextType::NotIsolated)
        .gated_api_features(Vec::<GatedApiFeatures>::new())
        .build()
        .expect("frame fixture has all mandatory fields");
    frames.on_frame_navigated(&cdp_frame);
    frames.on_execution_contexts_cleared();
    frames.navigate_frame(
        FrameId::new("frame"),
        FrameNavigationRequest::new(
            chromiumoxide::handler::frame::NavigationId(1),
            Request::new("Page.navigate".into(), json!({ "url": "about:blank" })),
        ),
    );

    let info = TargetInfo::builder()
        .target_id(TargetId::new("target"))
        .r#type("page")
        .title("test")
        .url("about:blank")
        .attached(true)
        .can_access_opener(false)
        .build()
        .expect("target fixture has all mandatory fields");
    let mut target = Target::new(info, TargetConfig::default(), BrowserContext::default());
    target.on_response(
        Response {
            id: CallId::new(1),
            result: None,
            error: None,
        },
        "Runtime.enable",
    );
}
