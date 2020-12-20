use chromiumoxide_cdp::cdp::js_protocol::runtime::ExecutionContextId;

/// Represents a context for JavaScript execution. A `Page` might have many
/// execution contexts
/// - each [iframe](https://developer.mozilla.org/en-US/docs/Web/HTML/Element/iframe)
///   has a "default" execution context that is always created after the frame
///   is attached to DOM.
/// [Extension's](https://developer.chrome.com/extensions) content scripts create additional execution contexts.
///
/// Besides pages, execution contexts can be found in
/// [Web Workers](https://developer.mozilla.org/en-US/docs/Web/API/Web_Workers_API).
#[derive(Debug)]
pub struct ExecutionContext {
    /// Identifier of a execution context
    context_id: ExecutionContextId,
}

impl ExecutionContext {}
