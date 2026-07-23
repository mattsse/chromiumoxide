use std::sync::{Arc, RwLock};

use futures::SinkExt;
use futures::channel::oneshot::channel as oneshot_channel;

use chromiumoxide_cdp::cdp::browser_protocol::dom::DescribeNodeParams;
use chromiumoxide_cdp::cdp::browser_protocol::page::{FrameId, NavigateParams, NavigateReturns};
use chromiumoxide_cdp::cdp::browser_protocol::target::SessionId;
use chromiumoxide_cdp::cdp::js_protocol::runtime::{
    CallArgument, CallFunctionOnParams, EvaluateParams, ExecutionContextId, ReleaseObjectParams,
    RemoteObjectId, RemoteObjectSubtype, RemoteObjectType,
};
use chromiumoxide_types::{Command, CommandResponse, Method, Request};

use crate::cmd::{CommandMessage, to_command_response};
use crate::element::{Element, remote_object_ids_for_exception};
use crate::error::{CdpError, Result};
use crate::handler::PageInner;
use crate::handler::domworld::DOMWorldKind;
use crate::handler::frame::FrameWaitError;
use crate::handler::target::{ExecutionContextInfo, FrameInfo, InternalTargetMessage};
use crate::js::{Evaluation, EvaluationResult};

/// A frame handle pinned to the CDP session that owned the frame when the
/// handle was created.
///
/// Process swaps do not retarget existing handles. Operations on a handle
/// whose frame has moved to another session fail with [`CdpError::FrameNotReady`];
/// obtain a fresh handle from [`crate::Page::all_frames`] or
/// [`crate::Page::frame_by_id`] and retry.
#[derive(Debug, Clone)]
pub struct Frame {
    inner: Arc<FrameInner>,
}

#[derive(Debug)]
pub(crate) struct FrameInner {
    frame_id: FrameId,
    session_id: SessionId,
    main_session_id: SessionId,
    parent_id: Option<FrameId>,
    security_origin: String,
    tab: Arc<PageInner>,
    cached_url: RwLock<Option<String>>,
}

impl Frame {
    pub(crate) fn from_info(tab: Arc<PageInner>, info: FrameInfo) -> Self {
        Self {
            inner: Arc::new(FrameInner {
                frame_id: info.frame_id,
                session_id: info.session_id,
                main_session_id: info.main_session_id,
                parent_id: info.parent_id,
                security_origin: info.security_origin,
                tab,
                cached_url: RwLock::new(info.url),
            }),
        }
    }

    /// The stable CDP frame identifier.
    pub fn id(&self) -> &FrameId {
        &self.inner.frame_id
    }

    /// The CDP session captured when this handle was created.
    pub fn session_id(&self) -> &SessionId {
        &self.inner.session_id
    }

    /// The effective CDP session captured when this handle was created.
    pub fn effective_session_id(&self) -> &SessionId {
        self.session_id()
    }

    /// The parent frame identifier captured when this handle was created.
    pub fn parent_id(&self) -> Option<&FrameId> {
        self.inner.parent_id.as_ref()
    }

    /// Whether this handle represents the page's main frame.
    pub fn is_main_frame(&self) -> bool {
        self.inner.parent_id.is_none()
    }

    /// Whether this frame is owned by a child target session.
    pub fn is_out_of_process(&self) -> bool {
        self.inner.session_id != self.inner.main_session_id
    }

    /// The security origin captured when this handle was created.
    pub fn security_origin(&self) -> &str {
        &self.inner.security_origin
    }

