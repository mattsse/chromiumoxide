use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};
use futures::{Sink, SinkExt, Stream, StreamExt};

use chromiumoxide_cdp::cdp::{Event, IntoEventKind, CustomEvent};
use chromiumoxide_types::Method;
use std::ops::Deref;

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

#[async_std::test]
async fn event_stream() {
    use chromiumoxide_cdp::cdp::browser_protocol::animation::EventAnimationCanceled;

    let (mut tx, rx) = futures::channel::mpsc::unbounded();

    let mut stream = EventStream::<EventAnimationCanceled>::new(rx);

    let event = EventAnimationCanceled {
        id: "id".to_string(),
    };
    let msg: Arc<dyn Event> = Arc::new(event.clone());

    tx.send(msg).await.unwrap();
    let next = stream.next().await.unwrap();
    assert_eq!(&*next, &event);
}

#[async_std::test]
async fn custom_event_stream() {
    use serde::Deserialize;

    #[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
    struct MyCustomEvent {
        name: String
    }

    impl Method for MyCustomEvent {
        fn identifier(&self) -> Cow<'static, str> {
           "Custom.Event".into()
        }
    }

    impl CustomEvent for MyCustomEvent {}

    let (mut tx, rx) = futures::channel::mpsc::unbounded();

    let mut stream = EventStream::<MyCustomEvent>::new(rx);

    let event = MyCustomEvent {name: "my event".to_string()};
    let msg: Arc<dyn Event> = Arc::new(event.clone());
    tx.send(msg).await.unwrap();
    let next = stream.next().await.unwrap();
    assert_eq!(&*next, &event);
}
