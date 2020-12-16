use std::borrow::Cow;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};
use futures::Stream;

use crate::CustomEvent;
use chromiumoxide_cdp::cdp::{CustomJsonEvent, Event};

// use futures::channel::oneshot::{Receiver, Sender};
// use futures::stream::{FusedStream, StreamExt};

pub struct Subscriptions {
    subs: HashMap<Cow<'static, str>, Vec<EventSubscription>>,
}

pub struct EventSubscription {
    listener: UnboundedSender<Arc<dyn Event>>,
}

impl EventSubscription {
    fn _send(&self) {
        // 1. poll ready, if `SendErrorKind::Disconnected` --> receiver
        // disconnected 2. start_send
        // 3. poll flush
        // SendErrorKind::Disconnected when reiver dropped
    }
}

/// The receiver part of an event subscription
pub struct EventStream<T: Event> {
    events: UnboundedReceiver<Arc<dyn Event>>,
    _marker: PhantomData<T>,
}

impl<T: Event> EventStream<T> {}

impl<T: Event + Unpin> Stream for EventStream<T> {
    type Item = Arc<T>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let pin = self.get_mut();
        match Stream::poll_next(Pin::new(&mut pin.events), cx) {
            Poll::Ready(Some(event)) => {
                if let Ok(e) = event.into_any_arc().downcast() {
                    Poll::Ready(Some(e))
                } else {
                    Poll::Pending
                }
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub struct CustomEventStream<T: CustomEvent> {
    events: UnboundedReceiver<Arc<dyn Event>>,
    _marker: PhantomData<T>,
}

impl<T: CustomEvent + Unpin> Stream for CustomEventStream<T> {
    type Item = serde_json::Result<T>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let pin = self.get_mut();
        match Stream::poll_next(Pin::new(&mut pin.events), cx) {
            Poll::Ready(Some(event)) => {
                if let Ok(e) = event.into_any_arc().downcast::<CustomJsonEvent>() {
                    Poll::Ready(Some(serde_json::from_value(e.params.clone())))
                } else {
                    Poll::Pending
                }
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}
