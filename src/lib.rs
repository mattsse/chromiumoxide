//! Blocking Chrome DevTools Protocol wire client.
//!
//! This crate exposes only three things:
//!
//! - The generated CDP command / response / event types, re-exported from
//!   [`chromiumoxide_cdp::cdp`].
//! - The shared base types ([`Command`], [`Method`], [`MethodType`], …)
//!   re-exported from [`chromiumoxide_types`].
//! - A blocking [`Connection`] that speaks CDP over a WebSocket.
//!
//! The caller is responsible for launching Chromium (e.g. `chromium
//! --remote-debugging-port=9222`) and locating the devtools WebSocket URL.

#![warn(missing_debug_implementations, rust_2018_idioms)]

pub use chromiumoxide_cdp::cdp;
pub use chromiumoxide_types::{self as types, Binary, Command, Method, MethodType};

pub use crate::conn::Connection;
pub use crate::error::{CdpError, Result};

pub mod conn;
pub mod error;
