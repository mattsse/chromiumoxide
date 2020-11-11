use serde::{Deserialize, Serialize};
#[doc = "Unique frame identifier.\n[FrameId](https://chromedevtools.github.io/devtools-protocol/tot/Page/#type-FrameId)"]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "camelCase")]
pub struct FrameId(String);
impl FrameId {
    pub const IDENTIFIER: &'static str = "Page.FrameId";
}
