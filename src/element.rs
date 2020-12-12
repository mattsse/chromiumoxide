use std::sync::Arc;

use futures::future;

use chromiumoxid_cdp::cdp::browser_protocol::dom::{
    BackendNodeId, DescribeNodeParams, GetContentQuadsParams, NodeId, ResolveNodeParams,
};
use chromiumoxid_cdp::cdp::js_protocol::runtime::{
    CallFunctionOnParams, CallFunctionOnReturns, RemoteObjectId, RemoteObjectType,
};

use crate::box_model::{ElementQuad, Point};
use crate::error::{CdpError, Result};
use crate::handler::PageInner;

/// A handle to a [DOM Element](https://developer.mozilla.org/en-US/docs/Web/API/Element).
#[derive(Debug)]
pub struct Element {
    /// The Unique object identifier
    pub remote_object_id: RemoteObjectId,
    pub backend_node_id: BackendNodeId,
    pub node_id: NodeId,
    tab: Arc<PageInner>,
}

impl Element {
    pub(crate) async fn new(tab: Arc<PageInner>, node_id: NodeId) -> Result<Self> {
        let backend_node_id = tab
            .execute(
                DescribeNodeParams::builder()
                    .node_id(node_id)
                    .depth(100)
                    .build(),
            )
            .await?
            .node
            .backend_node_id;

        let resp = tab
            .execute(
                ResolveNodeParams::builder()
                    .backend_node_id(backend_node_id)
                    .build(),
            )
            .await?;

        let remote_object_id = resp
            .result
            .object
            .object_id
            .ok_or_else(|| CdpError::msg(format!("No object Id found for {:?}", node_id)))?;
        Ok(Self {
            remote_object_id,
            backend_node_id,
            node_id,
            tab,
        })
    }

    /// Convert a slice of `NodeId`s into a `Vec` of `Element`s
    pub(crate) async fn from_nodes(tab: &Arc<PageInner>, node_ids: &[NodeId]) -> Result<Vec<Self>> {
        Ok(future::join_all(
            node_ids
                .into_iter()
                .copied()
                .map(|id| Element::new(Arc::clone(tab), id)),
        )
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?)
    }

    /// Returns the first element in the document which matches the given CSS
    /// selector.
    pub async fn find_element(&self, selector: impl Into<String>) -> Result<Self> {
        let node_id = self.tab.find_element(selector, self.node_id).await?;
        Ok(Element::new(Arc::clone(&self.tab), node_id).await?)
    }

    /// Return all `Element`s in the document that match the given selector
    pub async fn find_elements(&self, selector: impl Into<String>) -> Result<Vec<Element>> {
        Ok(Element::from_nodes(
            &self.tab,
            &self.tab.find_elements(selector, self.node_id).await?,
        )
        .await?)
    }

    /// Returns the best `Point` of this node to execute a click on.
    pub async fn clickable_point(&self) -> Result<Point> {
        let content_quads = self
            .tab
            .execute(
                GetContentQuadsParams::builder()
                    .backend_node_id(self.backend_node_id)
                    .build(),
            )
            .await?;
        content_quads
            .quads
            .iter()
            .filter(|q| q.inner().len() == 8)
            .map(|q| ElementQuad::from_quad(q))
            .filter(|q| q.quad_area() > 1.)
            .map(|q| q.quad_center())
            .next()
            .ok_or_else(|| CdpError::msg("Node is either not visible or not an HTMLElement"))
    }

    /// Calls function with given declaration on the element
    pub async fn call_js_fn(
        &self,
        function_declaration: impl Into<String>,
        await_promise: bool,
    ) -> Result<CallFunctionOnReturns> {
        Ok(self
            .tab
            .call_js_fn(
                function_declaration,
                await_promise,
                self.remote_object_id.clone(),
            )
            .await?)
    }

    pub async fn scroll_into_view(&self) -> Result<&Self> {
        let resp = self
            .call_js_fn(
                "async function() {
                if (!this.isConnected)
                    return 'Node is detached from document';
                if (this.nodeType !== Node.ELEMENT_NODE)
                    return 'Node is not of type HTMLElement';

                const visibleRatio = await new Promise(resolve => {
                    const observer = new IntersectionObserver(entries => {
                        resolve(entries[0].intersectionRatio);
                        observer.disconnect();
                    });
                    observer.observe(this);
                });

                if (visibleRatio !== 1.0)
                    this.scrollIntoView({
                        block: 'center',
                        inline: 'center',
                        behavior: 'instant'
                    });
                return false;
            }",
                true,
            )
            .await?;

        if resp.result.r#type == RemoteObjectType::String {
            let error_text = resp.result.value.unwrap().as_str().unwrap().to_string();
            return Err(CdpError::ScrollingFailed(error_text));
        }
        Ok(self)
    }

    /// Click on the element
    pub async fn click(&self) -> Result<&Self> {
        let center = self.scroll_into_view().await?.clickable_point().await?;
        self.tab.click_point(center).await?;
        Ok(self)
    }

    pub async fn type_str(&self, input: impl AsRef<str>) -> Result<&Self> {
        self.tab.type_str(input).await?;
        Ok(self)
    }

    pub async fn press_key(&self, key: impl AsRef<str>) -> Result<&Self> {
        self.tab.press_key(key).await?;
        Ok(self)
    }

    /// The inner text of this element.
    pub async fn inner_text(&self) -> Result<Option<String>> {
        Ok(self.get_string_property("innerText").await?)
    }

    /// The inner HTML of this element.
    pub async fn inner_html(&self) -> Result<Option<String>> {
        Ok(self.get_string_property("innerHTML").await?)
    }

    /// The outer HTML of this element.
    pub async fn outer_html(&self) -> Result<Option<String>> {
        Ok(self.get_string_property("outerHTML").await?)
    }

    /// Returns the string property of the element.
    ///
    /// If the property is an empty String, `None` is returned.
    pub async fn get_string_property(&self, property: impl AsRef<str>) -> Result<Option<String>> {
        let property = property.as_ref();
        let value = self
            .get_property(property)
            .await?
            .ok_or_else(|| CdpError::NotFound)?;
        let txt: String = serde_json::from_value(value)?;
        if txt.is_empty() {
            Ok(Some(txt))
        } else {
            Ok(None)
        }
    }

    /// Returns the javascript `property` of this element
    pub async fn get_property(
        &self,
        property: impl AsRef<str>,
    ) -> Result<Option<serde_json::Value>> {
        let js_fn = format!("function() {{ return this.{}; }}", property.as_ref());
        let resp = self.call_js_fn(js_fn, false).await?;
        Ok(resp.result.value)
    }
}
