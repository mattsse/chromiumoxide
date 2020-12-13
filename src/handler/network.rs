use chromiumoxide_types::Method;

use crate::cmd::CommandChain;
use chromiumoxide_cdp::cdp::browser_protocol::fetch::{EventAuthRequired, EventRequestPaused};
use chromiumoxide_cdp::cdp::browser_protocol::network::{
    EventLoadingFailed, EventLoadingFinished, EventRequestServedFromCache, EventRequestWillBeSent,
    EventResponseReceived,
};
use chromiumoxide_cdp::cdp::browser_protocol::{
    network::EnableParams, security::SetIgnoreCertificateErrorsParams,
};

#[derive(Debug)]
pub struct NetworkManager {
    ignore_httpserrors: bool,
}

impl NetworkManager {
    pub fn new(ignore_httpserrors: bool) -> Self {
        Self { ignore_httpserrors }
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
