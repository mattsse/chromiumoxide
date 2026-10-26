//! A target that is announced as something else and becomes a page later is
//! attached to — what CEF (Chromium Embedded Framework) does with every
//! browser it creates: `Target.targetCreated` with type `other`, then
//! `Target.targetInfoChanged` with type `page`.
//!
//! No browser needed: the handler talks to a fake DevTools endpoint.

use std::time::Duration;

use async_tungstenite::tungstenite::Message;
use chromiumoxide::Browser;
use futures::StreamExt;
use serde_json::{Value, json};

/// A CDP event as the browser sends it.
fn event(method: &str, target_info: Value) -> Message {
    Message::text(json!({ "method": method, "params": { "targetInfo": target_info } }).to_string())
}

/// The target's info with the given type and URL.
fn target(kind: &str, url: &str) -> Value {
    json!({
        "targetId": "CEF-BROWSER-1",
        "type": kind,
        "title": "",
        "url": url,
        "attached": false,
        "canAccessOpener": false,
    })
}

#[tokio::test]
async fn a_target_that_becomes_a_page_is_attached() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // The fake browser: answers every command with an empty result, announces
    // the target once discovery is switched on, and hands back the parameters
    // of the first `Target.attachToTarget` it is sent.
    let browser_side = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = async_tungstenite::tokio::accept_async(tcp).await.unwrap();
        while let Some(msg) = ws.next().await {
            let Ok(Message::Text(text)) = msg else {
                continue;
            };
            let call: Value = serde_json::from_str(&text).unwrap();
            let method = call["method"].as_str().unwrap_or_default().to_owned();
            if method == "Target.attachToTarget" {
                return call["params"].clone();
            }
            let answer = json!({ "id": call["id"], "result": {} }).to_string();
            ws.send(Message::text(answer)).await.unwrap();
            if method == "Target.setDiscoverTargets" {
                ws.send(event("Target.targetCreated", target("other", "")))
                    .await
                    .unwrap();
                ws.send(event(
                    "Target.targetInfoChanged",
                    target("page", "about:blank"),
                ))
                .await
                .unwrap();
            }
        }
        panic!("the connection ended without a Target.attachToTarget");
    });

    let (_browser, mut handler) = Browser::connect(format!("ws://{addr}")).await.unwrap();
    let handler = tokio::spawn(async move { while handler.next().await.is_some() {} });

    let attach = tokio::time::timeout(Duration::from_secs(5), browser_side)
        .await
        .expect("the target that became a page was never attached to")
        .unwrap();
    assert_eq!(attach["targetId"], "CEF-BROWSER-1");
    assert_eq!(attach["flatten"], true);
    handler.abort();
}
