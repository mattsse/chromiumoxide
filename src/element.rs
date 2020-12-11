use crate::error::{CdpError, Result};
use std::sync::Arc;

use crate::handler::PageInner;
use chromiumoxid_tmp::cdp::browser_protocol::dom::{
    BackendNodeId, DescribeNodeParams, NodeId, QuerySelectorParams, ResolveNodeParams,
};
use chromiumoxid_tmp::cdp::js_protocol::runtime::RemoteObjectId;

/// A handle to a [DOM Element](https://developer.mozilla.org/en-US/docs/Web/API/Element).
#[derive(Debug)]
pub struct Element {
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

    pub async fn find_element(&self, selector: impl Into<String>) -> Result<Self> {
        // TODO downcast to Option
        let node_id = self
            .tab
            .execute(QuerySelectorParams::new(self.node_id, selector))
            .await?
            .node_id;

        Ok(Element::new(Arc::clone(&self.tab), node_id).await?)
    }
}

// TODO port ResolveNodeParams from cdp
