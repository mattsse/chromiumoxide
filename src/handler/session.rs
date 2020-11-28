use crate::cdp::browser_protocol::target::{SessionId, TargetId};

/// Represents a Session within the cpd.
#[derive(Debug, Clone)]
pub struct Session {
    /// Identifier for this session.
    id: SessionId,
    /// The type of the target this session is attached to.
    /// Used to determine whether this is a page or worker session.
    target_type: String,
    /// The identifier of the target this session is attached to.
    target_id: TargetId,
}
