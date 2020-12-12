// use chromiumoxid_cdp::cdp::browser_protocol::target::CreateTargetParams;
//
// // Include all the types
// include!(concat!(env!("OUT_DIR"), "/cdp.rs"));
//
// /// convenience fixups
// impl Default for CreateTargetParams {
//     fn default() -> Self {
//         "about:blank".into()
//     }
// }

pub mod browser;
pub(crate) mod cmd;
pub mod conn;
pub mod element;
pub mod error;
pub mod handler;
pub mod keys;
pub mod layout;
pub mod page;
