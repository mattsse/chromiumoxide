use chromiumoxide_cdp::cdp::js_protocol::runtime::RemoteObject;
use serde::de::DeserializeOwned;
use std::fmt;

#[derive(Debug, Clone)]
pub struct JsFunction {}

impl fmt::Display for JsFunction {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        unimplemented!()
    }
}

#[derive(Debug, Clone)]
pub struct EvaluationResult {
    /// Mirror object referencing original JavaScript object
    inner: RemoteObject,
}

impl EvaluationResult {
    pub fn new(inner: RemoteObject) -> Self {
        Self { inner }
    }

    pub fn object(&self) -> &RemoteObject {
        &self.inner
    }

    /// Attempts to deserialize the value into the given type
    pub fn into_value<T: DeserializeOwned>(self) -> serde_json::Result<T> {
        let value = self
            .inner
            .value
            .ok_or_else(|| serde::de::Error::custom("No value found"))?;
        serde_json::from_value(value)
    }
}
