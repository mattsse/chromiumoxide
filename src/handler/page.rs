use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::channel::mpsc::{Receiver, Sender, channel};
use futures::channel::oneshot::channel as oneshot_channel;
use futures::stream::Fuse;
use futures::{SinkExt, StreamExt};

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
    CallFunctionOnParams, CallFunctionOnReturns, EvaluateParams, ExecutionContextId, RemoteObjectId,
};
use chromiumoxide_types::{Command, CommandResponse};

use crate::cmd::{CommandMessage, to_command_response};
use crate::error::{CdpError, Result};
use crate::handler::commandfuture::CommandFuture;
use crate::handler::domworld::DOMWorldKind;
use crate::handler::httpfuture::HttpFuture;
use crate::handler::movement::{MovementBehavior, movement_path};
use crate::handler::target::{GetExecutionContext, TargetMessage};
use crate::handler::target_message_future::TargetMessageFuture;
use crate::js::EvaluationResult;
use crate::layout::Point;
use crate::page::ScreenshotParams;
use crate::{ArcHttpRequest, keys, utils};
#[cfg(feature = "human_movements")]
use rand::RngExt;

/// Options that control how a click action is performed.
#[derive(Clone, Debug)]
pub struct ClickOptions {
    /// Number of times the click action should be executed.
    ///
    /// A value of `1` represents a single click.
    pub click_count: i64,
    /// Optional movement behavior for cursor path.
    ///
    /// `None` disables behavior-based path generation.
    pub movement_behavior: Option<MovementBehavior>,
}

impl Default for ClickOptions {
    fn default() -> Self {
        Self {
            click_count: 1,
            movement_behavior: None,
        }
    }
}

impl ClickOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn builder() -> ClickOptionsBuilder {
        ClickOptionsBuilder::default()
    }
}

#[derive(Clone, Debug)]
pub struct ClickOptionsBuilder {
    click_count: i64,
    movement_behavior: Option<MovementBehavior>,
}

impl Default for ClickOptionsBuilder {
    fn default() -> Self {
        Self {
            click_count: 1,
            movement_behavior: None,
        }
    }
}

impl ClickOptionsBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn click_count(mut self, count: impl Into<i64>) -> Self {
        self.click_count = count.into();
        self
    }

    pub fn movement_behavior(mut self, behavior: Option<impl Into<MovementBehavior>>) -> Self {
        self.movement_behavior = behavior.map(Into::into);
        self
    }

    pub fn build(self) -> ClickOptions {
        ClickOptions {
            click_count: self.click_count,
            movement_behavior: self.movement_behavior,
        }
    }
}

#[derive(Debug)]
pub struct PageHandle {
    pub(crate) rx: Fuse<Receiver<TargetMessage>>,
    page: Arc<PageInner>,
}

