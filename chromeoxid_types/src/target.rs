use crate::browser::BrowserContextId;
use crate::{Command, Method, SessionId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TargetId(String);
impl AsRef<str> for TargetId {
    fn as_ref(&self) -> &str {
        self.0.as_str()
    }
}

#[doc = "Creates a new page.\n[createTarget](https://chromedevtools.github.io/devtools-protocol/tot/Target/#method-createTarget)"]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTargetParams {
    #[doc = "The initial URL the page will be navigated to."]
    pub url: String,
    #[doc = "Frame width in DIP (headless chrome only)."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<i64>,
    #[doc = "Frame height in DIP (headless chrome only)."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<i64>,
    #[doc = "The browser context to create the page in."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_context_id: Option<BrowserContextId>,
    #[doc = "Whether BeginFrames for this target will be controlled via DevTools (headless chrome only,\nnot supported on MacOS yet, false by default)."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_begin_frame_control: Option<bool>,
    #[doc = "Whether to create a new Window or Tab (chrome-only, false by default)."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_window: Option<bool>,
    #[doc = "Whether to create the target in background or foreground (chrome-only,\nfalse by default)."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
}
impl CreateTargetParams {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            width: None,
            height: None,
            browser_context_id: None,
            enable_begin_frame_control: None,
            new_window: None,
            background: None,
        }
    }

    pub fn blank() -> Self {
        Self::new("about:blank")
    }
}

impl<T: Into<String>> From<T> for CreateTargetParams {
    fn from(s: T) -> Self {
        let url = s.into();
        CreateTargetParams::new(url)
    }
}

impl CreateTargetParams {
    pub const IDENTIFIER: &'static str = "Target.createTarget";
}

impl Method for CreateTargetParams {
    fn identifier(&self) -> ::std::borrow::Cow<'static, str> {
        Self::IDENTIFIER.into()
    }
}

#[doc = "Creates a new page.\n[createTarget](https://chromedevtools.github.io/devtools-protocol/tot/Target/#method-createTarget)"]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTargetReturns {
    #[doc = "The id of the page opened."]
    pub target_id: TargetId,
}

impl CreateTargetReturns {
    pub fn new(target_id: TargetId) -> CreateTargetReturns {
        Self { target_id }
    }
}

impl Command for CreateTargetParams {
    type Response = CreateTargetReturns;
}

#[doc = "Attaches to the target with given id.\n[attachToTarget](https://chromedevtools.github.io/devtools-protocol/tot/Target/#method-attachToTarget)"]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachToTargetParams {
    pub target_id: TargetId,
    #[doc = "Enables \"flat\" access to the session via specifying sessionId attribute in the commands.\nWe plan to make this the default, deprecate non-flattened mode,\nand eventually retire it. See crbug.com/991325."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flatten: Option<bool>,
}
impl AttachToTargetParams {
    pub fn new(target_id: TargetId) -> AttachToTargetParams {
        Self {
            target_id,
            flatten: Default::default(),
        }
    }
}
impl AttachToTargetParams {
    pub const IDENTIFIER: &'static str = "Target.attachToTarget";
}
impl Method for AttachToTargetParams {
    fn identifier(&self) -> ::std::borrow::Cow<'static, str> {
        Self::IDENTIFIER.into()
    }
}
#[doc = "Attaches to the target with given id.\n[attachToTarget](https://chromedevtools.github.io/devtools-protocol/tot/Target/#method-attachToTarget)"]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachToTargetReturns {
    #[doc = "Id assigned to the session."]
    pub session_id: SessionId,
}
impl AttachToTargetReturns {
    pub fn new(session_id: SessionId) -> AttachToTargetReturns {
        Self { session_id }
    }
}
impl Command for AttachToTargetParams {
    type Response = AttachToTargetReturns;
}
