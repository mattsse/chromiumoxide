use futures::StreamExt;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide_cdp::cdp::browser_protocol::page::{
    CaptureScreenshotFormat, CaptureScreenshotParams,
};

#[async_std::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (browser, mut handler) = Browser::launch(BrowserConfig::builder().build()?).await?;

    let handle = async_std::task::spawn(async move {
        loop {
            let _ = handler.next().await.unwrap();
        }
    });

    let page = browser.new_page("https://news.ycombinator.com/").await?;

    // take a screenshot of the page
    page.save_screenshot(
        CaptureScreenshotParams::builder()
            .format(CaptureScreenshotFormat::Png)
            .build(),
        "hn-page.png",
    )
    .await?;

    // get the top post and save a screenshot of it
    page.find_element("table.itemlist tr")
        .await?
        .save_screenshot(CaptureScreenshotFormat::Png, "top-post.jpg")
        .await?;

    handle.await;
    Ok(())
}
