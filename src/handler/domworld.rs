use chromiumoxide_cdp::cdp::js_protocol::runtime::ExecutionContextId;

#[derive(Debug, Clone, Default)]
pub struct DOMWorld {
    execution_ctx: Option<ExecutionContextId>,
    detached: bool,
}

impl DOMWorld {
    pub fn main_world() -> Self {
        Self {
            execution_ctx: None,
            detached: false,
        }
    }

    pub fn secondary_world() -> Self {
        Self {
            execution_ctx: None,
            detached: false,
        }
    }

    pub fn execution_context(&self) -> Option<ExecutionContextId> {
        self.execution_ctx
    }

    pub fn set_context(&mut self, ctx: ExecutionContextId) {
        self.execution_ctx = Some(ctx);
    }

    pub fn take_context(&mut self) -> Option<ExecutionContextId> {
        self.execution_ctx.take()
    }

    pub fn is_detached(&self) -> bool {
        self.detached
    }
}

/// There are two different kinds of worlds tracked for each `Frame`, that
/// represent a context for JavaScript execution. A `Page` might have many
/// execution contexts
/// - each [iframe](https://developer.mozilla.org/en-US/docs/Web/HTML/Element/iframe)
///   has a "default" execution context that is always created after the frame
///   is attached to DOM.
/// [Extension's](https://developer.chrome.com/extensions) content scripts create additional execution contexts.
///
/// Besides pages, execution contexts can be found in
/// [Web Workers](https://developer.mozilla.org/en-US/docs/Web/API/Web_Workers_API).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DOMWorldKind {
    /// The main world of a frame that represents the default execution context
    /// of a frame and is also created.
    #[default]
    Main,
    /// Each frame gets its own isolated world with universal access
    Secondary,
}
