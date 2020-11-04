pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/cdp.rs"));
}

pub mod browser;
pub mod keyboard;
pub mod nav;
pub mod query;
pub mod transport;
