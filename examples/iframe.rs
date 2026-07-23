//! Enumerate a page's frames and evaluate JavaScript inside a cross-origin
//! (out-of-process) iframe.
//!
//! By default this visits an external website for a manual sanity check, so it
//! does not run offline; pass a URL that embeds a cross-origin iframe as the
//! first argument to run it against your own page. Deterministic, offline OOPIF
//! coverage lives in the local fixtures under `tests/oopif.rs`.

use chromiumoxide::browser::{Browser, BrowserConfig};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Default is an external site purely for a manual sanity check; pass any
    // URL that embeds a cross-origin iframe as the first argument. Deterministic
    // OOPIF coverage lives in the local fixtures under `tests/oopif.rs`.
    let target = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://iframetester.com/?url=https%3A%2F%2Fexample.com".to_owned());
    // `--site-per-process` forces cross-origin iframes out of process, and
    // HTTPS-First is disabled to match the OOPIF test harness so Chrome 149 does
    // not upgrade/interstitial the navigations under test.
    let (mut browser, mut handler) = Browser::launch(
        BrowserConfig::builder()
            .arg("--site-per-process")
            .disable_https_first()
            .build()?,
    )
    .await?;
    let handler_task = tokio::spawn(async move {
        while let Some(result) = handler.next().await {
            if result.is_err() {
                break;
            }
        }
    });

    let page = browser.new_page(target).await?;
    println!("main session: {}", page.session_id().as_ref());
    for frame in page.all_frames().await? {
        println!(
            "frame={} session={} oop={} url={}",
            frame.id().as_ref(),
            frame.session_id().as_ref(),
            frame.is_out_of_process(),
            frame.url().as_deref().unwrap_or("<unknown>")
        );
        if frame.is_out_of_process() {
            let title: String = frame
                .eval("document.title")
                .await?
                .into_value()
                .map_err(|error| format!("frame title was not a string: {error}"))?;
            println!("  child document title: {title:?}");
        }
    }

    browser.close().await?;
    handler_task.await?;
    Ok(())
}
