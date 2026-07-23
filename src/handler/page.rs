use std::sync::Arc;

use futures::channel::mpsc::{Receiver, Sender, channel};
use futures::channel::oneshot::channel as oneshot_channel;
use futures::future::BoxFuture;
use futures::stream::Fuse;
use futures::{FutureExt, SinkExt, StreamExt};

use chromiumoxide_cdp::cdp::browser_protocol::browser::{GetVersionParams, GetVersionReturns};
use chromiumoxide_cdp::cdp::browser_protocol::dom::{
    DiscardSearchResultsParams, GetSearchResultsParams, NodeId, PerformSearchParams,
    QuerySelectorAllParams, QuerySelectorParams, Rgba,
};
use chromiumoxide_cdp::cdp::browser_protocol::emulation::{
    ClearDeviceMetricsOverrideParams, SetDefaultBackgroundColorOverrideParams,
    SetDeviceMetricsOverrideParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
    MouseButton,
};
use chromiumoxide_cdp::cdp::browser_protocol::page::{
    FrameId, GetLayoutMetricsParams, GetLayoutMetricsReturns, Viewport,
};
use chromiumoxide_cdp::cdp::browser_protocol::target::{ActivateTargetParams, SessionId, TargetId};
use chromiumoxide_cdp::cdp::js_protocol::runtime::{
    CallFunctionOnParams, EvaluateParams, ExecutionContextId,
};
use chromiumoxide_types::{Command, CommandResponse};

use crate::cmd::{CommandMessage, to_command_response};
use crate::error::{CdpError, Result};
use crate::frame::Frame;
use crate::handler::commandfuture::CommandFuture;
use crate::handler::domworld::DOMWorldKind;
use crate::handler::httpfuture::HttpFuture;
use crate::handler::target::{
    FrameBoundary, FrameInfo, GetExecutionContext, InternalTargetMessage, TargetMessage,
};
use crate::js::EvaluationResult;
use crate::layout::Point;
use crate::page::ScreenshotParams;
use crate::{ArcHttpRequest, keys, utils};

#[derive(Debug)]
pub struct PageHandle {
    pub(crate) rx: Fuse<Receiver<TargetMessage>>,
    pub(crate) internal_rx: Fuse<Receiver<InternalTargetMessage>>,
    page: Arc<PageInner>,
}

