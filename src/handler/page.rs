use crate::handler::target::TargetMessage;
use chromiumoxid_cdp::cdp::browser_protocol::target::{SessionId, TargetId};
use chromiumoxid_types::{Command, CommandResponse};
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::channel::oneshot::channel as oneshot_channel;
use futures::stream::Fuse;
use std::sync::Arc;

use crate::cmd::{to_command_response, CommandMessage};
use crate::error::{CdpError, Result};
use crate::keys;
use crate::layout::Point;
use chromiumoxid_cdp::cdp::browser_protocol::dom::{
    NodeId, QuerySelectorAllParams, QuerySelectorParams,
};
use chromiumoxid_cdp::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
    MouseButton,
};
use chromiumoxid_cdp::cdp::js_protocol::runtime::{
    CallFunctionOnParams, CallFunctionOnReturns, RemoteObjectId,
};
use futures::{SinkExt, StreamExt};

#[derive(Debug)]
pub struct PageHandle {
    pub(crate) rx: Fuse<Receiver<TargetMessage>>,
    page: Arc<PageInner>,
}

impl PageHandle {
    pub fn new(target_id: TargetId, session_id: SessionId) -> Self {
        let (commands, rx) = channel(1);
        let page = PageInner {
            target_id,
            session_id,
            sender: commands,
        };
        Self {
            rx: rx.fuse(),
            page: Arc::new(page),
        }
    }

    pub(crate) fn inner(&self) -> &Arc<PageInner> {
        &self.page
    }
}

#[derive(Debug)]
pub(crate) struct PageInner {
    target_id: TargetId,
    session_id: SessionId,
    sender: Sender<TargetMessage>,
}

impl PageInner {
    pub(crate) async fn execute<T: Command>(&self, cmd: T) -> Result<CommandResponse<T::Response>> {
        Ok(execute(cmd, self.sender.clone(), Some(self.session_id.clone())).await?)
    }

    /// The identifier of this page's target
    pub fn target_id(&self) -> &TargetId {
        &self.target_id
    }

    /// The identifier of this page's target's session
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub(crate) fn sender(&self) -> &Sender<TargetMessage> {
        &self.sender
    }

    /// Returns the first element in the node which matches the given CSS
    /// selector.
    pub async fn find_element(&self, selector: impl Into<String>, node: NodeId) -> Result<NodeId> {
        Ok(self
            .execute(QuerySelectorParams::new(node, selector))
            .await?
            .node_id)
    }

    /// Return all `Element`s inside the node that match the given selector
    pub(crate) async fn find_elements(
        &self,
        selector: impl Into<String>,
        node: NodeId,
    ) -> Result<Vec<NodeId>> {
        Ok(self
            .execute(QuerySelectorAllParams::new(node, selector))
            .await?
            .result
            .node_ids)
    }

    /// Moves the mouse to this point (dispatches a mouseMoved event)
    pub async fn move_mouse_to_point(&self, point: Point) -> Result<&Self> {
        let cmd = DispatchMouseEventParams::builder()
            .r#type(DispatchMouseEventType::MouseMoved)
            .x(point.x)
            .y(point.y)
            .build()
            .unwrap();
        self.execute(cmd).await?;
        Ok(self)
    }

    pub async fn click_point(&self, point: Point) -> Result<&Self> {
        let cmd = DispatchMouseEventParams::builder()
            .x(point.x)
            .y(point.y)
            .button(MouseButton::Left)
            .click_count(1);

        self.execute(
            cmd.clone()
                .r#type(DispatchMouseEventType::MousePressed)
                .build()
                .unwrap(),
        )
        .await?;

        self.execute(
            cmd.r#type(DispatchMouseEventType::MouseReleased)
                .build()
                .unwrap(),
        )
        .await?;
        Ok(self)
    }

    pub async fn type_str(&self, input: impl AsRef<str>) -> Result<&Self> {
        for c in input.as_ref().split("") {
            // split call above will have empty string at start and end which we won't type
            if c.is_empty() {
                continue;
            }
            self.press_key(c).await?;
        }
        Ok(self)
    }

    pub async fn press_key(&self, key: impl AsRef<str>) -> Result<&Self> {
        let definition = keys::get_key_definition(key.as_ref())
            .ok_or_else(|| CdpError::msg(format!("Key not found: {}", key.as_ref())))?;

        let mut cmd = DispatchKeyEventParams::builder();

        // See https://github.com/GoogleChrome/puppeteer/blob/62da2366c65b335751896afbb0206f23c61436f1/lib/Input.js#L114-L115
        // And https://github.com/GoogleChrome/puppeteer/blob/62da2366c65b335751896afbb0206f23c61436f1/lib/Input.js#L52
        let key_down_event_type = if let Some(txt) = definition.text {
            cmd = cmd.text(txt);
            DispatchKeyEventType::KeyDown
        } else if definition.key.len() == 1 {
            cmd = cmd.text(definition.key);
            DispatchKeyEventType::KeyDown
        } else {
            DispatchKeyEventType::RawKeyDown
        };

        cmd = cmd
            .r#type(DispatchKeyEventType::KeyDown)
            .key(definition.key)
            .code(definition.code)
            .windows_virtual_key_code(definition.key_code)
            .native_virtual_key_code(definition.key_code);

        self.execute(cmd.clone().r#type(key_down_event_type).build().unwrap())
            .await?;
        self.execute(cmd.r#type(DispatchKeyEventType::KeyUp).build().unwrap())
            .await?;
        Ok(self)
    }

    /// Calls function with given declaration on the remote object with the
    /// matching id
    pub async fn call_js_fn(
        &self,
        function_declaration: impl Into<String>,
        await_promise: bool,
        remote_object_id: RemoteObjectId,
    ) -> Result<CallFunctionOnReturns> {
        let resp = self
            .execute(
                CallFunctionOnParams::builder()
                    .object_id(remote_object_id)
                    .function_declaration(function_declaration)
                    .generate_preview(true)
                    .await_promise(await_promise)
                    .build()
                    .unwrap(),
            )
            .await?;
        Ok(resp.result)
    }
}

pub(crate) async fn execute<T: Command>(
    cmd: T,
    mut sender: Sender<TargetMessage>,
    session: Option<SessionId>,
) -> Result<CommandResponse<T::Response>> {
    let (tx, rx) = oneshot_channel();
    let method = cmd.identifier();
    let msg = CommandMessage::with_session(cmd, tx, session)?;

    sender.send(TargetMessage::Command(msg)).await?;
    let resp = rx.await??;
    to_command_response::<T>(resp, method)
}
