use std::sync::Arc;

use futures::channel::mpsc::Sender;
use futures::channel::oneshot::channel as oneshot_channel;
use futures::{future, FutureExt, SinkExt, TryFutureExt};

use chromeoxid_types::*;

use crate::browser::CommandMessage;
use crate::element::Element;

#[derive(Debug)]
pub(crate) struct TabInner {
    target_id: TargetId,
    session_id: SessionId,
    commands: Sender<CommandMessage>,
}

impl TabInner {
    pub(crate) async fn execute<T: Command>(
        &self,
        cmd: T,
    ) -> anyhow::Result<CommandResponse<T::Response>> {
        Ok(execute(cmd, self.commands.clone()).await?)
    }
}

#[derive(Debug)]
pub struct Tab {
    inner: Arc<TabInner>,
}

impl Tab {
    pub(crate) async fn new(
        target_id: TargetId,
        commands: Sender<CommandMessage>,
    ) -> anyhow::Result<Self> {
        let resp = execute(
            AttachToTargetParams::new(target_id.clone()),
            commands.clone(),
        )
        .await?;

        let inner = Arc::new(TabInner {
            target_id,
            commands,
            session_id: resp.result.session_id,
        });

        Ok(Self { inner })
    }

    pub async fn execute<T: Command>(
        &self,
        cmd: T,
    ) -> anyhow::Result<CommandResponse<T::Response>> {
        Ok(self.inner.execute(cmd).await?)
    }

    pub async fn get_document(&self) -> anyhow::Result<Node> {
        let resp = execute(GetDocumentParams::default(), self.inner.commands.clone()).await?;
        Ok(resp.result.root)
    }

    pub async fn find_element(&self, selector: impl Into<String>) -> anyhow::Result<Element> {
        let root = self.get_document().await?;
        let node_id = self
            .execute(QuerySelectorParams::new(root.node_id, selector.into()))
            .await?
            .node_id;
        Ok(Element::new(Arc::clone(&self.inner), node_id).await?)
    }

    pub async fn find_elements(&self, selector: impl Into<String>) -> anyhow::Result<Vec<Element>> {
        let root = self.get_document().await?;
        let resp = self
            .execute(QuerySelectorAllParams::new(root.node_id, selector.into()))
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

    pub async fn describe_node(&self, node_id: NodeId) -> anyhow::Result<Node> {
        let resp = self
            .execute(DescribeNodeParams::with_node_id_and_depth(node_id, 100))
            .await?;
        Ok(resp.result.node)
    }
}

async fn execute<T: Command>(
    cmd: T,
    mut sender: Sender<CommandMessage>,
) -> anyhow::Result<CommandResponse<T::Response>> {
    let (tx, rx) = oneshot_channel();
    let method = cmd.identifier();
    let msg = CommandMessage::new(cmd, tx)?;

    sender.send(msg).await?;
    let resp = rx.await?;

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
        Err(anyhow::anyhow!("Empty Response"))
    }
}
