use std::path::Path;
use std::sync::Arc;

use futures::channel::oneshot::channel as oneshot_channel;
use futures::SinkExt;

use chromiumoxide_cdp::cdp::browser_protocol;
use chromiumoxide_cdp::cdp::browser_protocol::dom::*;
use chromiumoxide_cdp::cdp::browser_protocol::network::{
    Cookie, GetCookiesParams, SetUserAgentOverrideParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::page::*;
use chromiumoxide_cdp::cdp::browser_protocol::target::{ActivateTargetParams, SessionId, TargetId};
use chromiumoxide_cdp::cdp::js_protocol;
use chromiumoxide_cdp::cdp::js_protocol::debugger::GetScriptSourceParams;
use chromiumoxide_cdp::cdp::js_protocol::runtime::{EvaluateParams, RemoteObject, ScriptId};
use chromiumoxide_types::*;

use crate::element::Element;
use crate::error::{CdpError, Result};
use crate::handler::target::TargetMessage;
use crate::handler::PageInner;
use crate::layout::Point;

#[derive(Debug)]
pub struct Page {
    inner: Arc<PageInner>,
}

impl Page {
    /// Execute a command and return the `Command::Response`
    pub async fn execute<T: Command>(&self, cmd: T) -> Result<CommandResponse<T::Response>> {
        Ok(self.inner.execute(cmd).await?)
    }

    /// This resolves once the navigation finished and the page is loaded.
    ///
    /// This is necessary after an interaction with the page that may trigger a
    /// navigation (`click`, `press_key`) in order to wait until the new browser
    /// page is loaded
    pub async fn wait_for_navigation(&self) -> Result<&Self> {
        self.inner.wait_for_navigation().await?;
        Ok(self)
    }

    /// Navigate directly to the given URL.
    ///
    /// This resolves directly after the requested URL is fully loaded.
    pub async fn goto(&self, params: impl Into<NavigateParams>) -> Result<&Self> {
        let res = self.execute(params.into()).await?;
        if let Some(err) = res.result.error_text {
            return Err(CdpError::ChromeMessage(err));
        }

        Ok(self)
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
    pub async fn url(&self) -> Result<Option<String>> {
        let (tx, rx) = oneshot_channel();
        self.inner
            .sender()
            .clone()
            .send(TargetMessage::Url(tx))
            .await?;
        Ok(rx.await?)
    }

    /// Return the main frame of the page
    pub async fn mainframe(&self) -> Result<Option<FrameId>> {
        let (tx, rx) = oneshot_channel();
        self.inner
            .sender()
            .clone()
            .send(TargetMessage::MainFrame(tx))
            .await?;
        Ok(rx.await?)
    }

    /// Allows overriding user agent with the given string.
    pub async fn set_user_agent(
        &self,
        params: impl Into<SetUserAgentOverrideParams>,
    ) -> Result<&Self> {
        self.execute(params.into()).await?;
        Ok(self)
    }

    /// Returns the root DOM node (and optionally the subtree) of the page.
    ///
    /// # Note: This does not return the actual HTML document of the page. To
    /// retrieve the HTML content of the page see `Page::content`.
    pub async fn get_document(&self) -> Result<Node> {
        let resp = self.execute(GetDocumentParams::default()).await?;
        Ok(resp.result.root)
    }

    /// Returns the first element in the document which matches the given CSS
    /// selector.
    ///
    /// Execute a query selector on the document's node.
    pub async fn find_element(&self, selector: impl Into<String>) -> Result<Element> {
        let root = self.get_document().await?.node_id;
        let node_id = self.inner.find_element(selector, root).await?;
        Ok(Element::new(Arc::clone(&self.inner), node_id).await?)
    }

    /// Return all `Element`s in the document that match the given selector
    pub async fn find_elements(&self, selector: impl Into<String>) -> Result<Vec<Element>> {
        let root = self.get_document().await?.node_id;
        let node_ids = self.inner.find_elements(selector, root).await?;
        Ok(Element::from_nodes(&self.inner, &node_ids).await?)
    }

    /// Describes node given its id
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

    /// Moves the mouse to this point (dispatches a mouseMoved event)
    pub async fn move_mouse_to_point(&self, point: Point) -> Result<&Self> {
        self.inner.move_mouse_to_point(point).await?;
        Ok(self)
    }

    /// Performs a mouse click event at the point's location.
    ///
    /// Bear in mind that if `click()` triggers a navigation the new page is not
    /// immediately loaded when `click()` resolves. To wait until navigation is
    /// finished an additional `wait_for_navigation()` is required:
    ///
    /// # Example
    ///
    /// Trigger a navigation and wait until the triggered navigation is finished
    ///
    /// ```no_run
    /// # use chromiumoxide::page::Page;
    /// # use chromiumoxide::error::Result;
    /// # use chromiumoxide::layout::Point;
    /// # async fn demo(page: Page, point: Point) -> Result<()> {
    ///     let html = page.click(point).await?.wait_for_navigation().await?.content();
    ///     # Ok(())
    /// # }
    /// ```
    pub async fn click(&self, point: Point) -> Result<&Self> {
        self.inner.click(point).await?;
        Ok(self)
    }

    /// Print the current page as pdf.
    ///
    /// See [`PrintToPdfParams`]
    ///
    /// # Note Generating a pdf is currently only supported in Chrome headless.
    pub async fn pdf(&self, opts: PrintToPdfParams) -> Result<Vec<u8>> {
        let res = self.execute(opts).await?;
        Ok(base64::decode(&res.data)?)
    }

    /// Save the current page as pdf as file to the `output` path and return the
    /// pdf contents.
    ///
    /// # Note Generating a pdf is currently only supported in Chrome headless.
    pub async fn save_pdf(
        &self,
        opts: PrintToPdfParams,
        output: impl AsRef<Path>,
    ) -> Result<Vec<u8>> {
        let pdf = self.pdf(opts).await?;
        async_std::fs::write(output.as_ref(), &pdf).await?;
        Ok(pdf)
    }

    /// Enables log domain. Enabled by default.
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

    /// Enables runtime domain. Activated by default.
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

    /// Enables Debugger. Enabled by default.
    pub async fn enable_debugger(&self) -> Result<&Self> {
        self.execute(js_protocol::debugger::EnableParams::default())
            .await?;
        Ok(self)
    }

    /// Disables Debugger.
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

    /// Returns the HTML content of the page
    pub async fn content(&self) -> Result<String> {
        let resp = self
            .evaluate(
                "{
          let retVal = '';
          if (document.doctype) {
            retVal = new XMLSerializer().serializeToString(document.doctype);
          }
          if (document.documentElement) {
            retVal += document.documentElement.outerHTML;
          }
          retVal
      }
      ",
            )
            .await?;
        let value = resp.value.ok_or(CdpError::NotFound)?;
        Ok(serde_json::from_value(value)?)
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

impl From<Arc<PageInner>> for Page {
    fn from(inner: Arc<PageInner>) -> Self {
        Self { inner }
    }
}
