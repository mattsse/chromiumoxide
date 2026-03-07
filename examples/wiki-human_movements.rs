use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::{ClickOptions, MovementBehavior};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let (browser, mut handler) =
        Browser::launch(BrowserConfig::builder().with_head().build()?).await?;

    let handle = tokio::spawn(async move {
        loop {
            let _ = handler.next().await.unwrap();
        }
    });

    let page = browser.new_page("about:blank").await?;
    // Add mouse dot injection script to see human-like mouse movements
    page.evaluate_on_new_document(
        r#"
            (function () {
                if (window.__mouseDotInjected) return;
                window.__mouseDotInjected = true;

                function inject() {
                    if (!document.body) {
                        return setTimeout(inject, 50);
                    }

                    const dot = document.createElement("div");
                    dot.id = "rust-mouse-dot";
                    dot.style.position = "fixed";
                    dot.style.width = "12px";
                    dot.style.height = "12px";
                    dot.style.borderRadius = "50%";
                    dot.style.background = "red";
                    dot.style.pointerEvents = "none";
                    dot.style.zIndex = "999999";
                    dot.style.transform = "translate(-50%, -50%)";

                    document.body.appendChild(dot);

                    document.addEventListener("mousemove", (e) => {
                        dot.style.left = e.clientX + "px";
                        dot.style.top = e.clientY + "px";
                    });
                }

                inject();
            })();
        "#,
    )
    .await?;
    page.goto("https://en.wikipedia.org").await?;
    let human_click = ClickOptions::builder()
        .movement_behavior(Some(MovementBehavior::BezierPath))
        .build();

    page.find_element(".search-toggle")
        .await?
        .click_with(human_click.clone())
        .await?;

    let search_input = page.find_element("input[name='search']").await?;
    search_input
        .click_with(human_click)
        .await?
        .type_str("Rust programming language")
        .await?
        .press_key("Enter")
        .await?;

    let _html = page.wait_for_navigation().await?.content().await?;

    handle.await?;
    Ok(())
}
