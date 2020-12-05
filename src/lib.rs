use crate::cdp::browser_protocol::target::{CreateTargetParams, SessionId};

// Include all the types
include!(concat!(env!("OUT_DIR"), "/cdp.rs"));

pub mod session;
// pub mod sketch;

/// convenience fixups
impl Default for CreateTargetParams {
    fn default() -> Self {
        "about:blank".into()
    }
}

pub mod browser;
pub mod conn;
pub mod element;
pub mod error;
pub mod handler;
pub mod keyboard;
pub mod page;
pub mod query;
