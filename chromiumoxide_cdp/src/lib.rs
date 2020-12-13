use crate::cdp::browser_protocol::target::CreateTargetParams;

// Include all the types
include!(concat!(env!("OUT_DIR"), "/cdp.rs"));

/// convenience fixups
impl Default for CreateTargetParams {
    fn default() -> Self {
        "about:blank".into()
    }
}
