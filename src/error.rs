use std::io;

use async_tungstenite::tungstenite;
use std::time::Instant;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CdpError {
    #[error("{0}")]
    Ws(#[from] tungstenite::Error),
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Serde(#[from] serde_json::Error),
    #[error("{0}")]
    Chrome(#[from] chromiumoxid_types::Error),
    #[error("Received no response from the chromium instance.")]
    NoResponse,
}

/// An Error where `now > deadline`
#[derive(Debug, Clone)]
pub struct DeadlineExceeded {
    /// The deadline that was set.
    pub deadline: Instant,
    /// The current time
    pub now: Instant,
}

impl DeadlineExceeded {
    /// Creates a new instance
    ///
    /// panics if `now < deadline`
    pub fn new(now: Instant, deadline: Instant) -> Self {
        assert!(now < deadline);
        Self { now, deadline }
    }
}
