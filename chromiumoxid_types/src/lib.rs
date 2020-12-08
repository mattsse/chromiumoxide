use std::borrow::Cow;
use std::fmt;
use std::ops::Deref;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// A Message sent by the client
#[derive(Serialize, Debug, PartialEq)]
pub struct MethodCall {
    /// Identifier for this method call
    ///
    /// [`MethodCall`] id's must be unique for every session
    pub id: CallId,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub method: Cow<'static, str>,
    pub params: serde_json::Value,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallId(usize);

impl fmt::Display for CallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CallId({})", self.0)
    }
}

impl CallId {
    pub fn new(id: usize) -> Self {
        CallId(id)
    }
}

pub trait Command: serde::ser::Serialize + Method {
    type Response: serde::de::DeserializeOwned + fmt::Debug;

    // fn create_call(&self, call_id: CallId) -> serde_json::Result<MethodCall> {
    //     Ok(MethodCall {
    //         id: call_id,
    //         session_id: None,
    //         method: self.method_name(),
    //         params: serde_json::to_value(self)?,
    //     })
    // }
}

pub struct CommandResponse<T>
where
    T: fmt::Debug,
{
    pub id: CallId,
    pub result: T,
    pub method: Cow<'static, str>,
}

pub type CommandResult<T> = Result<CommandResponse<T>, Error>;

impl<T: fmt::Debug> Deref for CommandResponse<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.result
    }
}

#[derive(Deserialize, Debug, PartialEq, Clone)]
pub struct CdpJsonEventMessage {
    /// Name of the method
    pub method: Cow<'static, str>,
    pub session_id: Option<String>,
    /// Json params
    pub params: serde_json::Value,
}

impl Method for CdpJsonEventMessage {
    fn identifier(&self) -> Cow<'static, str> {
        self.method.clone()
    }
}

impl Event for CdpJsonEventMessage {
    fn session_id(&self) -> Option<&str> {
        self.params.get("sessionId").and_then(|x| x.as_str())
    }
}

pub trait Event: Method + DeserializeOwned {
    fn session_id(&self) -> Option<&str>;
}

pub trait Method {
    /// The whole string identifier for this method like: `DOM.removeNode`
    fn identifier(&self) -> Cow<'static, str>;

    /// The name of the domain this method belongs to: `DOM`
    fn domain_name(&self) -> Cow<'static, str> {
        self.split().0
    }

    /// The standalone identifier of the method inside the domain: `removeNode`
    fn method_name(&self) -> Cow<'static, str> {
        self.split().1
    }

    /// Tuple of (`domain_name`, `method_name`) : (`DOM`, `removeNode`)
    fn split(&self) -> (Cow<'static, str>, Cow<'static, str>) {
        match self.identifier() {
            Cow::Borrowed(id) => {
                let mut iter = id.split('.');
                (iter.next().unwrap().into(), iter.next().unwrap().into())
            }
            Cow::Owned(id) => {
                let mut iter = id.split('.');
                (
                    Cow::Owned(iter.next().unwrap().into()),
                    Cow::Owned(iter.next().unwrap().into()),
                )
            }
        }
    }
}

/// A cdp request
#[derive(Deserialize, Debug, PartialEq, Clone)]
pub struct Request {
    pub method: Cow<'static, str>,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub params: serde_json::Value,
}

impl Request {
    pub fn new(method: Cow<'static, str>, params: serde_json::Value) -> Self {
        Self {
            method,
            params,
            session_id: None,
        }
    }

    pub fn with_session(
        method: Cow<'static, str>,
        params: serde_json::Value,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            method,
            params,
            session_id: Some(session_id.into()),
        }
    }
}

/// A response to a [`MethodCall`] from the chromium instance
#[derive(Deserialize, Debug, PartialEq, Clone)]
pub struct Response {
    /// Numeric identifier for the exact request
    pub id: CallId,
    // /// The identifier for the type of request issued
    // pub method: Cow<'static, str>,
    /// The response payload
    pub result: Option<serde_json::Value>,
    /// The Reason why the [`MethodCall`] failed.
    pub error: Option<Error>,
}

// TODO impl custom deserialize https://users.rust-lang.org/t/how-to-deserialize-untagged-enums-fast/28331/4
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum Message<T = CdpJsonEventMessage> {
    Response(Response),
    Event(T),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseError {
    pub id: CallId,
    /// Error code
    pub code: usize,
    /// Error Message
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Error {
    /// Error code
    pub code: i64,
    /// Error Message
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for Error {}

/// Represents a binary type as defined in CDP
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binary(String);

impl AsRef<str> for Binary {
    fn as_ref(&self) -> &str {
        self.0.as_str()
    }
}

impl Into<String> for Binary {
    fn into(self) -> String {
        self.0
    }
}

impl From<String> for Binary {
    fn from(expr: String) -> Self {
        Self(expr)
    }
}
