//! Managed Fetch request interception with `PausedRequest` / `FulfillResponse`.
//!
//! This example navigates to an external website (`TARGET`), so it does not run
//! offline; edit `TARGET` to point at a local server to run it without network
//! access.

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::{Binary, FulfillResponse};
use futures::StreamExt;

const CONTENT: &str = "<html><head></head><body><h1>TEST</h1></body></html>";
const TARGET: &str = "https://news.ycombinator.com/";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let (mut browser, mut handler) =
        Browser::launch(BrowserConfig::builder().disable_cache().build()?).await?;
    let browser_handle = tokio::spawn(async move {
        while let Some(result) = handler.next().await {
            if result.is_err() {
                break;
            }
        }
    });

    let page = browser.new_page("about:blank").await?;
    // Register the responder before enabling Fetch so the first navigation
    // cannot pause before a consumer exists.
    let mut paused_requests = page.paused_requests().await?;
    page.set_request_interception(true).await?;

    let intercept_handle = tokio::spawn(async move {
        while let Some(paused) = paused_requests.next().await {
            let result = if paused.event().request.url == TARGET {
                let response = FulfillResponse::builder(200)
                    .body(Binary::from(BASE64_STANDARD.encode(CONTENT)))
                    .build()
                    .expect("the synthetic response is valid");
                paused.fulfill(response).await
            } else {
                paused.continue_request().await
            };
            if let Err(error) = result {
                eprintln!("Failed to resolve an intercepted request: {error}");
            }
        }
    });

    page.goto(TARGET).await?;
    if page.content().await?.contains("<h1>TEST</h1>") {
        println!("Content overridden");
    }

    page.goto("https://google.com").await?;
    if !page.content().await?.contains("<h1>TEST</h1>") {
        println!("Other content was continued normally");
    }

    browser.close().await?;
    browser_handle.await?;
    intercept_handle.await?;
    Ok(())
}
