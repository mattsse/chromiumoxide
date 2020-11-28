use chromiumoxid_types::Method;

use crate::cdp::browser_protocol::{
    network::EnableParams, security::SetIgnoreCertificateErrorsParams,
};
use crate::handler::cmd::CommandChain;

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
}

impl Default for NetworkManager {
    fn default() -> Self {
        NetworkManager::new(true)
    }
}
