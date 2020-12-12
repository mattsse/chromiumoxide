use crate::error::{CdpError, Result};
use futures::future;
use std::sync::Arc;

use crate::handler::PageInner;
use chromiumoxid_cdp::cdp::browser_protocol::dom::{
    BackendNodeId, DescribeNodeParams, NodeId,
    ResolveNodeParams,
};
use chromiumoxid_cdp::cdp::js_protocol::runtime::RemoteObjectId;

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
        Ok(Element::from_nodes(&self.tab, &self.tab.find_elements(selector, self.node_id).await?).await?)
    }
}
