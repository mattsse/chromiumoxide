use crate::cdp::browser_protocol::target::{SessionId, TargetId};
use crate::handler::target::{TargetEvent, TargetMessage};
use chromiumoxid_types::{Command, CommandResponse};
use futures::channel::mpsc::Receiver;
use futures::channel::mpsc::Sender;
use futures::channel::oneshot::channel as oneshot_channel;
use futures::stream::Fuse;
use std::sync::Arc;

use crate::browser::CommandMessage;
use crate::error::CdpError;
use async_std::pin::Pin;
use futures::task::{Context, Poll};
use futures::SinkExt;

pub struct PageHandle {
    pub(crate) rx: Fuse<Receiver<TargetMessage>>,
    page: Arc<PageInner>,
}

#[derive(Debug)]
pub(crate) struct PageInner {
    target_id: TargetId,
    session_id: SessionId,
    commands: Sender<TargetMessage>,
}

impl PageInner {
    pub(crate) async fn execute<T: Command>(
        &self,
        cmd: T,
    ) -> Result<CommandResponse<T::Response>, CdpError> {
        Ok(execute(cmd, self.commands.clone(), Some(self.session_id.clone())).await?)
    }

    pub fn target_id(&self) -> &TargetId {
        &self.target_id
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }
}

async fn execute<T: Command>(
    cmd: T,
    mut sender: Sender<TargetMessage>,
    session: Option<SessionId>,
) -> Result<CommandResponse<T::Response>, CdpError> {
    let (tx, rx) = oneshot_channel();
    let method = cmd.identifier();
    let msg = CommandMessage::with_session(cmd, tx, session)?;

    sender.send(TargetMessage::Command(msg)).await?;
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
        Err(CdpError::NoResponse)
    }
}
