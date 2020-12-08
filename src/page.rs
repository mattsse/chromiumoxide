use std::path::Path;
use std::sync::Arc;

use futures::channel::mpsc::Sender;
use futures::channel::oneshot::channel as oneshot_channel;
use futures::{future, SinkExt};

use chromiumoxid_types::*;

use crate::cdp::browser_protocol;
use crate::cdp::browser_protocol::dom::*;
use crate::cdp::browser_protocol::network::{Cookie, GetCookiesParams, SetUserAgentOverrideParams};
use crate::cdp::browser_protocol::page::*;
use crate::cdp::browser_protocol::target::{ActivateTargetParams, SessionId, TargetId};
use crate::cdp::js_protocol;
use crate::cdp::js_protocol::debugger::GetScriptSourceParams;
use crate::cdp::js_protocol::runtime::{EvaluateParams, RemoteObject, ScriptId};
use crate::cmd::CommandMessage;
use crate::element::Element;
use crate::error::{CdpError, Result};
use crate::handler::PageInner;

#[derive(Debug)]
pub struct Page {
    inner: Arc<PageInner>,
}

impl Page {
    pub async fn execute<T: Command>(&self, cmd: T) -> Result<CommandResponse<T::Response>> {
        Ok(self.inner.execute(cmd).await?)
    }

    /// Navigate directly to the given URL.
    pub async fn goto(&self, params: impl Into<NavigateParams>) -> Result<FrameId> {
        let res = self.execute(params.into()).await?;
        if let Some(err) = res.result.error_text {
            return Err(CdpError::ChromeMessage(err));
        }

        Ok(res.result.frame_id)
    }

    /// The identifier of the `Target` this page belongs to
    pub fn target_id(&self) -> &TargetId {
        self.inner.target_id()
    }

    /// The identifier of the `Session` target of this page is attached to
    pub fn session_id(&self) -> &SessionId {
        self.inner.session_id()
    }

    /// Returns the current url of the page
    pub async fn current_url(&self) -> Result<String> {
        let res = self.execute(GetFrameTreeParams::default()).await?;
        Ok(res.result.frame_tree.frame.url)
    }

    /// Allows overriding user agent with the given string.
    pub async fn set_user_agent(
        &self,
        params: impl Into<SetUserAgentOverrideParams>,
    ) -> Result<&Self> {
        self.execute(params.into()).await?;
        Ok(self)
    }

    pub async fn get_document(&self) -> Result<Node> {
        let resp = self.execute(GetDocumentParams::default()).await?;
        Ok(resp.result.root)
    }

    pub async fn find_element(&self, selector: impl Into<String>) -> Result<Element> {
        let root = self.get_document().await?.node_id;
        let node_id = self
            .execute(QuerySelectorParams::new(root, selector))
            .await?
            .node_id;

        Ok(Element::new(Arc::clone(&self.inner), node_id).await?)
    }

    pub async fn find_elements(&self, selector: impl Into<String>) -> Result<Vec<Element>> {
        let root = self.get_document().await?.node_id;
        let resp = self
            .execute(QuerySelectorAllParams::new(root, selector))
            .await?;

        Ok(future::join_all(
            resp.result
                .node_ids
                .into_iter()
                .map(|id| Element::new(Arc::clone(&self.inner), id)),
        )
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?)
    }

    pub async fn describe_node(&self, node_id: NodeId) -> Result<Node> {
        let resp = self
            .execute(
                DescribeNodeParams::builder()
                    .node_id(node_id)
                    .depth(100)
                    .build(),
            )
            .await?;
        Ok(resp.result.node)
    }

    pub async fn close(self) {
        todo!()
    }

    /// Print the current page as pdf.
    ///
    /// See [`PrintToPdfParams`]
    pub async fn pdf(&self, opts: PrintToPdfParams) -> Result<Vec<u8>> {
        todo!()
        // let res = self.execute(opts).await?;
        // Ok(base64::decode(&res.data)?)
    }

