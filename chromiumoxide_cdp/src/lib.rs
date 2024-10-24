use std::fmt;

use crate::cdp::browser_protocol::fetch;
use crate::cdp::browser_protocol::network::{self, CookieParam, DeleteCookiesParams};
use crate::cdp::browser_protocol::target::CreateTargetParams;
use crate::cdp::js_protocol::runtime::{
    CallFunctionOnParams, EvaluateParams, ExceptionDetails, StackTrace,
};
use crate::revision::Revision;

#[allow(clippy::multiple_bound_locations)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[allow(unreachable_patterns)]
pub mod cdp;
pub mod revision;

// The CDP is not a stable API, it changes from time to time and sometimes
// in backward incompatible ways.
//
// When the CDP changes, the chromium team pushes a commit to the repository
// https://github.com/ChromeDevTools/devtools-protocol. There you can find
// valid CDP revisions. That number corresponds to a chromium revision.
// It is a monotonic version number referring to the chromium master commit position.
//
// To map a revision to a chromium version you can use the site
// https://chromiumdash.appspot.com/commits. We should not necessarily
// always use the latest revision, as this will mean only the newest chromium
// browser can be used. Apart from breaking changes, using an older CDP
// is generally a good idea.

/// Currently built CDP revision
pub const CURRENT_REVISION: Revision = Revision(1354347);

/// convenience fixups
impl Default for CreateTargetParams {
    fn default() -> Self {
        "about:blank".into()
    }
}

/// RequestId conversion

impl From<fetch::RequestId> for network::RequestId {
    fn from(req: fetch::RequestId) -> Self {
        let s: String = req.into();
        s.into()
    }
}

impl From<network::RequestId> for fetch::RequestId {
    fn from(req: network::RequestId) -> Self {
        let s: String = req.into();
        s.into()
    }
}

impl From<network::InterceptionId> for fetch::RequestId {
    fn from(req: network::InterceptionId) -> Self {
        let s: String = req.into();
        s.into()
    }
}

impl From<network::InterceptionId> for network::RequestId {
    fn from(req: network::InterceptionId) -> Self {
        let s: String = req.into();
        s.into()
    }
}

impl From<fetch::RequestId> for network::InterceptionId {
    fn from(req: fetch::RequestId) -> Self {
        let s: String = req.into();
        s.into()
    }
}

impl From<network::RequestId> for network::InterceptionId {
    fn from(req: network::RequestId) -> Self {
        let s: String = req.into();
        s.into()
    }
}

impl DeleteCookiesParams {
    /// Create a new instance from a `CookieParam`
    pub fn from_cookie(param: &CookieParam) -> Self {
        DeleteCookiesParams {
            name: param.name.clone(),
            url: param.url.clone(),
            domain: param.domain.clone(),
            path: param.path.clone(),
            partition_key: param.partition_key.clone(),
        }
    }
}

impl From<EvaluateParams> for CallFunctionOnParams {
    fn from(params: EvaluateParams) -> CallFunctionOnParams {
        CallFunctionOnParams {
            function_declaration: params.expression,
            object_id: None,
            arguments: None,
            silent: params.silent,
            return_by_value: params.return_by_value,
            generate_preview: params.generate_preview,
            user_gesture: params.user_gesture,
            await_promise: params.await_promise,
            execution_context_id: params.context_id,
            object_group: params.object_group,
            throw_on_side_effect: None,
            unique_context_id: None,
            serialization_options: None,
        }
    }
}

impl fmt::Display for ExceptionDetails {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{}:{}: {}",
            self.line_number, self.column_number, self.text
        )?;

        if let Some(stack) = self.stack_trace.as_ref() {
            stack.fmt(f)?
        }
        Ok(())
    }
}

impl std::error::Error for ExceptionDetails {}

impl fmt::Display for StackTrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(desc) = self.description.as_ref() {
            writeln!(f, "{desc}")?;
        }
        for frame in &self.call_frames {
            writeln!(
                f,
                "{}@{}:{}:{}",
                frame.function_name, frame.url, frame.line_number, frame.column_number
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chromiumoxide_types::Message;

    use super::cdp::CdpEventMessage;

    // This makes sure we can parse arbitrary numbers (timestamp) as f64
    // and that the Message untagged union works core
    #[test]
    fn test_event_deserialize_f64() {
        let raw = r#"{"method":"Page.lifecycleEvent","params":{"frameId":"B0FCF18A982213C9947D313EAA8F934A","loaderId":"547DA6CC3D4A41314EA08A88BFA62B21","name":"commit","timestamp":1531.878478},"sessionId":"E0BCD37484373226136272710B8CB432"}"#;

        let event = serde_json::from_str::<Message<CdpEventMessage>>(raw).unwrap();

        assert!(matches!(event, Message::Event(_)));
    }
}
