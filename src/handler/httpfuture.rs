use futures::FutureExt;
use futures::channel::mpsc;
use futures::future::{BoxFuture, Fuse, FusedFuture};
use pin_project_lite::pin_project;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::handler::commandfuture::CommandFuture;
use crate::handler::target::TargetMessage;
use crate::handler::target_message_future::TargetMessageFuture;
use crate::{ArcHttpRequest, Result};
use chromiumoxide_types::Command;

type ArcRequest = ArcHttpRequest;

pin_project! {
    pub struct HttpFuture<T: Command> {
        #[pin]
        command: Fuse<CommandFuture<T>>,
        #[pin]
        navigation: BoxFuture<'static, Result<ArcHttpRequest>>,
    }
}

impl<T: Command> HttpFuture<T> {
    pub fn new(sender: mpsc::Sender<TargetMessage>, command: CommandFuture<T>) -> Self {
        Self {
            command: command.fuse(),
            navigation: TargetMessageFuture::<T>::wait_for_navigation(sender).boxed(),
        }
    }

    pub(crate) fn with_navigation(
        command: CommandFuture<T>,
        navigation: BoxFuture<'static, Result<ArcHttpRequest>>,
    ) -> Self {
        Self {
            command: command.fuse(),
            navigation,
        }
    }
}

impl<T> Future for HttpFuture<T>
where
    T: Command,
{
    type Output = Result<ArcRequest>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        // 1. First complete command request future
        // 2. Switch polls navigation
        if this.command.is_terminated() {
            this.navigation.poll(cx)
        } else {
            match this.command.poll(cx) {
                Poll::Ready(Ok(_command_response)) => {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
                Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                Poll::Pending => Poll::Pending,
            }
        }
    }
}