    /// Save the current page as pdf as file
    pub async fn save_pdf(
        &self,
        opts: PrintToPdfParams,
        output: impl AsRef<Path>,
    ) -> Result<Vec<u8>> {
        todo!()
        // let pdf = self.pdf(opts).await?;
        // async_std::fs::write(output.as_ref(), &pdf).await?;
        // Ok(pdf)
    }

    /// Enables log domain.
    ///
    /// Sends the entries collected so far to the client by means of the
    /// entryAdded notification.
    ///
    /// See https://chromedevtools.github.io/devtools-protocol/tot/Log#method-enable
    pub async fn enable_log(&self) -> Result<&Self> {
        self.execute(browser_protocol::log::EnableParams::default())
            .await?;
        Ok(self)
    }

    /// Disables log domain
    ///
    /// Prevents further log entries from being reported to the client
    ///
    /// See https://chromedevtools.github.io/devtools-protocol/tot/Log#method-disable
    pub async fn disable_log(&self) -> Result<&Self> {
        self.execute(browser_protocol::log::DisableParams::default())
            .await?;
        Ok(self)
    }

    /// Enables runtime domain.
    pub async fn enable_runtime(&self) -> Result<&Self> {
        self.execute(js_protocol::runtime::EnableParams::default())
            .await?;
        Ok(self)
    }

    /// Disables runtime domain
    pub async fn disable_runtime(&self) -> Result<&Self> {
        self.execute(js_protocol::runtime::DisableParams::default())
            .await?;
        Ok(self)
    }

    /// Enables Debugger
    pub async fn enable_debugger(&self) -> Result<&Self> {
        self.execute(js_protocol::debugger::EnableParams::default())
            .await?;
        Ok(self)
    }

    /// Disables Debugger
    pub async fn disable_debugger(&self) -> Result<&Self> {
        self.execute(js_protocol::debugger::DisableParams::default())
            .await?;
        Ok(self)
    }

    /// Activates (focuses) the target.
    pub async fn activate(&self) -> Result<&Self> {
        self.execute(ActivateTargetParams::new(self.inner.target_id().clone()))
            .await?;
        Ok(self)
    }

    /// Returns all cookies that match the tab's current URL.
    pub async fn get_cookies(&self) -> Result<Vec<Cookie>> {
        Ok(self
            .execute(GetCookiesParams::default())
            .await?
            .result
            .cookies)
    }

    /// Returns the title of the document.
    pub async fn get_title(&self) -> Result<Option<String>> {
        let remote_object = self.evaluate("document.title").await?;
        let title: String = serde_json::from_value(
            remote_object
                .value
                .ok_or_else(|| CdpError::msg("No title found"))?,
        )?;
        if title.is_empty() {
            Ok(None)
        } else {
            Ok(Some(title))
        }
    }

    /// Evaluates expression on global object.
    pub async fn evaluate(&self, evaluate: impl Into<EvaluateParams>) -> Result<RemoteObject> {
        Ok(self.execute(evaluate.into()).await?.result.result)
    }

    /// Returns source for the script with given id.
    ///
    /// Debugger must be enabled.
    pub async fn get_script_source(&self, script_id: impl Into<String>) -> Result<String> {
        Ok(self
            .execute(GetScriptSourceParams::new(ScriptId::from(script_id.into())))
            .await?
            .result
            .script_source)
    }
}

async fn execute<T: Command>(
    cmd: T,
    mut sender: Sender<CommandMessage>,
    session: Option<SessionId>,
) -> Result<CommandResponse<T::Response>> {
    let (tx, rx) = oneshot_channel();
    let method = cmd.identifier();
    let msg = CommandMessage::with_session(cmd, tx, session)?;

    sender.send(msg).await?;
    let resp = rx.await??;

    if let Some(res) = resp.result {
        let result = serde_json::from_value(res)?;
        Ok(CommandResponse {
            id: resp.id,
            result,
            method,
        })
    } else if let Some(err) = resp.error {
        Err(err.into())
    } else {
        Err(CdpError::NoResponse)
    }
}

impl From<Arc<PageInner>> for Page {
    fn from(inner: Arc<PageInner>) -> Self {
        Self { inner }
    }
}
