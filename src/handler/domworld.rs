use chromiumoxide_cdp::cdp::js_protocol::runtime::ExecutionContextId;
use std::collections::HashSet;

#[derive(Debug, Clone, Default)]
pub struct DOMWorld {
    /// Bindings that have been registered in the current context
    ctx_bindings: HashSet<String>,
    execution_ctx: Option<ExecutionContextId>,
    detached: bool,
}

impl DOMWorld {
    pub fn main_world() -> Self {
        Self {
            ctx_bindings: Default::default(),
            execution_ctx: None,
            detached: false,
        }
    }

    pub fn secondary_world() -> Self {
        Self {
            ctx_bindings: Default::default(),
            execution_ctx: None,
            detached: false,
        }
    }

    pub fn context(&self) -> Option<ExecutionContextId> {
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

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum DOMWorldKind {
    Main,
    Secondary,
}
