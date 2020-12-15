use crate::cdp::browser_protocol::network::{CookieParam, DeleteCookiesParams};
use crate::cdp::browser_protocol::target::CreateTargetParams;

// Include all the types
include!(concat!(env!("OUT_DIR"), "/cdp.rs"));

/// convenience fixups
impl Default for CreateTargetParams {
    fn default() -> Self {
        "about:blank".into()
    }
}

impl DeleteCookiesParams {
    /// Create a new instance from a `CookieParam`
    pub fn from_cookie(param: &CookieParam) -> Self {
        DeleteCookiesParams {
            name: param.name.clone(),
            url: param.url.clone(),
            domain: param.domain.clone(),
            path: param.path.clone(),
        }
    }
}
