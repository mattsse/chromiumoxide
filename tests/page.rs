use std::collections::HashSet;
use std::time::Duration;

use chromiumoxide::cdp::browser_protocol::network::{EventLoadingFinished, EventResponseReceived};
use chromiumoxide::listeners::EventListenerRequest;
use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use tokio::time::timeout;

use crate::test;

#[tokio::test]
async fn test_evaluate_on_new_document() {
    test(async |browser| {
        let page = browser
            .new_page("about:blank")
            .await
            .expect("should create new page");

        page.evaluate_on_new_document("window.testValue = 42;")
            .await
            .expect("should evaluate script on new document");

        page.goto("https://www.google.com")
            .await
            .expect("should navigate to www.google.com");

        let result: i32 = page
            .evaluate("window.testValue")
            .await
            .expect("should evaluate window.testValue")
            .into_value()
            .expect("should convert to i32");

        assert_eq!(result, 42);
    })
    .await;
}

#[tokio::test]
async fn test_add_init_script() {
    test(async |browser| {
        let page = browser
            .new_page("about:blank")
            .await
            .expect("should create new page");

        page.add_init_script("window.testValue = 42;")
            .await
            .expect("should add init script");

        page.goto("https://www.google.com")
            .await
            .expect("should navigate to www.google.com");

        let result: i32 = page
            .evaluate("window.testValue")
            .await
            .expect("should evaluate window.testValue")
            .into_value()
            .expect("should convert to i32");

        assert_eq!(result, 42);
    })
    .await;
}

#[tokio::test]
async fn test_add_event_listener_receives_methods_in_order() {
    test(async |browser| {
        let page = browser
            .new_page("about:blank")
            .await
            .expect("should create new page");

        // One channel for both methods, so what arrives on it is ordered. Nothing holds the sender
        // afterwards but the subscriptions themselves, so the stream ends when the page is gone.
        let (tx, mut events) = unbounded();
        for subscription in [
            EventListenerRequest::new::<EventResponseReceived>(tx.clone()),
            EventListenerRequest::new::<EventLoadingFinished>(tx.clone()),
        ] {
            page.add_event_listener(subscription)
                .await
                .expect("should add event listener");
        }
        drop(tx);

        page.goto("https://www.google.com")
            .await
            .expect("should navigate to www.google.com");

        // A request's head is received before its loading finishes, so every id that finishes must
        // already have been seen receiving. Draining stops once the page has gone quiet.
        let mut received = HashSet::new();
        let mut finished = 0usize;
        while let Ok(Some(event)) = timeout(Duration::from_secs(2), events.next()).await {
            let event = event.into_any_arc();
            let event = match event.downcast::<EventResponseReceived>() {
                Ok(response) => {
                    received.insert(response.request_id.inner().clone());
                    continue;
                }
                Err(event) => event,
            };
            if let Ok(done) = event.downcast::<EventLoadingFinished>() {
                let id = done.request_id.inner();
                assert!(
                    received.contains(id),
                    "request {id} finished loading before its head was received"
                );
                finished += 1;
            }
        }

        assert!(finished > 0, "no request finished loading");
    })
    .await;
}
