use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "camelCase")]
pub struct BrowserContextId(String);
impl BrowserContextId {
    pub const IDENTIFIER: &'static str = "Browser.BrowserContextID";
}
