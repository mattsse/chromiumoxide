pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/browser_protocol.rs"));
    include!(concat!(env!("OUT_DIR"), "/js_protocol.rs"));
}

pub mod browser;
pub mod keyboard;
pub mod nav;
pub mod query;
pub mod transport;
