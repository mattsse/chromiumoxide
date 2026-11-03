use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::Stream;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};

use chromiumoxide_cdp::cdp::{Event, EventKind, IntoEventKind};
use chromiumoxide_types::MethodId;

/// All the currently active listeners
#[derive(Debug, Default)]
pub struct EventListeners {
    /// Tracks the listeners for each event identified by the key
    listeners: HashMap<MethodId, Vec<EventListener>>,
}

impl EventListeners {
    /// Register a subscription for a method
    pub fn add_listener(&mut self, req: EventListenerRequest) {
        let EventListenerRequest {
            listener,
            method,
            kind,
        } = req;
        let subs = self.listeners.entry(method).or_default();
        subs.push(EventListener { listener, kind });
    }

    /// Queue in a event that should be send to all listeners
    pub fn start_send<T: Event>(&mut self, event: T) {
        if let Some(subscriptions) = self.listeners.get_mut(&T::method_id()) {
            let event: Arc<dyn Event> = Arc::new(event);
            subscriptions
                .iter_mut()
                .for_each(|sub| sub.start_send(Arc::clone(&event)));
        }
    }

    /// Try to queue in a new custom event if a listener is registered and the
    /// converting the json value to the registered event type succeeds
    pub fn try_send_custom(
        &mut self,
        method: &str,
        val: serde_json::Value,
    ) -> serde_json::Result<()> {
        if let Some(subscriptions) = self.listeners.get_mut(method) {
            let mut event = None;
            if let Some(json_to_arc_event) = subscriptions
                .iter()
                .filter_map(|sub| {
                    if let EventKind::Custom(conv) = &sub.kind {
                        Some(conv)
                    } else {
                        None
                    }
                })
                .next()
            {
                event = Some(json_to_arc_event(val)?);
            }
            if let Some(event) = event {
                subscriptions
                    .iter_mut()
                    .filter(|sub| sub.kind.is_custom())
                    .for_each(|sub| sub.start_send(Arc::clone(&event)));
            }
        }
        Ok(())
    }

    /// Does the housekeeping when the receiver part of a subscription is dropped. Events are not
    /// queued here, so there is nothing to drain: see `EventListener::start_send`.
    pub fn poll(&mut self, _cx: &mut Context<'_>) {
        for subscriptions in self.listeners.values_mut() {
            subscriptions.retain(|sub| !sub.is_disconnected());
        }
    }
}

pub struct EventListenerRequest {
    listener: UnboundedSender<Arc<dyn Event>>,
    method: MethodId,
    kind: EventKind,
}

impl EventListenerRequest {
    pub fn new<T: IntoEventKind>(listener: UnboundedSender<Arc<dyn Event>>) -> Self {
        Self {
            listener,
            method: T::method_id(),
            kind: T::event_kind(),
        }
    }
}

impl fmt::Debug for EventListenerRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventListenerRequest")
            .field("method", &self.method)
            .field("kind", &self.kind)
            .finish()
    }
}

/// Represents a single event listener
pub struct EventListener {
    /// the sender half of the event channel
    listener: UnboundedSender<Arc<dyn Event>>,
    /// For what kind of event this event is for
    kind: EventKind,
}

impl EventListener {
    /// Sends an event to the subscription's channel.
    ///
    /// The event goes out here rather than onto a queue drained by a later `poll`. The channel is
    /// unbounded, so `Sink::poll_ready` for it never returns `Poll::Pending` and such a queue can
    /// neither apply backpressure nor stay short; all it does is hold a burst of events and
    /// release it grouped by method, which loses the order the session delivered them in. A
    /// consumer that correlates events of different methods (a request's
    /// `Network.responseReceived` with its `Network.loadingFinished`, say) needs that order.
    pub fn start_send(&mut self, event: Arc<dyn Event>) {
        let _ = self.listener.unbounded_send(event);
    }

    /// Whether the receiving half of the subscription is gone.
    pub fn is_disconnected(&self) -> bool {
        self.listener.is_closed()
    }
}

impl fmt::Debug for EventListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventListener").finish()
    }
}

/// The receiver part of an event subscription
pub struct EventStream<T: IntoEventKind> {
    events: UnboundedReceiver<Arc<dyn Event>>,
    _marker: PhantomData<T>,
}

impl<T: IntoEventKind> fmt::Debug for EventStream<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventStream").finish()
    }
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

#[cfg(test)]
mod tests {
    use futures::{SinkExt, StreamExt};

    use chromiumoxide_cdp::cdp::CustomEvent;
    use chromiumoxide_cdp::cdp::browser_protocol::animation::{
        EventAnimationCanceled, EventAnimationCreated,
    };
    use chromiumoxide_types::MethodType;

    use super::*;

    #[tokio::test]
    async fn event_stream() {
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

    #[tokio::test]
    async fn custom_event_stream() {
        use serde::Deserialize;

        #[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
        struct MyCustomEvent {
            name: String,
        }

        impl MethodType for MyCustomEvent {
            fn method_id() -> MethodId {
                "Custom.Event".into()
            }
        }
        impl CustomEvent for MyCustomEvent {}

        let (mut tx, rx) = futures::channel::mpsc::unbounded();
        let mut stream = EventStream::<MyCustomEvent>::new(rx);

        let event = MyCustomEvent {
            name: "my event".to_string(),
        };
        let msg: Arc<dyn Event> = Arc::new(event.clone());
        tx.send(msg).await.unwrap();
        let next = stream.next().await.unwrap();
        assert_eq!(&*next, &event);
    }

    #[tokio::test]
    async fn event_listeners() {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        let mut listeners = EventListeners::default();

        let event = EventAnimationCanceled {
            id: "id".to_string(),
        };

        listeners.add_listener(EventListenerRequest {
            method: EventAnimationCanceled::method_id(),
            kind: EventAnimationCanceled::event_kind(),
            listener: tx,
        });

        listeners.start_send(event.clone());

        let mut stream = EventStream::<EventAnimationCanceled>::new(rx);

        tokio::spawn(async move {
            loop {
                std::future::poll_fn(|cx| {
                    listeners.poll(cx);
                    Poll::Pending
                })
                .await
            }
        });

        let next = stream.next().await.unwrap();
        assert_eq!(&*next, &event);
    }

    #[test]
    fn one_subscription_keeps_the_order_events_were_sent_in() {
        // Events of two methods reach one sender, so the order they were sent in is the order the
        // session delivered them in. A subscription set draws its own order over the methods once
        // and keeps it, so the question is how many of them draw a bad one, not how often one
        // misbehaves: a fresh set per attempt.
        for attempt in 0..200 {
            let (tx, mut rx) = futures::channel::mpsc::unbounded();
            let mut listeners = EventListeners::default();
            listeners.add_listener(EventListenerRequest::new::<EventAnimationCreated>(
                tx.clone(),
            ));
            listeners.add_listener(EventListenerRequest::new::<EventAnimationCanceled>(tx));

            listeners.start_send(EventAnimationCreated { id: "id".into() });
            listeners.start_send(EventAnimationCanceled { id: "id".into() });
            listeners.poll(&mut Context::from_waker(&futures::task::noop_waker()));

            let first = rx.try_recv().expect("an event was queued");
            assert!(
                first
                    .into_any_arc()
                    .downcast::<EventAnimationCreated>()
                    .is_ok(),
                "attempt {attempt}: an animation is created before it is cancelled"
            );
        }
    }
}