impl PageHandle {
    pub fn new(target_id: TargetId, session_id: SessionId, opener_id: Option<TargetId>) -> Self {
        let (commands, rx) = channel(1);
        let page = PageInner {
            target_id,
            session_id,
            opener_id,
            sender: commands,
            mouse_position: Mutex::new(Point::new(0.0, 0.0)),
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
    opener_id: Option<TargetId>,
    sender: Sender<TargetMessage>,
    mouse_position: Mutex<Point>,
}

impl PageInner {
    /// Execute a PDL command and return its response
    pub(crate) async fn execute<T: Command>(&self, cmd: T) -> Result<CommandResponse<T::Response>> {
        execute(cmd, self.sender.clone(), Some(self.session_id.clone())).await
    }

    /// Create a PDL command future
    pub(crate) fn command_future<T: Command>(&self, cmd: T) -> Result<CommandFuture<T>> {
        CommandFuture::new(cmd, self.sender.clone(), Some(self.session_id.clone()))
    }

    /// This creates navigation future with the final http response when the page is loaded
    pub(crate) fn wait_for_navigation(&self) -> TargetMessageFuture<ArcHttpRequest> {
        TargetMessageFuture::<ArcHttpRequest>::wait_for_navigation(self.sender.clone())
    }

    /// This creates HTTP future with navigation and responds with the final
    /// http response when the page is loaded
    pub(crate) fn http_future<T: Command>(&self, cmd: T) -> Result<HttpFuture<T>> {
        Ok(HttpFuture::new(
            self.sender.clone(),
            self.command_future(cmd)?,
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

    /// Returns the first element in the node which matches the given CSS
    /// selector.
    pub async fn find_element(&self, selector: impl Into<String>, node: NodeId) -> Result<NodeId> {
        Ok(self
            .execute(QuerySelectorParams::new(node, selector))
            .await?
            .node_id)
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
        self.move_mouse_with_behavior(point, None).await?;
        Ok(self)
    }

    /// Returns the current mouse position tracked by this page.
    pub fn mouse_pos(&self) -> Point {
        *self.mouse_position.lock().unwrap()
    }

    /// Performs a mouse click event at the point's location
    pub async fn click(&self, point: Point) -> Result<&Self> {
        let default_opts = ClickOptions::default();
        self.click_with(point, default_opts).await
    }

    /// Performs a mouse click event at the point's location with custom options
    pub async fn click_with(&self, point: Point, options: ClickOptions) -> Result<&Self> {
        let movement_behavior = options.movement_behavior.as_ref();

        let point = {
            #[cfg(not(feature = "human_movements"))]
            {
                point
            }

            #[cfg(feature = "human_movements")]
            {
                //TODO: Make it optionable
                // Target Selection Jitter: don't land exactly on the pixel
                let mut rng = rand::rng();
                let jitter_x = rng.random_range(-2.0..2.0);
                let jitter_y = rng.random_range(-2.0..2.0);

                Point {
                    x: point.x + jitter_x,
                    y: point.y + jitter_y,
                }
            }
        };
        let cmd = DispatchMouseEventParams::builder()
            .x(point.x)
            .y(point.y)
            .button(MouseButton::Left)
            .click_count(options.click_count);

        self.move_mouse_with_behavior(point, movement_behavior)
            .await?;

        #[cfg(feature = "human_movements")]
        {
            // Small pause before clicking (humans don't click instantly after arriving)
            if movement_behavior.is_some() {
                tokio::time::sleep(Duration::from_millis(rand::random_range(50..=150))).await;
            }
        }
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
        #[cfg(feature = "human_movements")]
        {
            // Small pause after clicking
            if movement_behavior.is_some() {
                tokio::time::sleep(Duration::from_millis(rand::random_range(35..=110))).await;
            }
        }
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
        #[cfg(feature = "human_movements")]
        {
            // Small delay to simulate human typing (humans doesnt directly type +300WPM)
            tokio::time::sleep(Duration::from_millis(rand::random_range(12..=60))).await;
        }
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
        #[cfg(feature = "human_movements")]
        {
            // Small delay to simulate human typing
            tokio::time::sleep(Duration::from_millis(rand::random_range(24..=65))).await;
        }
        Ok(self)
    }

    async fn move_mouse_with_behavior(
        &self,
        point: Point,
        behavior: Option<&MovementBehavior>,
    ) -> Result<()> {
        match behavior {
            Some(behavior) => {
                let start = { *self.mouse_position.lock().unwrap() };
                let path = movement_path(start, point, behavior);
                for (idx, path_point) in path.iter().enumerate() {
                    let (x, y) = (path_point.x, path_point.y);

                    self.execute(DispatchMouseEventParams::new(
                        DispatchMouseEventType::MouseMoved,
                        x,
                        y,
                    ))
                    .await?;
                    *self.mouse_position.lock().unwrap() = point;

                    // Tiny delay to simulate physical movement
                    if idx + 1 != path.len() {
                        #[cfg(feature = "human_movements")]
                        tokio::time::sleep(Duration::from_millis(rand::random_range(5..15))).await;
                        #[cfg(not(feature = "human_movements"))]
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                }
            }
            None => {
                self.execute(DispatchMouseEventParams::new(
                    DispatchMouseEventType::MouseMoved,
                    point.x,
                    point.y,
                ))
                .await?;
                *self.mouse_position.lock().unwrap() = point;
            }
        }

        Ok(())
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
