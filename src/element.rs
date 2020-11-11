use crate::tab::TabInner;
use chromeoxid_types::{
    BackendNodeId, DescribeNodeParams, NodeId, QuerySelectorParams, RemoteObjectId,
    ResolveNodeParams,
};
use std::sync::Arc;

/// A handle to a [DOM Element](https://developer.mozilla.org/en-US/docs/Web/API/Element).
#[derive(Debug)]
pub struct Element {
    pub remote_object_id: RemoteObjectId,
    pub backend_node_id: BackendNodeId,
    pub node_id: NodeId,
    tab: Arc<TabInner>,
}

impl Element {
    pub(crate) async fn new(tab: Arc<TabInner>, node_id: NodeId) -> anyhow::Result<Self> {
        let backend_node_id = tab
            .execute(DescribeNodeParams::with_node_id_and_depth(node_id, 100))
            .await?
            .node
            .backend_node_id;

        let resp = tab
            .execute(ResolveNodeParams::with_backend_node(backend_node_id))
            .await?;

        let remote_object_id = resp
            .result
            .object
            .object_id
            .ok_or_else(|| anyhow::anyhow!("No object Id found for {:?}", node_id))?;
        Ok(Self {
            remote_object_id,
            backend_node_id,
            node_id,
            tab,
        })
    }

    pub async fn find_element(&self, selector: impl Into<String>) -> anyhow::Result<Self> {
        // TODO downcast to Option
        let node_id = self
            .tab
            .execute(QuerySelectorParams::new(self.node_id, selector.into()))
            .await?
            .node_id;

        Ok(Element::new(Arc::clone(&self.tab), node_id).await?)
    }
}

// TODO port ResolveNodeParams from cdp
