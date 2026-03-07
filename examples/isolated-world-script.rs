use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide_cdp::cdp::browser_protocol::page::CreateIsolatedWorldParams;
use chromiumoxide_cdp::cdp::js_protocol::runtime::EvaluateParams;
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (mut browser, mut handler) = Browser::launch(BrowserConfig::builder().build()?).await?;

    let handle = tokio::spawn(async move {
        while let Some(h) = handler.next().await {
            if h.is_err() {
                break;
            }
        }
    });

    let page = browser.new_page("about:blank").await?;

    page.goto("https://example.com").await?;

    let frame_id = page.mainframe().await?.ok_or("No main frame available")?;

    // Create isolated world and get execution context id directly from response.
    let isolated_world = page
        .execute(
            CreateIsolatedWorldParams::builder()
                .frame_id(frame_id)
                .world_name("example_isolated_world")
                .grant_univeral_access(true)
                .build()
                .unwrap(),
        )
        .await?;

    let ctx_id = isolated_world.result.execution_context_id;

    // Evaluate inside isolated world by passing context_id.
    let res = page
        .execute(
            EvaluateParams::builder()
                .expression("({ title: document.title, href: location.href })")
                .context_id(ctx_id)
                .return_by_value(true)
                .await_promise(true)
                .build()
                .unwrap(),
        )
        .await?;

    println!("isolated world result: {:?}", res.result.result.value);

    browser.close().await?;
    handle.await?;
    Ok(())
}
