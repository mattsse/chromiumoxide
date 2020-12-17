use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};
use futures::{Sink, SinkExt, Stream, StreamExt};

use chromiumoxide_cdp::cdp::{Event, IntoEventKind};

// use futures::channel::oneshot::{Receiver, Sender};
// use futures::stream::{FusedStream, StreamExt};

/// All the currently active subscriptions
pub struct Subscriptions {
    /// Tracks the subscribers for each event identified by the key
    subs: HashMap<Cow<'static, str>, Vec<EventSubscription>>,
}

impl Subscriptions {}

/// Represents a single event listner
pub struct EventSubscription {
    /// the sender half of the event channel
    listener: UnboundedSender<Arc<dyn Event>>,
    queued_events: VecDeque<Arc<dyn Event>>,
}

impl EventSubscription {
    pub fn send(&mut self, event: Arc<dyn Event>) {
        self.queued_events.push_back(event)
    }

    pub fn poll(&mut self, cx: &mut Context<'_>) -> Result<(), ()> {
        loop {
            match Sink::poll_ready(Pin::new(&mut self.listener), cx) {
                Poll::Ready(Ok(_)) => {}
                Poll::Ready(Err(_)) => {
                    // disconnected
                    return Err(());
                }
                Poll::Pending => {
                    return Ok(());
                }
            }
            if let Some(event) = self.queued_events.pop_front() {
                if Sink::start_send(Pin::new(&mut self.listener), event).is_err() {
                    return Err(());
                }
            } else {
                return Ok(());
            }
        }
    }
}

/// The receiver part of an event subscription
pub struct EventStream<T: IntoEventKind> {
    events: UnboundedReceiver<Arc<dyn Event>>,
    _marker: PhantomData<T>,
}

impl<T: IntoEventKind> EventStream<T> {
    pub fn new(events: UnboundedReceiver<Arc<dyn Event>>) -> Self {
        Self {
            events,
            _marker: PhantomData,
        }
    }
}

impl<T: IntoEventKind + Unpin> Stream for EventStream<T> {
    type Item = Arc<T>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
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
