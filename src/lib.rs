include!(concat!(env!("OUT_DIR"), "/cdp.rs"));

use crate::cdp::browser_protocol::target::{CreateTargetParams, SessionId};

/// convenience fixups
impl<T: Into<String>> From<T> for CreateTargetParams {
    fn from(url: T) -> Self {
        CreateTargetParams::new(url)
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
