use chromiumoxide_cdp::cdp::browser_protocol::fetch::{EventAuthRequired, EventRequestPaused};
use chromiumoxide_cdp::cdp::browser_protocol::network::{
    EventLoadingFailed, EventLoadingFinished, EventRequestServedFromCache, EventRequestWillBeSent,
    EventResponseReceived,
};
use chromiumoxide_cdp::cdp::browser_protocol::{
    network::EnableParams, security::SetIgnoreCertificateErrorsParams,
};
use chromiumoxide_types::Method;

use crate::cmd::CommandChain;
use crate::handler::http::HttpRequest;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

#[derive(Debug)]
pub struct NetworkManager {
    ignore_httpserrors: bool,
    requests: HashMap<String, HttpRequest>,
    // TODO put event in an Arc?
    requests_will_be_sent: HashMap<String, EventRequestWillBeSent>,
    extra_headers: HashMap<String, String>,
    user_cache_disabled: bool,
    attempted_authentications: HashSet<String>,
    user_request_interception_enabled: bool,
    offline: bool,
}

impl NetworkManager {
    pub fn new(ignore_httpserrors: bool) -> Self {
        Self {
            ignore_httpserrors,
            requests: Default::default(),
            requests_will_be_sent: Default::default(),
            extra_headers: Default::default(),
            user_cache_disabled: false,
            attempted_authentications: Default::default(),
            user_request_interception_enabled: false,
            offline: false,
        }
    }

    pub fn init_commands(&self) -> CommandChain {
        let enable = EnableParams::default();
        if self.ignore_httpserrors {
            let ignore = SetIgnoreCertificateErrorsParams::new(true);
            CommandChain::new(vec![
                (enable.identifier(), serde_json::to_value(enable).unwrap()),
                (ignore.identifier(), serde_json::to_value(ignore).unwrap()),
            ])
        } else {
            CommandChain::new(vec![(
                enable.identifier(),
                serde_json::to_value(enable).unwrap(),
            )])
        }
    }

    pub fn poll(&mut self, _now: Instant) -> Option<NetworkEvent> {
        None
    }

    pub fn on_fetch_request_paused(&mut self, _event: &EventRequestPaused) {}

    pub fn on_fetch_auth_required(&mut self, _event: &EventAuthRequired) {}

    pub fn on_request_will_be_sent(&mut self, _event: &EventRequestWillBeSent) {}

    pub fn on_request_served_from_cache(&mut self, _event: &EventRequestServedFromCache) {}

    pub fn on_response_received(&mut self, _event: &EventResponseReceived) {}

    pub fn on_network_loading_finished(&mut self, _event: &EventLoadingFinished) {}

    pub fn on_network_loading_failed(&mut self, _event: &EventLoadingFailed) {}
}

impl Default for NetworkManager {
    fn default() -> Self {
        NetworkManager::new(true)
    }
}

#[derive(Debug)]
pub enum NetworkEvent {}
