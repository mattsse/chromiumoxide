use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt;

/// A Message sent by the client
#[derive(Serialize, Debug, PartialEq)]
pub struct MethodCall {
    /// Identifier for this method call
    ///
    /// [`MethodCall`] id's must be unique for every session
    pub id: CallId,
    #[serde(rename = "method")]
    method_name: Cow<'static, str>,
    /// Json byte vector
    params: serde_json::Value,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallId(usize);

pub trait Command: serde::ser::Serialize + Method {
    type Response: serde::de::DeserializeOwned + std::fmt::Debug;

    fn create_call(&self, call_id: CallId) -> serde_json::Result<MethodCall> {
        Ok(MethodCall {
            id: call_id,
            method_name: Self::method_name(),
            params: serde_json::to_value(self)?,
        })
    }

    fn to_vec(&self, call_id: CallId) -> serde_json::Result<Vec<u8>> {
        serde_json::to_vec(&self.create_call(call_id)?)
    }
}
/// An event produced by the Chrome instance
#[derive(Deserialize, Debug, PartialEq, Clone)]
pub struct Event {
    #[serde(rename = "method")]
    method_name: Cow<'static, str>,
    /// Json byte vector
    params: serde_json::Value,
}

pub trait Method {
    fn domain_name() -> Cow<'static, str> {
        Self::split().0
    }

    fn method_name() -> Cow<'static, str> {
        Self::split().1
    }

    fn identifier() -> Cow<'static, str>;

    fn split() -> (Cow<'static, str>, Cow<'static, str>);
}

/// A response to a [`MethodCall`] from the Chrome instance
#[derive(Deserialize, Debug, PartialEq, Clone)]
pub struct Response {
    /// matching [`MethodCall`] identifier
    pub id: CallId,
    /// The response payload
    pub result: Option<serde_json::Value>,
    /// The Reason why the [`MethodCall`] failed.
    pub error: Option<Error>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum Message {
    Event(Event),
    Response(Response),
    ConnectionShutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Error {
    /// Error code
    pub code: usize,
    /// Error Message
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for Error {}
