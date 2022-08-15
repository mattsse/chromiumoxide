use futures::channel::{
    mpsc,
    oneshot::{self, channel as oneshot_channel},
};
use pin_project_lite::pin_project;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use crate::error::Result;
use crate::handler::http::HttpRequest;
use crate::handler::target::TargetMessage;

type ArcRequest = Option<Arc<HttpRequest>>;

pin_project! {
pub struct NavigationFuture {
    #[pin]
    rx_request: oneshot::Receiver<ArcRequest>,
    #[pin]
    target_sender: mpsc::Sender<TargetMessage>,

    message: Option<TargetMessage>,
}
}

impl NavigationFuture {
    pub fn new(target_sender: mpsc::Sender<TargetMessage>) -> Self {
        let (tx, rx_request) = oneshot_channel();

        let message = Some(TargetMessage::WaitForNavigation(tx));

        Self {
            target_sender,
            rx_request,
            message,
        }
    }
}

impl Future for NavigationFuture {
    type Output = Result<ArcRequest>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();

        if this.message.is_some() {
            match this.target_sender.poll_ready(cx) {
                Poll::Ready(Err(e)) => Poll::Ready(Err(e.into())),
                Poll::Ready(Ok(_)) => {
                    let message = this.message.take().expect("existence checked above");
                    let _ = this.target_sender.start_send(message)?;

                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
                Poll::Pending => Poll::Pending,
            }
        } else {
            this.rx_request.as_mut().poll(cx).map_err(Into::into)
        }
    }
}
