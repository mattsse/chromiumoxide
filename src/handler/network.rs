use chromiumoxide_cdp::cdp::browser_protocol::fetch::{
    self, AuthChallengeResponse, AuthChallengeResponseResponse, ContinueRequestParams,
    ContinueWithAuthParams, DisableParams, EventAuthRequired, EventRequestPaused, RequestPattern,
};
#[allow(deprecated)]
use chromiumoxide_cdp::cdp::browser_protocol::network::{
    EmulateNetworkConditionsParams, EventLoadingFailed, EventLoadingFinished,
    EventRequestServedFromCache, EventRequestWillBeSent, EventResponseReceived, Headers,
    InterceptionId, RequestId, Response, SetCacheDisabledParams, SetExtraHttpHeadersParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::target::SessionId;
use chromiumoxide_cdp::cdp::browser_protocol::{
    network::EnableParams, security::SetIgnoreCertificateErrorsParams,
};
use chromiumoxide_types::{Command, Method, MethodId, Request};

use crate::auth::Credentials;
use crate::cmd::CommandChain;
use crate::handler::http::HttpRequest;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

pub(crate) type NetworkCommand = (MethodId, serde_json::Value);

#[derive(Debug)]
pub struct NetworkManager {
    queued_events: VecDeque<QueuedNetworkEvent>,
    session_requests: VecDeque<SessionCdpRequest>,
    ignore_httpserrors: bool,
    requests: HashMap<RequestId, HttpRequest>,
    // TODO put event in an Arc?
    requests_will_be_sent: HashMap<RequestId, RequestWillBeSent>,
    extra_headers: HashMap<String, String>,
    request_id_to_interception_id: HashMap<RequestId, InterceptionId>,
    user_cache_disabled: bool,
    attempted_authentications: HashSet<RequestId>,
    credentials: Option<Credentials>,
    user_request_interception_enabled: bool,
    protocol_request_interception_enabled: bool,
    offline: bool,
    request_timeout: Duration,
}

impl NetworkManager {
    pub fn new(ignore_httpserrors: bool, request_timeout: Duration) -> Self {
        Self {
            queued_events: Default::default(),
            session_requests: Default::default(),
            ignore_httpserrors,
            requests: Default::default(),
            requests_will_be_sent: Default::default(),
            extra_headers: Default::default(),
            request_id_to_interception_id: Default::default(),
            user_cache_disabled: false,
            attempted_authentications: Default::default(),
            credentials: None,
            user_request_interception_enabled: false,
            protocol_request_interception_enabled: false,
            offline: false,
            request_timeout,
        }
    }

    pub fn init_commands(&self) -> CommandChain {
        let enable = EnableParams::default();
        let cmds = if self.ignore_httpserrors {
            let ignore = SetIgnoreCertificateErrorsParams::new(true);
            vec![
                (enable.identifier(), serde_json::to_value(enable).unwrap()),
                (ignore.identifier(), serde_json::to_value(ignore).unwrap()),
            ]
        } else {
            vec![(enable.identifier(), serde_json::to_value(enable).unwrap())]
        };
        CommandChain::new(cmds, self.request_timeout)
    }

    fn push_cdp_request<T: Command>(&mut self, cmd: T) {
        let (method, params) = Self::serialize_command(cmd);
        self.push_event(NetworkEvent::SendCdpRequest((method, params)), None);
    }

    fn push_cdp_requests(&mut self, commands: Vec<NetworkCommand>) {
        for command in commands {
            self.push_event(NetworkEvent::SendCdpRequest(command), None);
        }
    }

    fn serialize_command<T: Command>(cmd: T) -> NetworkCommand {
        (
            cmd.identifier(),
            serde_json::to_value(cmd).expect("Command should not panic"),
        )
    }

    pub(crate) fn push_cdp_request_session<T: Command>(&mut self, cmd: T, session_id: SessionId) {
        self.session_requests.push_back(SessionCdpRequest {
            method: cmd.identifier(),
            params: serde_json::to_value(cmd).expect("Command should not panic"),
            session_id,
        });
    }

    fn push_event(&mut self, event: NetworkEvent, session_id: Option<SessionId>) {
        self.queued_events
            .push_back(QueuedNetworkEvent { event, session_id });
    }

    /// The next event to handle
    pub fn poll(&mut self) -> Option<NetworkEvent> {
        self.queued_events.pop_front().map(|queued| queued.event)
    }

    pub(crate) fn poll_session_request(&mut self) -> Option<Request> {
        self.session_requests.pop_front().map(|request| Request {
            method: request.method,
            session_id: Some(request.session_id.into()),
            params: request.params,
        })
    }

    pub fn extra_headers(&self) -> &HashMap<String, String> {
        &self.extra_headers
    }

    pub(crate) fn ignore_https_errors(&self) -> bool {
        self.ignore_httpserrors
    }

    pub(crate) fn is_cache_disabled(&self) -> bool {
        self.user_cache_disabled || self.protocol_request_interception_enabled
    }

    pub(crate) fn is_request_interception_enabled(&self) -> bool {
        self.protocol_request_interception_enabled
    }

    pub(crate) fn credentials(&self) -> Option<&Credentials> {
        self.credentials.as_ref()
    }

    pub(crate) fn is_offline(&self) -> bool {
        self.offline
    }

    pub fn set_extra_headers(&mut self, headers: HashMap<String, String>) {
        let commands = self.set_extra_headers_core(headers);
        self.push_cdp_requests(commands);
    }

    fn set_extra_headers_core(&mut self, headers: HashMap<String, String>) -> Vec<NetworkCommand> {
        self.extra_headers = headers;
        let headers = serde_json::to_value(self.extra_headers.clone()).unwrap();
        vec![Self::serialize_command(SetExtraHttpHeadersParams::new(
            Headers::new(headers),
        ))]
    }

    pub fn set_request_interception(&mut self, enabled: bool) {
        let commands = self.set_request_interception_core(enabled);
        self.push_cdp_requests(commands);
    }

    pub(crate) fn set_request_interception_core(&mut self, enabled: bool) -> Vec<NetworkCommand> {
        self.user_request_interception_enabled = enabled;
        self.update_protocol_request_interception_core()
    }

    pub fn set_cache_enabled(&mut self, enabled: bool) {
        let commands = self.set_cache_enabled_core(enabled);
        self.push_cdp_requests(commands);
    }

    fn set_cache_enabled_core(&mut self, enabled: bool) -> Vec<NetworkCommand> {
        self.user_cache_disabled = !enabled;
        self.update_protocol_cache_disabled_core()
    }

    pub fn update_protocol_cache_disabled(&mut self) {
        let commands = self.update_protocol_cache_disabled_core();
        self.push_cdp_requests(commands);
    }

    fn update_protocol_cache_disabled_core(&self) -> Vec<NetworkCommand> {
        vec![Self::serialize_command(SetCacheDisabledParams::new(
            self.user_cache_disabled || self.protocol_request_interception_enabled,
        ))]
    }

    pub fn authenticate(&mut self, credentials: Credentials) {
        let commands = self.authenticate_core(credentials);
        self.push_cdp_requests(commands);
    }

    pub(crate) fn authenticate_core(&mut self, credentials: Credentials) -> Vec<NetworkCommand> {
        self.credentials = Some(credentials);
        self.update_protocol_request_interception_core()
    }

    fn update_protocol_request_interception_core(&mut self) -> Vec<NetworkCommand> {
        let enabled = self.user_request_interception_enabled || self.credentials.is_some();
        // Always re-emit the idempotent batch, even when the desired state
        // equals our current belief. The belief is set optimistically here
        // before Chrome ACKs and is never rolled back on ACK failure, so a
        // short-circuit on equality would make a same-value retry after a failed
        // enable produce an empty batch and resolve `Ok(())` without Chrome ever
        // applying it (a silent false success on the response-confirmed
        // `set_request_interception`/`authenticate` contract). `Fetch.enable`,
        // `Fetch.disable`, and `Network.setCacheDisabled` are all idempotent, so
        // re-sending is safe and guarantees every awaited call waits for a real
        // ACK. (I-020.)
        self.protocol_request_interception_enabled = enabled;
        let mut commands = self.update_protocol_cache_disabled_core();
        if enabled {
            commands.push(Self::serialize_command(
                fetch::EnableParams::builder()
                    .handle_auth_requests(true)
                    .pattern(RequestPattern::builder().url_pattern("*").build())
                    .build(),
            ));
        } else {
            commands.push(Self::serialize_command(DisableParams::default()));
        }
        commands
    }

    pub fn on_fetch_request_paused(&mut self, event: &EventRequestPaused) {
        if !self.user_request_interception_enabled && self.protocol_request_interception_enabled {
            self.push_cdp_request(ContinueRequestParams::new(event.request_id.clone()))
        }
        self.correlate_request_paused(event);
    }

    pub(crate) fn on_fetch_request_paused_in_session(
        &mut self,
        event: &EventRequestPaused,
        session_id: &SessionId,
    ) -> PauseDisposition {
        let disposition = if !self.user_request_interception_enabled
            && self.protocol_request_interception_enabled
        {
            self.push_cdp_request_session(
                ContinueRequestParams::new(event.request_id.clone()),
                session_id.clone(),
            );
            PauseDisposition::AutoResponded
        } else {
            PauseDisposition::UserIntercept
        };
        self.correlate_request_paused(event);
        disposition
    }

    fn correlate_request_paused(&mut self, event: &EventRequestPaused) {
        if let Some(network_id) = event.network_id.as_ref() {
            if let Some(request_will_be_sent) =
                self.requests_will_be_sent.remove(network_id.as_ref())
            {
                self.on_request(
                    &request_will_be_sent.event,
                    Some(event.request_id.clone().into()),
                    request_will_be_sent.session_id,
                );
            } else {
                self.request_id_to_interception_id
                    .insert(network_id.clone(), event.request_id.clone().into());
            }
        }
    }

    pub fn on_fetch_auth_required(&mut self, event: &EventAuthRequired) {
        let auth = self.auth_challenge_response(event);
        self.push_cdp_request(ContinueWithAuthParams::new(event.request_id.clone(), auth));
    }

    pub(crate) fn on_fetch_auth_required_in_session(
        &mut self,
        event: &EventAuthRequired,
        session_id: &SessionId,
    ) {
        let auth = self.auth_challenge_response(event);
        self.push_cdp_request_session(
            ContinueWithAuthParams::new(event.request_id.clone(), auth),
            session_id.clone(),
        );
    }

    fn auth_challenge_response(&mut self, event: &EventAuthRequired) -> AuthChallengeResponse {
        let response = if self
            .attempted_authentications
            .contains(event.request_id.as_ref())
        {
            AuthChallengeResponseResponse::CancelAuth
        } else if self.credentials.is_some() {
            self.attempted_authentications
                .insert(event.request_id.clone().into());
            AuthChallengeResponseResponse::ProvideCredentials
        } else {
            AuthChallengeResponseResponse::Default
        };

        let mut auth = AuthChallengeResponse::new(response);
        if let Some(creds) = self.credentials.clone() {
            auth.username = Some(creds.username);
            auth.password = Some(creds.password);
        }
        auth
    }

    pub fn set_offline_mode(&mut self, value: bool) {
        let commands = self.set_offline_mode_core(value);
        self.push_cdp_requests(commands);
    }

    fn set_offline_mode_core(&mut self, value: bool) -> Vec<NetworkCommand> {
        // Safe today: this mutator is legacy fire-and-forget (no fan-out ACK), so
        // an equality short-circuit cannot cause a false-confirmed await. If
        // Phase 2 gives `set_offline_mode` dynamic fan-out + ACK (design §5.5),
        // this short-circuit MUST be removed like I-020 did for interception, or
        // a same-value retry after a failed ACK will silently report success.
        if self.offline == value {
            return Vec::new();
        }
        self.offline = value;
        vec![Self::serialize_command(
            // This event was recently deprecated, so we continue to use it for now
            // if some users are on older versions of chromium.
            #[allow(deprecated)]
            EmulateNetworkConditionsParams::builder()
                .offline(self.offline)
                .latency(0)
                .download_throughput(-1.)
                .upload_throughput(-1.)
                .build()
                .unwrap(),
        )]
    }

    /// Request interception doesn't happen for data URLs with Network Service.
    pub fn on_request_will_be_sent(&mut self, event: &EventRequestWillBeSent) {
        self.on_request_will_be_sent_core(event, None);
    }

    pub(crate) fn on_request_will_be_sent_in_session(
        &mut self,
        event: &EventRequestWillBeSent,
        session_id: &SessionId,
    ) {
        self.on_request_will_be_sent_core(event, Some(session_id.clone()));
    }

    fn on_request_will_be_sent_core(
        &mut self,
        event: &EventRequestWillBeSent,
        session_id: Option<SessionId>,
    ) {
        if self.protocol_request_interception_enabled && !event.request.url.starts_with("data:") {
            if let Some(interception_id) = self
                .request_id_to_interception_id
                .remove(event.request_id.as_ref())
            {
                self.on_request(event, Some(interception_id), session_id);
            } else {
                // TODO remove the clone for event
                self.requests_will_be_sent.insert(
                    event.request_id.clone(),
                    RequestWillBeSent {
                        event: event.clone(),
                        session_id,
                    },
                );
            }
        } else {
            self.on_request(event, None, session_id);
        }
    }

    pub fn on_request_served_from_cache(&mut self, event: &EventRequestServedFromCache) {
        if let Some(request) = self.requests.get_mut(event.request_id.as_ref()) {
            request.from_memory_cache = true;
        }
    }

    pub fn on_response_received(&mut self, event: &EventResponseReceived) {
        if let Some(mut request) = self.requests.remove(event.request_id.as_ref()) {
            request.set_response(event.response.clone());
            let session_id = request.session_id().cloned();
            self.push_event(NetworkEvent::RequestFinished(request), session_id)
        }
    }

    pub fn on_network_loading_finished(&mut self, event: &EventLoadingFinished) {
        if let Some(request) = self.requests.remove(event.request_id.as_ref()) {
            if let Some(interception_id) = request.interception_id.as_ref() {
                self.attempted_authentications
                    .remove(interception_id.as_ref());
            }
            let session_id = request.session_id().cloned();
            self.push_event(NetworkEvent::RequestFinished(request), session_id);
        }
    }

    pub fn on_network_loading_failed(&mut self, event: &EventLoadingFailed) {
        if let Some(mut request) = self.requests.remove(event.request_id.as_ref()) {
            request.failure_text = Some(event.error_text.clone());
            if let Some(interception_id) = request.interception_id.as_ref() {
                self.attempted_authentications
                    .remove(interception_id.as_ref());
            }
            let session_id = request.session_id().cloned();
            self.push_event(NetworkEvent::RequestFailed(request), session_id);
        }
    }

    pub(crate) fn on_session_draining(&mut self, session_id: &SessionId) {
        self.requests
            .retain(|_, request| request.session_id() != Some(session_id));
        self.requests_will_be_sent
            .retain(|_, request| request.session_id.as_ref() != Some(session_id));
        self.queued_events
            .retain(|event| event.session_id.as_ref() != Some(session_id));
        self.session_requests
            .retain(|request| &request.session_id != session_id);
    }

    pub(crate) fn on_session_detached(&mut self, session_id: &SessionId) {
        self.on_session_draining(session_id);
    }

    fn on_request(
        &mut self,
        event: &EventRequestWillBeSent,
        interception_id: Option<InterceptionId>,
        session_id: Option<SessionId>,
    ) {
        let mut redirect_chain = Vec::new();
        if let Some(redirect_resp) = event.redirect_response.as_ref() {
            if let Some(mut request) = self.requests.remove(event.request_id.as_ref()) {
                self.handle_request_redirect(&mut request, redirect_resp.clone());
                redirect_chain = std::mem::take(&mut request.redirect_chain);
                redirect_chain.push(request);
            }
        }
        let request = match session_id.clone() {
            Some(session_id) => HttpRequest::new_with_session(
                event.request_id.clone(),
                event.frame_id.clone(),
                interception_id,
                self.user_request_interception_enabled,
                redirect_chain,
                session_id,
            ),
            None => HttpRequest::new(
                event.request_id.clone(),
                event.frame_id.clone(),
                interception_id,
                self.user_request_interception_enabled,
                redirect_chain,
            ),
        };

        self.requests.insert(event.request_id.clone(), request);
        self.push_event(NetworkEvent::Request(event.request_id.clone()), session_id);
    }

    fn handle_request_redirect(&mut self, request: &mut HttpRequest, response: Response) {
        request.set_response(response);
        if let Some(interception_id) = request.interception_id.as_ref() {
            self.attempted_authentications
                .remove(interception_id.as_ref());
        }
    }
}

#[derive(Debug)]
struct SessionCdpRequest {
    method: MethodId,
    params: serde_json::Value,
    session_id: SessionId,
}

#[derive(Debug)]
struct QueuedNetworkEvent {
    event: NetworkEvent,
    session_id: Option<SessionId>,
}

#[derive(Debug, Clone)]
struct RequestWillBeSent {
    event: EventRequestWillBeSent,
    session_id: Option<SessionId>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum PauseDisposition {
    AutoResponded,
    UserIntercept,
}

#[derive(Debug)]
pub enum NetworkEvent {
    SendCdpRequest((MethodId, serde_json::Value)),
    Request(RequestId),
    Response(RequestId),
    RequestFailed(HttpRequest),
    RequestFinished(HttpRequest),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn session(id: &str) -> SessionId {
        SessionId::new(id)
    }

    fn network_request(url: &str) -> serde_json::Value {
        json!({
            "url": url,
            "method": "GET",
            "headers": {},
            "initialPriority": "High",
            "referrerPolicy": "no-referrer"
        })
    }

    fn request_will_be_sent(id: &str) -> EventRequestWillBeSent {
        serde_json::from_value(json!({
            "requestId": id,
            "loaderId": format!("loader-{id}"),
            "documentURL": format!("https://{id}.example/"),
            "request": network_request(&format!("https://{id}.example/")),
            "timestamp": 1.0,
            "wallTime": 1.0,
            "initiator": { "type": "other" },
            "redirectHasExtraInfo": false
        }))
        .expect("requestWillBeSent fixture is valid")
    }

    fn paused_request(fetch_id: &str, network_id: Option<&str>) -> EventRequestPaused {
        serde_json::from_value(json!({
            "requestId": fetch_id,
            "request": network_request("https://paused.example/"),
            "frameId": "frame",
            "resourceType": "Document",
            "networkId": network_id
        }))
        .expect("requestPaused fixture is valid")
    }

    fn queued_methods(manager: &mut NetworkManager) -> Vec<String> {
        let mut methods = Vec::new();
        while let Some(event) = manager.poll() {
            if let NetworkEvent::SendCdpRequest((method, _)) = event {
                methods.push(method.as_ref().to_owned());
            }
        }
        methods
    }

    #[test]
    fn interception_core_returns_exact_batch_without_legacy_queue_side_effects() {
        let mut manager = NetworkManager::new(false, Duration::from_secs(1));

        let commands = manager.set_request_interception_core(true);
        assert_eq!(
            commands
                .iter()
                .map(|(method, _)| method.as_ref())
                .collect::<Vec<_>>(),
            vec!["Network.setCacheDisabled", "Fetch.enable"]
        );
        assert_eq!(commands[0].1["cacheDisabled"], true);
        assert!(manager.poll().is_none());
        // A same-value repeat must re-emit the idempotent batch rather than
        // short-circuit to empty: the empty path resolves the awaited call
        // `Ok(())` without Chrome re-applying it, which after a failed enable is
        // a silent false success (I-020).
        assert_eq!(
            manager
                .set_request_interception_core(true)
                .iter()
                .map(|(method, _)| method.as_ref())
                .collect::<Vec<_>>(),
            vec!["Network.setCacheDisabled", "Fetch.enable"]
        );

        let commands = manager.set_request_interception_core(false);
        assert_eq!(
            commands
                .iter()
                .map(|(method, _)| method.as_ref())
                .collect::<Vec<_>>(),
            vec!["Network.setCacheDisabled", "Fetch.disable"]
        );
        assert_eq!(commands[0].1["cacheDisabled"], false);
        assert!(manager.poll().is_none());
    }

    #[test]
    fn legacy_mutator_wrappers_queue_each_core_command_once() {
        let mut manager = NetworkManager::new(false, Duration::from_secs(1));
        manager.set_request_interception(true);
        assert_eq!(
            queued_methods(&mut manager),
            vec!["Network.setCacheDisabled", "Fetch.enable"]
        );

        manager.authenticate(Credentials {
            username: "user".to_owned(),
            password: "pass".to_owned(),
        });
        // Interception is already on, but authenticate still re-emits the
        // idempotent batch (I-020: no equality short-circuit), so the awaited
        // call always waits for a real ACK instead of a false success.
        assert_eq!(
            queued_methods(&mut manager),
            vec!["Network.setCacheDisabled", "Fetch.enable"]
        );

        let mut auth_manager = NetworkManager::new(false, Duration::from_secs(1));
        let commands = auth_manager.authenticate_core(Credentials {
            username: "user".to_owned(),
            password: "pass".to_owned(),
        });
        assert_eq!(
            commands
                .iter()
                .map(|(method, _)| method.as_ref())
                .collect::<Vec<_>>(),
            vec!["Network.setCacheDisabled", "Fetch.enable"]
        );
        assert!(auth_manager.poll().is_none());
    }

    #[test]
    fn session_cleanup_removes_only_matching_private_state() {
        let mut manager = NetworkManager::new(false, Duration::from_secs(1));
        let dead = session("dead");
        let live = session("live");
        let dead_request_id = RequestId::new("dead-request");
        let live_request_id = RequestId::new("live-request");

        manager.requests.insert(
            dead_request_id.clone(),
            HttpRequest::new_with_session(
                dead_request_id.clone(),
                None,
                None,
                false,
                Vec::new(),
                dead.clone(),
            ),
        );
        manager.requests.insert(
            live_request_id.clone(),
            HttpRequest::new_with_session(
                live_request_id.clone(),
                None,
                None,
                false,
                Vec::new(),
                live.clone(),
            ),
        );
        manager.requests_will_be_sent.insert(
            dead_request_id.clone(),
            RequestWillBeSent {
                event: request_will_be_sent("dead-request"),
                session_id: Some(dead.clone()),
            },
        );
        manager.requests_will_be_sent.insert(
            live_request_id.clone(),
            RequestWillBeSent {
                event: request_will_be_sent("live-request"),
                session_id: Some(live.clone()),
            },
        );
        manager.push_event(
            NetworkEvent::Request(dead_request_id.clone()),
            Some(dead.clone()),
        );
        manager.push_event(
            NetworkEvent::Request(live_request_id.clone()),
            Some(live.clone()),
        );
        manager.push_cdp_request_session(
            ContinueRequestParams::new(
                chromiumoxide_cdp::cdp::browser_protocol::fetch::RequestId::new("dead-fetch"),
            ),
            dead.clone(),
        );
        manager.push_cdp_request_session(
            ContinueRequestParams::new(
                chromiumoxide_cdp::cdp::browser_protocol::fetch::RequestId::new("live-fetch"),
            ),
            live.clone(),
        );

        manager.on_session_draining(&dead);
        manager.on_session_detached(&dead);

        assert!(!manager.requests.contains_key(&dead_request_id));
        assert!(manager.requests.contains_key(&live_request_id));
        assert!(!manager.requests_will_be_sent.contains_key(&dead_request_id));
        assert!(manager.requests_will_be_sent.contains_key(&live_request_id));
        let session_request = manager
            .poll_session_request()
            .expect("the live session request survives");
        assert_eq!(session_request.session_id.as_deref(), Some(live.as_ref()));
        assert!(manager.poll_session_request().is_none());
        assert!(matches!(
            manager.poll(),
            Some(NetworkEvent::Request(request_id)) if request_id == live_request_id
        ));
        assert!(manager.poll().is_none());
    }

    #[test]
    fn session_cleanup_preserves_request_will_be_sent_creation_stamp() {
        let mut manager = NetworkManager::new(false, Duration::from_secs(1));
        manager.set_request_interception(true);
        while manager.poll().is_some() {}
        let parent = session("parent");
        let child = session("child");
        let request = request_will_be_sent("network-request");
        let paused = paused_request("fetch-request", Some("network-request"));

        manager.on_request_will_be_sent_in_session(&request, &parent);
        assert_eq!(
            manager.on_fetch_request_paused_in_session(&paused, &child),
            PauseDisposition::UserIntercept
        );

        let stored = manager
            .requests
            .get(&RequestId::new("network-request"))
            .expect("correlated request is stored");
        assert_eq!(stored.session_id(), Some(&parent));
        assert!(matches!(
            manager.queued_events.front(),
            Some(QueuedNetworkEvent {
                event: NetworkEvent::Request(request_id),
                session_id: Some(session_id),
            }) if request_id.as_ref() == "network-request" && session_id == &parent
        ));

        manager.on_session_draining(&child);
        assert!(
            manager
                .requests
                .contains_key(&RequestId::new("network-request"))
        );
        manager.on_session_draining(&parent);
        assert!(
            !manager
                .requests
                .contains_key(&RequestId::new("network-request"))
        );
        assert!(manager.queued_events.is_empty());
    }

    #[test]
    fn session_cleanup_auto_response_keeps_exact_pause_session() {
        let mut manager = NetworkManager::new(false, Duration::from_secs(1));
        manager.authenticate(Credentials {
            username: "user".to_owned(),
            password: "pass".to_owned(),
        });
        while manager.poll().is_some() {}
        let child = session("child");
        let paused = paused_request("fetch-request", None);

        assert_eq!(
            manager.on_fetch_request_paused_in_session(&paused, &child),
            PauseDisposition::AutoResponded
        );
        let request = manager
            .poll_session_request()
            .expect("auto response is queued");
        assert_eq!(request.method.as_ref(), "Fetch.continueRequest");
        assert_eq!(request.session_id.as_deref(), Some(child.as_ref()));

        manager.push_cdp_request_session(
            ContinueRequestParams::new(
                chromiumoxide_cdp::cdp::browser_protocol::fetch::RequestId::new("late"),
            ),
            child.clone(),
        );
        manager.on_session_detached(&child);
        assert!(manager.poll_session_request().is_none());
    }
}
