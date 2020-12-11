use chromiumoxid_tmp::cdp::browser_protocol::target::{SessionId, TargetId};
use crate::handler::target::TargetMessage;
use chromiumoxid_types::{Command, CommandResponse};
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::channel::oneshot::channel as oneshot_channel;
use futures::stream::Fuse;
use std::sync::Arc;

use crate::cmd::CommandMessage;
use crate::error::{CdpError, Result};
use futures::{SinkExt, StreamExt};

#[derive(Debug)]
pub struct PageHandle {
    pub(crate) rx: Fuse<Receiver<TargetMessage>>,
    page: Arc<PageInner>,
}

impl PageHandle {
    pub fn new(target_id: TargetId, session_id: SessionId) -> Self {
        let (commands, rx) = channel(1);
        let page = PageInner {
            target_id,
            session_id,
            commands,
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
    commands: Sender<TargetMessage>,
}

impl PageInner {
    pub(crate) async fn execute<T: Command>(&self, cmd: T) -> Result<CommandResponse<T::Response>> {
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
) -> Result<CommandResponse<T::Response>> {
    let (tx, rx) = oneshot_channel();
    let method = cmd.identifier();
    let msg = CommandMessage::with_session(cmd, tx, session)?;

    sender.send(TargetMessage::Command(msg)).await?;
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
