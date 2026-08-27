use chromiumoxide_types::CallId;
use thiserror::Error;
use tungstenite::Message;

pub type Result<T, E = CdpError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum CdpError {
    #[error("{0}")]
    Ws(#[from] tungstenite::Error),
    #[error("{0}")]
    Serde(#[from] serde_json::Error),
    #[error("{0}")]
    Chrome(#[from] chromiumoxide_types::Error),
    #[error("Received unexpected ws message: {0:?}")]
    UnexpectedWsMessage(Message),
    #[error("The websocket connection was closed by the peer.")]
    ConnectionClosed,
    #[error(
        "Response id {got} did not match in-flight request id {expected}; concurrent send() is not supported."
    )]
    ResponseIdMismatch { expected: CallId, got: CallId },
    #[error("Failed to parse ws text frame: {1}")]
    InvalidMessage(String, serde_json::Error),
}
