use std::fmt;

use crate::cdp::browser_protocol::network::{CookieParam, DeleteCookiesParams};
use crate::cdp::browser_protocol::target::CreateTargetParams;
use crate::cdp::js_protocol::runtime::{ExceptionDetails, StackTrace};

// Include all the types
include!(concat!(env!("OUT_DIR"), "/cdp.rs"));

/// convenience fixups
impl Default for CreateTargetParams {
    fn default() -> Self {
        "about:blank".into()
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
            writeln!(f, "{}", desc)?;
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
