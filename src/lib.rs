// Include all the types
include!(concat!(env!("OUT_DIR"), "/cdp.rs"));

use crate::cdp::browser_protocol::network::SetUserAgentOverrideParams;
use crate::cdp::browser_protocol::page::NavigateParams;
use crate::cdp::browser_protocol::target::CreateTargetParams;
use crate::cdp::js_protocol::runtime::EvaluateParams;

/// convenience fixups
impl<T: Into<String>> From<T> for CreateTargetParams {
    fn from(url: T) -> Self {
        CreateTargetParams::new(url)
    }
}

impl<T: Into<String>> From<T> for NavigateParams {
    fn from(url: T) -> Self {
        NavigateParams::new(url)
    }
}

impl<T: Into<String>> From<T> for SetUserAgentOverrideParams {
    fn from(user_agent: T) -> Self {
        SetUserAgentOverrideParams::new(user_agent)
    }
}

impl Default for CreateTargetParams {
    fn default() -> Self {
        "about:blank".into()
    }
}

impl<T: Into<String>> From<T> for EvaluateParams {
    fn from(expr: T) -> Self {
        EvaluateParams::new(expr.into())
    }
}

pub mod browser;
pub mod conn;
pub mod context;
pub mod element;
pub mod error;
pub mod keyboard;
pub mod nav;
pub mod query;
pub mod tab;