impl PageHandle {
    pub fn new(target_id: TargetId, session_id: SessionId, opener_id: Option<TargetId>) -> Self {
        let (commands, rx) = channel(1);
        let (internal_commands, internal_rx) = channel(1);
        let page = PageInner {
            target_id,
            session_id,
            opener_id,
            sender: commands,
            internal_sender: internal_commands,
        };
        Self {
            rx: rx.fuse(),
            internal_rx: internal_rx.fuse(),
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
    opener_id: Option<TargetId>,
    sender: Sender<TargetMessage>,
    internal_sender: Sender<InternalTargetMessage>,
}

impl PageInner {
    /// Execute a PDL command and return its response
    pub(crate) async fn execute<T: Command>(&self, cmd: T) -> Result<CommandResponse<T::Response>> {
        execute(cmd, self.sender.clone(), Some(self.session_id.clone())).await
    }

    /// Execute a command on an explicitly captured CDP session.
    ///
    /// Element handles use this path so their DOM and Runtime object ids never
    /// silently fall back to the page's main session after an OOP frame swap.
    pub(crate) async fn execute_with_session<T: Command>(
        &self,
        cmd: T,
        session_id: &SessionId,
    ) -> Result<CommandResponse<T::Response>> {
        let (tx, rx) = oneshot_channel();
        let method = cmd.identifier();
        let command = CommandMessage::with_session(cmd, tx, Some(session_id.clone()))?;
        self.internal_sender
            .clone()
            .send(InternalTargetMessage::SessionCommand {
                session_id: session_id.clone(),
                command,
            })
            .await?;
        let response = rx.await??;
        to_command_response::<T>(response, method)
    }

    /// Create a PDL command future
    pub(crate) fn command_future<T: Command>(&self, cmd: T) -> Result<CommandFuture<T>> {
        CommandFuture::new(cmd, self.sender.clone(), Some(self.session_id.clone()))
    }

    /// This creates navigation future with the final http response when the page is loaded
    pub(crate) fn wait_for_navigation(&self) -> BoxFuture<'static, Result<ArcHttpRequest>> {
        let mut sender = self.internal_sender.clone();
        async move {
            let (tx, rx) = oneshot_channel();
            sender
                .send(InternalTargetMessage::WaitForNavigationResult { tx })
                .await?;
            rx.await?
        }
        .boxed()
    }

    /// This creates HTTP future with navigation and responds with the final
    /// http response when the page is loaded
    pub(crate) fn http_future<T: Command>(&self, cmd: T) -> Result<HttpFuture<T>> {
        Ok(HttpFuture::with_navigation(
            self.command_future(cmd)?,
            self.wait_for_navigation(),
        ))
    }

    /// The identifier of this page's target
    pub fn target_id(&self) -> &TargetId {
        &self.target_id
    }

    /// The identifier of this page's target's session
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// The identifier of this page's target's opener target
    pub fn opener_id(&self) -> &Option<TargetId> {
        &self.opener_id
    }

    pub(crate) fn sender(&self) -> &Sender<TargetMessage> {
        &self.sender
    }

    /// Sender for operations whose correctness depends on a captured frame or
    /// child-session identity. It is intentionally separate from the legacy
    /// public message channel.
    pub(crate) fn internal_sender(&self) -> &Sender<InternalTargetMessage> {
        &self.internal_sender
    }

    pub(crate) async fn frame_info(&self, frame_id: FrameId) -> Result<Option<FrameInfo>> {
        let (tx, rx) = oneshot_channel();
        self.internal_sender
            .clone()
            .send(InternalTargetMessage::GetFrameInfo { frame_id, tx })
            .await?;
        rx.await?
    }

    /// Snapshot the cross-session edges between a frame and the page root.
    pub(crate) async fn frame_boundary_chain(
        &self,
        frame_id: FrameId,
        expected_session_id: SessionId,
    ) -> Result<Vec<FrameBoundary>> {
        let (tx, rx) = oneshot_channel();
        self.internal_sender
            .clone()
            .send(InternalTargetMessage::GetFrameBoundaryChain {
                frame_id,
                expected_session_id,
                tx,
            })
            .await?;
        rx.await?
    }

    pub(crate) fn frame_from_info(self: &Arc<Self>, info: FrameInfo) -> Frame {
        Frame::from_info(Arc::clone(self), info)
    }

    pub(crate) async fn frame_by_id(self: &Arc<Self>, frame_id: FrameId) -> Result<Option<Frame>> {
        Ok(self
            .frame_info(frame_id)
            .await?
            .map(|info| self.frame_from_info(info)))
    }

    pub(crate) async fn all_frames(self: &Arc<Self>) -> Result<Vec<Frame>> {
        let (tx, rx) = oneshot_channel();
        self.internal_sender
            .clone()
            .send(InternalTargetMessage::GetAllFrames { tx })
            .await?;
        Ok(rx
            .await?
            .into_iter()
            .map(|info| self.frame_from_info(info))
            .collect())
    }

    pub(crate) async fn main_frame(self: &Arc<Self>) -> Result<Option<Frame>> {
        let (tx, rx) = oneshot_channel();
        self.sender
            .clone()
            .send(TargetMessage::MainFrame(tx))
            .await?;
        let Some(frame_id) = rx.await? else {
            return Ok(None);
        };
        self.frame_by_id(frame_id).await
    }

    /// Returns the first element in the node which matches the given CSS
    /// selector.
    pub async fn find_element(&self, selector: impl Into<String>, node: NodeId) -> Result<NodeId> {
        let node_id = self
            .execute(QuerySelectorParams::new(node, selector))
            .await?
            .node_id;
        if *node_id.inner() == 0 {
            return Err(CdpError::NotFound);
        }
        Ok(node_id)
    }

    /// Activates (focuses) the target.
    pub async fn activate(&self) -> Result<&Self> {
        self.execute(ActivateTargetParams::new(self.target_id().clone()))
            .await?;
        Ok(self)
    }

    /// Version information about the browser
    pub async fn version(&self) -> Result<GetVersionReturns> {
        Ok(self.execute(GetVersionParams::default()).await?.result)
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

    /// Returns all elements which matches the given xpath selector
    pub async fn find_xpaths(&self, query: impl Into<String>) -> Result<Vec<NodeId>> {
        let perform_search_returns = self
            .execute(PerformSearchParams {
                query: query.into(),
                include_user_agent_shadow_dom: Some(true),
            })
            .await?
            .result;

        let search_results = self
            .execute(GetSearchResultsParams::new(
                perform_search_returns.search_id.clone(),
                0,
                perform_search_returns.result_count,
            ))
            .await?
            .result;

        self.execute(DiscardSearchResultsParams::new(
            perform_search_returns.search_id,
        ))
        .await?;

        Ok(search_results.node_ids)
    }

    /// Moves the mouse to this point (dispatches a mouseMoved event)
    pub async fn move_mouse(&self, point: Point) -> Result<&Self> {
        self.execute(DispatchMouseEventParams::new(
            DispatchMouseEventType::MouseMoved,
            point.x,
            point.y,
        ))
        .await?;
        Ok(self)
    }

    /// Performs a mouse click event at the point's location
    pub async fn click(&self, point: Point) -> Result<&Self> {
        let default_opts = chromiumoxide_types::ClickOptions::default();
        self.click_with(point, default_opts).await
    }

    /// Performs a mouse click event at the point's location with custom options
    pub async fn click_with(
        &self,
        point: Point,
        options: chromiumoxide_types::ClickOptions,
    ) -> Result<&Self> {
        let cmd = DispatchMouseEventParams::builder()
            .x(point.x)
            .y(point.y)
            .button(MouseButton::Left)
            .click_count(options.click_count);

        self.move_mouse(point)
            .await?
            .execute(
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

    /// This simulates pressing keys on the page.
    ///
    /// # Note The `input` is treated as series of `KeyDefinition`s, where each
    /// char is inserted as a separate keystroke. So sending
    /// `page.type_str("Enter")` will be processed as a series of single
    /// keystrokes:  `["E", "n", "t", "e", "r"]`. To simulate pressing the
    /// actual Enter key instead use `page.press_key(
    /// keys::get_key_definition("Enter").unwrap())`.
    pub async fn type_str(&self, input: impl AsRef<str>) -> Result<&Self> {
        for c in input.as_ref().split("").filter(|s| !s.is_empty()) {
            self.press_key(c).await?;
        }
        Ok(self)
    }

    /// Uses the `DispatchKeyEvent` mechanism to simulate pressing keyboard
    /// keys.
    pub async fn press_key(&self, key: impl AsRef<str>) -> Result<&Self> {
        let key = key.as_ref();
        let key_definition = keys::get_key_definition(key)
            .ok_or_else(|| CdpError::msg(format!("Key not found: {key}")))?;
        let mut cmd = DispatchKeyEventParams::builder();

        // See https://github.com/GoogleChrome/puppeteer/blob/62da2366c65b335751896afbb0206f23c61436f1/lib/Input.js#L114-L115
        // And https://github.com/GoogleChrome/puppeteer/blob/62da2366c65b335751896afbb0206f23c61436f1/lib/Input.js#L52
        let key_down_event_type = if let Some(txt) = key_definition.text {
            cmd = cmd.text(txt);
            DispatchKeyEventType::KeyDown
        } else if key_definition.key.len() == 1 {
            cmd = cmd.text(key_definition.key);
            DispatchKeyEventType::KeyDown
        } else {
            DispatchKeyEventType::RawKeyDown
        };

        cmd = cmd
            .r#type(DispatchKeyEventType::KeyDown)
            .key(key_definition.key)
            .code(key_definition.code)
            .windows_virtual_key_code(key_definition.key_code)
            .native_virtual_key_code(key_definition.key_code);

        self.execute(cmd.clone().r#type(key_down_event_type).build().unwrap())
            .await?;
        self.execute(cmd.r#type(DispatchKeyEventType::KeyUp).build().unwrap())
            .await?;
        Ok(self)
    }

    pub async fn evaluate_expression(
        &self,
        evaluate: impl Into<EvaluateParams>,
    ) -> Result<EvaluationResult> {
        let mut evaluate = evaluate.into();
        if evaluate.context_id.is_none() {
            evaluate.context_id = self.execution_context().await?;
        }
        if evaluate.await_promise.is_none() {
            evaluate.await_promise = Some(true);
        }
        if evaluate.return_by_value.is_none() {
            evaluate.return_by_value = Some(true);
        }

        let resp = self.execute(evaluate).await?.result;
        if let Some(exception) = resp.exception_details {
            return Err(CdpError::JavascriptException(Box::new(exception)));
        }

        Ok(EvaluationResult::new(resp.result))
    }

    pub async fn evaluate_function(
        &self,
        evaluate: impl Into<CallFunctionOnParams>,
    ) -> Result<EvaluationResult> {
        let mut evaluate = evaluate.into();
        if evaluate.execution_context_id.is_none() {
            evaluate.execution_context_id = self.execution_context().await?;
        }
        if evaluate.await_promise.is_none() {
            evaluate.await_promise = Some(true);
        }
        if evaluate.return_by_value.is_none() {
            evaluate.return_by_value = Some(true);
        }

        let resp = self.execute(evaluate).await?.result;
        if let Some(exception) = resp.exception_details {
            return Err(CdpError::JavascriptException(Box::new(exception)));
        }
        Ok(EvaluationResult::new(resp.result))
    }

    pub async fn execution_context(&self) -> Result<Option<ExecutionContextId>> {
        self.execution_context_for_world(None, DOMWorldKind::Main)
            .await
    }

    pub async fn secondary_execution_context(&self) -> Result<Option<ExecutionContextId>> {
        self.execution_context_for_world(None, DOMWorldKind::Secondary)
            .await
    }

    pub async fn frame_execution_context(
        &self,
        frame_id: FrameId,
    ) -> Result<Option<ExecutionContextId>> {
        self.execution_context_for_world(Some(frame_id), DOMWorldKind::Main)
            .await
    }

    pub async fn frame_secondary_execution_context(
        &self,
        frame_id: FrameId,
    ) -> Result<Option<ExecutionContextId>> {
        self.execution_context_for_world(Some(frame_id), DOMWorldKind::Secondary)
            .await
    }

    pub async fn execution_context_for_world(
        &self,
        frame_id: Option<FrameId>,
        dom_world: DOMWorldKind,
    ) -> Result<Option<ExecutionContextId>> {
        let (tx, rx) = oneshot_channel();
        self.sender
            .clone()
            .send(TargetMessage::GetExecutionContext(GetExecutionContext {
                dom_world,
                frame_id,
                tx,
            }))
            .await?;
        Ok(rx.await?)
    }

    /// Returns metrics relating to the layout of the page
    pub async fn layout_metrics(&self) -> Result<GetLayoutMetricsReturns> {
        Ok(self
            .execute(GetLayoutMetricsParams::default())
            .await?
            .result)
    }

    pub async fn screenshot(&self, params: impl Into<ScreenshotParams>) -> Result<Vec<u8>> {
        self.activate().await?;
        let params = params.into();
        let full_page = params.full_page();
        let omit_background = params.omit_background();

        let mut cdp_params = params.cdp_params;

        if full_page {
            let metrics = self.layout_metrics().await?;
            let width = metrics.css_content_size.width;
            let height = metrics.css_content_size.height;

            cdp_params.clip = Some(Viewport {
                x: 0.,
                y: 0.,
                width,
                height,
                scale: 1.,
            });

            self.execute(SetDeviceMetricsOverrideParams::new(
                width as i64,
                height as i64,
                1.,
                false,
            ))
            .await?;
        }

        if omit_background {
            self.execute(SetDefaultBackgroundColorOverrideParams {
                color: Some(Rgba {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: Some(0.),
                }),
            })
            .await?;
        }

        let res = self.execute(cdp_params).await?.result;

        if omit_background {
            self.execute(SetDefaultBackgroundColorOverrideParams { color: None })
                .await?;
        }

        if full_page {
            self.execute(ClearDeviceMetricsOverrideParams {}).await?;
        }

        Ok(utils::base64::decode(&res.data)?)
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
