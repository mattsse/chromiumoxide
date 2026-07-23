//! Block navigations to specific hosts using `PausedRequest` as the response
//! capability, correlating with `requestWillBeSent`.
//!
//! This example navigates to external websites, so it does not run offline;
//! edit the target URLs to point at a local server to run it without network
//! access.

use std::collections::HashMap;
use std::time::Duration;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::{
    ErrorReason, EventRequestWillBeSent, RequestId, ResourceType,
};
use chromiumoxide::{Binary, FulfillResponse, PausedRequest};
use futures::{StreamExt, select};

const CONTENT: &str = "<html><head><meta http-equiv=\"refresh\" content=\"0;URL='http://www.example.com/'\" /></head><body><h1>TEST</h1></body></html>";
const TARGET: &str = "http://google.com/";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let (mut browser, mut handler) = Browser::launch(
        BrowserConfig::builder()
            .disable_cache()
            .request_timeout(Duration::from_secs(5))
            .build()?,
    )
    .await?;
    let browser_handle = tokio::spawn(async move {
        while let Some(result) = handler.next().await {
            if result.is_err() {
                break;
            }
        }
    });

    let page = browser.new_page("about:blank").await?;
    let mut paused_requests = page.paused_requests().await?.fuse();
    let mut request_will_be_sent = page
        .event_listener::<EventRequestWillBeSent>()
        .await?
        .fuse();
    page.set_request_interception(true).await?;

    let intercept_handle = tokio::spawn(async move {
        let mut resolutions = HashMap::<RequestId, InterceptResolution>::new();
        loop {
            select! {
                paused = paused_requests.next() => {
                    let Some(paused) = paused else {
                        continue;
                    };
                    if paused.event().response_status_code.is_some() {
                        if let Err(error) = paused.continue_request().await {
                            eprintln!("Failed to continue a response-stage pause: {error}");
                        }
                        continue;
                    }

                    let Some(network_id) = paused.event().network_id.clone() else {
                        if let Err(error) = paused.continue_request().await {
                            eprintln!("Failed to continue an uncorrelated request: {error}");
                        }
                        continue;
                    };
                    let target = paused.event().request.url == TARGET;
                    let resolution = resolutions.entry(network_id.clone()).or_default();
                    resolution.paused = Some(paused);
                    if target {
                        resolution.action = InterceptAction::Fulfill;
                    }
                    resolve(&network_id, &mut resolutions).await;
                },
                sent = request_will_be_sent.next() => {
                    let Some(sent) = sent else {
                        continue;
                    };
                    let action = if sent.request.url == TARGET {
                        InterceptAction::Fulfill
                    } else if is_navigation(&sent) {
                        InterceptAction::Abort
                    } else {
                        InterceptAction::Forward
                    };
                    resolutions.entry(sent.request_id.clone()).or_default().action = action;
                    resolve(&sent.request_id, &mut resolutions).await;
                },
                complete => break,
            }
        }
    });

    page.goto(TARGET).await?;
    println!("Content: {}", page.content().await?);

    browser.close().await?;
    browser_handle.await?;
    intercept_handle.await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
enum InterceptAction {
    Forward,
    Abort,
    Fulfill,
    #[default]
    None,
}

#[derive(Debug, Default)]
struct InterceptResolution {
    action: InterceptAction,
    paused: Option<PausedRequest>,
}

fn is_navigation(event: &EventRequestWillBeSent) -> bool {
    event.request_id.inner() == event.loader_id.inner()
        && event.r#type.as_ref() == Some(&ResourceType::Document)
}

async fn resolve(
    network_id: &RequestId,
    resolutions: &mut HashMap<RequestId, InterceptResolution>,
) {
    let ready = resolutions.get(network_id).is_some_and(|resolution| {
        resolution.paused.is_some() && resolution.action != InterceptAction::None
    });
    if !ready {
        return;
    }

    let resolution = resolutions
        .remove(network_id)
        .expect("a ready resolution remains registered");
    let paused = resolution
        .paused
        .expect("a ready resolution owns its response capability");
    let result = match resolution.action {
        InterceptAction::Forward => paused.continue_request().await,
        InterceptAction::Abort => paused.fail(ErrorReason::Aborted).await,
        InterceptAction::Fulfill => {
            let response = FulfillResponse::builder(200)
                .body(Binary::from(BASE64_STANDARD.encode(CONTENT)))
                .build()
                .expect("the synthetic response is valid");
            paused.fulfill(response).await
        }
        InterceptAction::None => unreachable!("readiness excludes an empty action"),
    };
    if let Err(error) = result {
        eprintln!("Failed to resolve request {network_id:?}: {error}");
    }
}
