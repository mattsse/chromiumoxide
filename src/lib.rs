#![warn(missing_debug_implementations, rust_2018_idioms)]

pub use chromiumoxide_cdp::cdp;
pub use chromiumoxide_types::{self as types, Binary, Command, Method, MethodType};

pub use crate::conn::Connection;
pub use crate::error::{CdpError, Result};

pub mod conn;
pub mod error;