    /// The URL captured when this handle was created or last refreshed.
    pub fn url(&self) -> Option<String> {
        self.inner
            .cached_url
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Re-query the current URL for this frame identifier.
    ///
    /// Unlike [`Frame::eval`], [`Frame::goto`], [`Frame::query_selector`], and
    /// [`Frame::wait_for_navigation`] — which are pinned to this handle's
    /// captured session and fail with [`CdpError::FrameNotReady`] once the frame
    /// has swapped away — this reads whichever session currently owns the frame
    /// id and follows it. On a stale handle it therefore returns the URL of the
    /// frame's new binding rather than an error, and refreshes the cached URL to
    /// match. It is a best-effort current-state read, not a stale-handle guard.
    pub async fn fetch_url(&self) -> Result<Option<String>> {
        let url = self
            .inner
            .tab
            .frame_info(self.inner.frame_id.clone())
            .await?
            .and_then(|info| info.url);
        *self
            .inner
            .cached_url
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = url.clone();
        Ok(url)
    }

    /// Execute a raw CDP command on the captured frame session.
    ///
    /// `Page.navigate` is intentionally rejected here: raw execution loses the
    /// frame identity needed to register a navigation watcher and would
    /// misroute to the page's main frame. Navigate with [`Frame::goto`], which
    /// pins both the frame and its session. Rejection is immediate and produces
    /// no queued event or CDP request.
    pub async fn execute<T: Command>(&self, cmd: T) -> Result<CommandResponse<T::Response>> {
        let (tx, rx) = oneshot_channel();
        let method = cmd.identifier();
        if method.as_ref() == NavigateParams::IDENTIFIER {
            return Err(CdpError::NotAllowed(
                "Page.navigate is not supported through Frame::execute; use Frame::goto instead"
                    .to_owned(),
            ));
        }
        let command = CommandMessage::with_session(cmd, tx, Some(self.inner.session_id.clone()))?;
        self.inner
            .tab
            .internal_sender()
            .clone()
            .send(InternalTargetMessage::FrameCommand {
                frame_id: self.inner.frame_id.clone(),
                expected_session_id: self.inner.session_id.clone(),
                command,
            })
            .await?;
        let response = rx.await??;
        to_command_response::<T>(response, method)
    }

    /// Evaluate JavaScript in this frame's main execution context.
    pub async fn eval(&self, evaluation: impl Into<Evaluation>) -> Result<EvaluationResult> {
        match evaluation.into() {
            Evaluation::Expression(expression) => self.evaluate_expression(expression).await,
            Evaluation::Function(function) => self.evaluate_function(function).await,
        }
    }

    /// Query the first element in this frame's pinned main execution context.
    ///
    /// The returned Element keeps the remote object in this Frame handle's
    /// captured session and has no document-scoped `NodeId`.
    pub async fn query_selector(&self, selector: &str) -> Result<Option<Element>> {
        let context = self.pinned_execution_context().await?;
        let params = CallFunctionOnParams::builder()
            .function_declaration("function(sel) { return document.querySelector(sel); }")
            .argument(CallArgument::builder().value(selector).build())
            .return_by_value(false)
            .execution_context_id(context.context_id)
            .build()
            .map_err(CdpError::msg)?;
        let response = self.execute(params).await?.result;

        if let Some(exception) = response.exception_details {
            let cleanup = remote_object_ids_for_exception(&response.result, &exception);
            self.release_remote_objects_best_effort(cleanup).await;
            return Err(CdpError::JavascriptException(Box::new(exception)));
        }
        if response.result.subtype == Some(RemoteObjectSubtype::Null) {
            if let Some(remote_object_id) = response.result.object_id {
                self.release_remote_objects_best_effort([remote_object_id])
                    .await;
            }
            return Ok(None);
        }

        let remote_object_id = response
            .result
            .object_id
            .ok_or_else(|| CdpError::msg("Selector result did not contain a remote object id"))?;
        let described = self
            .execute(
                DescribeNodeParams::builder()
                    .object_id(remote_object_id.clone())
                    .build(),
            )
            .await;
        let backend_node_id = match described {
            Ok(response) => response.result.node.backend_node_id,
            Err(error) => {
                self.release_remote_objects_best_effort([remote_object_id])
                    .await;
                return Err(error);
            }
        };

        Ok(Some(Element::from_remote_parts(
            Arc::clone(&self.inner.tab),
            remote_object_id,
            backend_node_id,
            self.inner.frame_id.clone(),
            self.inner.session_id.clone(),
        )))
    }

    /// Navigate this frame and wait for its load lifecycle to complete.
    pub async fn goto(&self, url: impl Into<String>) -> Result<NavigateReturns> {
        let params = NavigateParams::new(url);
        let method = params.identifier();
        let request = Request::with_session(
            method.clone(),
            serde_json::to_value(params)?,
            self.inner.session_id.as_ref(),
        );
        let (tx, rx) = oneshot_channel();
        self.inner
            .tab
            .internal_sender()
            .clone()
            .send(InternalTargetMessage::FrameNavigate {
                frame_id: self.inner.frame_id.clone(),
                session_id: self.inner.session_id.clone(),
                req: request,
                tx,
            })
            .await?;
        let response = rx.await??;
        Ok(to_command_response::<NavigateParams>(response, method)?.result)
    }

    /// Wait for the next new-document navigation of this frame.
    pub async fn wait_for_navigation(&self) -> Result<()> {
        let (tx, rx) = oneshot_channel();
        self.inner
            .tab
            .internal_sender()
            .clone()
            .send(InternalTargetMessage::FrameWaitForNavigation {
                frame_id: self.inner.frame_id.clone(),
                session_id: self.inner.session_id.clone(),
                tx,
            })
            .await?;
        match rx.await? {
            Ok(()) => Ok(()),
            Err(FrameWaitError::Timeout) => Err(CdpError::Timeout),
            Err(FrameWaitError::FrameSwappedOrDetached) => Err(CdpError::FrameNotReady),
            Err(FrameWaitError::FrameNotFound { frame }) => Err(CdpError::FrameNotFound(frame)),
            Err(FrameWaitError::NavigationFailed(message)) => Err(CdpError::ChromeMessage(message)),
        }
    }

    /// Return currently bound direct child frames.
    pub async fn child_frames(&self) -> Result<Vec<Frame>> {
        Ok(self
            .inner
            .tab
            .all_frames()
            .await?
            .into_iter()
            .filter(|frame| frame.parent_id() == Some(self.id()))
            .collect())
    }

    /// Return the currently bound parent frame, if any.
    pub async fn parent(&self) -> Result<Option<Frame>> {
        let Some(parent_id) = self.parent_id().cloned() else {
            return Ok(None);
        };
        self.inner.tab.frame_by_id(parent_id).await
    }

    async fn pinned_execution_context(&self) -> Result<ExecutionContextInfo> {
        let (tx, rx) = oneshot_channel();
        self.inner
            .tab
            .internal_sender()
            .clone()
            .send(InternalTargetMessage::GetPinnedExecutionContext {
                dom_world: DOMWorldKind::Main,
                frame_id: self.inner.frame_id.clone(),
                expected_session_id: self.inner.session_id.clone(),
                tx,
            })
            .await?;
        let context = rx.await??.ok_or(CdpError::FrameNotReady)?;
        if context.session_id != self.inner.session_id {
            return Err(CdpError::FrameNotReady);
        }
        Ok(context)
    }

    async fn evaluate_expression(
        &self,
        mut expression: EvaluateParams,
    ) -> Result<EvaluationResult> {
        let context_id = self.pinned_context_id().await?;
        expression.context_id = Some(context_id);
        if expression.await_promise.is_none() {
            expression.await_promise = Some(true);
        }
        if expression.return_by_value.is_none() {
            expression.return_by_value = Some(true);
        }
        let fallback = expression
            .eval_as_function_fallback
            .is_some_and(|enabled| enabled)
            .then(|| expression.clone());
        let response = self.execute(expression).await?.result;
        if let Some(exception) = response.exception_details {
            return Err(CdpError::JavascriptException(Box::new(exception)));
        }
        let result = EvaluationResult::new(response.result);
        if result.object().r#type == RemoteObjectType::Function {
            if let Some(fallback) = fallback {
                return self.evaluate_function(fallback.into()).await;
            }
        }
        Ok(result)
    }

