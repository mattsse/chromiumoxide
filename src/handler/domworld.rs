use std::collections::HashSet;
use chromiumoxide_cdp::cdp::js_protocol::runtime::EventBindingCalled;

#[derive(Debug, Clone)]
pub struct DOMWorld {
    /// Bindings that have been registered in the current context
    ctx_bindings: HashSet<String>,
    detached: bool,
}

impl DOMWorld {

    pub fn on_runtime_binding_called(&mut self, ev: &EventBindingCalled) {

    }
}