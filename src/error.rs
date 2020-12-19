use std::io;
use std::time::Instant;

use async_tungstenite::tungstenite;
use base64::DecodeError;
use futures::channel::mpsc::SendError;
use futures::channel::oneshot::Canceled;
use thiserror::Error;

use chromiumoxide_cdp::cdp::browser_protocol::page::FrameId;

use crate::handler::frame::NavigationError;

pub type Result<T, E = CdpError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum CdpError {
    #[error("{0}")]
    Ws(#[from] tungstenite::Error),
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Serde(#[from] serde_json::Error),
    #[error("{0}")]
    Chrome(#[from] chromiumoxide_types::Error),
    #[error("Received no response from the chromium instance.")]
    NoResponse,
    #[error("{0}")]
    ChannelSendError(#[from] ChannelError),
    #[error("Request timed out.")]
    Timeout,
    #[error("FrameId {0:?} not found.")]
    FrameNotFound(FrameId),
    /// Error message related to a cdp response that is not a
    /// `chromiumoxide_types::Error`
    #[error("{0}")]
    ChromeMessage(String),
    #[error("{0}")]
    DecodeError(#[from] DecodeError),
    #[error("{0}")]
    ScrollingFailed(String),
    #[error("Requested value not found.")]
    NotFound,
}
impl CdpError {
    pub fn msg(msg: impl Into<String>) -> Self {
        CdpError::ChromeMessage(msg.into())
    }
}

#[derive(Debug, Error)]
pub enum ChannelError {
    #[error("{0}")]
    Send(#[from] SendError),
    #[error("{0}")]
    Canceled(#[from] Canceled),
}

impl From<Canceled> for CdpError {
    fn from(err: Canceled) -> Self {
        ChannelError::from(err).into()
    }
}

impl From<SendError> for CdpError {
    fn from(err: SendError) -> Self {
        ChannelError::from(err).into()
    }
}

impl From<NavigationError> for CdpError {
    fn from(err: NavigationError) -> Self {
        match err {
            NavigationError::Timeout { .. } => CdpError::Timeout,
            NavigationError::FrameNotFound { frame, .. } => CdpError::FrameNotFound(frame),
        }
    }
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
    /// panics if `now > deadline`
    pub fn new(now: Instant, deadline: Instant) -> Self {
        assert!(now > deadline);
        Self { now, deadline }
    }
}