    async fn evaluate_function(
        &self,
        mut function: CallFunctionOnParams,
    ) -> Result<EvaluationResult> {
        let context_id = self.pinned_context_id().await?;
        function.execution_context_id = Some(context_id);
        if function.await_promise.is_none() {
            function.await_promise = Some(true);
        }
        if function.return_by_value.is_none() {
            function.return_by_value = Some(true);
        }
        let response = self.execute(function).await?.result;
        if let Some(exception) = response.exception_details {
            return Err(CdpError::JavascriptException(Box::new(exception)));
        }
        Ok(EvaluationResult::new(response.result))
    }

    async fn pinned_context_id(&self) -> Result<ExecutionContextId> {
        Ok(self.pinned_execution_context().await?.context_id)
    }

    async fn release_remote_objects_best_effort(
        &self,
        remote_object_ids: impl IntoIterator<Item = RemoteObjectId>,
    ) {
        for remote_object_id in remote_object_ids {
            if let Err(error) = self
                .execute(ReleaseObjectParams::new(remote_object_id))
                .await
            {
                // Cleanup must never replace the selector's original protocol
                // or JavaScript error, especially during a concurrent swap.
                tracing::warn!(%error, "failed to release temporary frame remote object");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::{FutureExt, StreamExt};
    use serde_json::json;

    use chromiumoxide_cdp::cdp::browser_protocol::target::TargetId;
    use chromiumoxide_types::{CallId, Response};

    use super::*;
    use crate::handler::PageHandle;

    fn test_frame() -> (Frame, PageHandle) {
        let page_handle = PageHandle::new(TargetId::new("target"), SessionId::new("main"), None);
        let frame = Frame::from_info(
            Arc::clone(page_handle.inner()),
            FrameInfo {
                frame_id: FrameId::new("child-frame"),
                session_id: SessionId::new("child"),
                main_session_id: SessionId::new("main"),
                parent_id: Some(FrameId::new("main-frame")),
                url: Some("https://child.example/".to_owned()),
                security_origin: "https://child.example".to_owned(),
            },
        );
        (frame, page_handle)
    }

    fn ok_response(result: serde_json::Value) -> Response {
        Response {
            id: CallId::new(1),
            result: Some(result),
            error: None,
        }
    }

    async fn answer_pinned_context(page_handle: &mut PageHandle) {
        let InternalTargetMessage::GetPinnedExecutionContext {
            frame_id,
            expected_session_id,
            tx,
            ..
        } = page_handle
            .internal_rx
            .next()
            .await
            .expect("pinned context query is sent")
        else {
            panic!("expected pinned execution-context query")
        };
        assert_eq!(frame_id, FrameId::new("child-frame"));
        assert_eq!(expected_session_id, SessionId::new("child"));
        tx.send(Ok(Some(ExecutionContextInfo {
            context_id: ExecutionContextId::new(42),
            session_id: SessionId::new("child"),
        })))
        .expect("pinned context receiver remains live");
    }

    async fn answer_selector_call(page_handle: &mut PageHandle, result: serde_json::Value) {
        let InternalTargetMessage::FrameCommand {
            frame_id,
            expected_session_id,
            command,
        } = page_handle
            .internal_rx
            .next()
            .await
            .expect("selector command is sent")
        else {
            panic!("expected selector frame command")
        };
        assert_eq!(frame_id, FrameId::new("child-frame"));
        assert_eq!(expected_session_id, SessionId::new("child"));
        assert_eq!(command.method.as_ref(), CallFunctionOnParams::IDENTIFIER);
        assert_eq!(
            command.params["functionDeclaration"],
            "function(sel) { return document.querySelector(sel); }"
        );
        assert_eq!(command.params["arguments"][0]["value"], "#target");
        assert_eq!(command.params["returnByValue"], false);
        assert_eq!(command.params["executionContextId"], 42);
        command
            .sender
            .send(Ok(ok_response(result)))
            .expect("selector response receiver remains live");
    }

    #[tokio::test]
    async fn frame_query_selector_returns_none_for_null() {
        let (frame, mut page_handle) = test_frame();
        let controller = async {
            answer_pinned_context(&mut page_handle).await;
            answer_selector_call(
                &mut page_handle,
                json!({
                    "result": {
                        "type": "object",
                        "subtype": "null",
                        "value": null
                    }
                }),
            )
            .await;
        };

        let (result, ()) = futures::join!(frame.query_selector("#target"), controller);
        assert!(result.expect("null selector succeeds").is_none());
        assert!(page_handle.internal_rx.next().now_or_never().is_none());
    }

    #[tokio::test]
    async fn frame_query_selector_describes_the_remote_node_on_the_pinned_path() {
        let (frame, mut page_handle) = test_frame();
        let controller = async {
            answer_pinned_context(&mut page_handle).await;
            answer_selector_call(
                &mut page_handle,
                json!({
                    "result": {
                        "type": "object",
                        "subtype": "node",
                        "objectId": "node"
                    }
                }),
            )
            .await;

            let InternalTargetMessage::FrameCommand {
                frame_id,
                expected_session_id,
                command,
            } = page_handle
                .internal_rx
                .next()
                .await
                .expect("describe command is sent")
            else {
                panic!("expected describe frame command")
            };
            assert_eq!(frame_id, FrameId::new("child-frame"));
            assert_eq!(expected_session_id, SessionId::new("child"));
            assert_eq!(command.method.as_ref(), DescribeNodeParams::IDENTIFIER);
            assert_eq!(command.params["objectId"], "node");
            command
                .sender
                .send(Ok(ok_response(json!({
                    "node": {
                        "nodeId": 0,
                        "backendNodeId": 7,
                        "nodeType": 1,
                        "nodeName": "BUTTON",
                        "localName": "button",
                        "nodeValue": ""
                    }
                }))))
                .expect("describe response receiver remains live");
        };

        let (result, ()) = futures::join!(frame.query_selector("#target"), controller);
        let element = result
            .expect("selector succeeds")
            .expect("selector returns an element");
        assert_eq!(element.remote_object_id, RemoteObjectId::new("node"));
        assert_eq!(
            element.backend_node_id,
            chromiumoxide_cdp::cdp::browser_protocol::dom::BackendNodeId::new(7)
        );
        assert!(element.node_id.is_none());
    }

    #[tokio::test]
    async fn frame_query_selector_releases_both_exception_object_ids() {
        let (frame, mut page_handle) = test_frame();
        let controller = async {
            answer_pinned_context(&mut page_handle).await;
            answer_selector_call(
                &mut page_handle,
                json!({
                    "result": { "type": "object", "objectId": "result" },
                    "exceptionDetails": {
                        "exceptionId": 1,
                        "text": "boom",
                        "lineNumber": 0,
                        "columnNumber": 0,
                        "exception": { "type": "object", "objectId": "exception" }
                    }
                }),
            )
            .await;

            for expected_object_id in ["result", "exception"] {
                let InternalTargetMessage::FrameCommand {
                    frame_id,
                    expected_session_id,
                    command,
                } = page_handle
                    .internal_rx
                    .next()
                    .await
                    .expect("cleanup command is sent")
                else {
                    panic!("expected cleanup frame command")
                };
                assert_eq!(frame_id, FrameId::new("child-frame"));
                assert_eq!(expected_session_id, SessionId::new("child"));
                assert_eq!(command.method.as_ref(), ReleaseObjectParams::IDENTIFIER);
                assert_eq!(command.params["objectId"], expected_object_id);
                command
                    .sender
                    .send(Ok(ok_response(json!({}))))
                    .expect("cleanup response receiver remains live");
            }
        };

        let (result, ()) = futures::join!(frame.query_selector("#target"), controller);
        assert!(matches!(result, Err(CdpError::JavascriptException(_))));
    }

    #[tokio::test]
    async fn frame_execute_rejects_navigate_without_emitting_work() {
        let (frame, mut page_handle) = test_frame();
        let result = frame
            .execute(NavigateParams::new("https://child.example/next"))
            .await;
        match result {
            Err(CdpError::NotAllowed(message)) => {
                assert!(
                    message.contains("Frame::goto"),
                    "message names the supported path: {message}"
                );
            }
            Err(other) => panic!("expected NotAllowed rejection, got {other:?}"),
            Ok(_) => panic!("expected NotAllowed rejection, got Ok"),
        }
        // The rejection must be immediate: no FrameCommand/FrameNavigate event
        // and therefore no CDP request may reach the target.
        assert!(page_handle.internal_rx.next().now_or_never().is_none());
    }
}
