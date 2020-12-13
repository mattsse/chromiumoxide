use futures::StreamExt;

use chromiumoxide::browser::{Browser, BrowserConfig};

#[async_std::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    pretty_env_logger::init();

    let (browser, mut handler) = Browser::launch(BrowserConfig::builder().build()?).await?;

    let handle = async_std::task::spawn(async move {
        loop {
            let _event = handler.next().await.unwrap();
        }
    });

    let page = browser.new_page("https://en.wikipedia.org").await?;

    page.find_element("input#searchInput")
        .await?
        .click()
        .await?
        .type_str("Rust programming language")
        .await?
        .press_key("Enter")
        .await?;

    let path = page
        .wait_for_navigation()
        .await?
        .find_element("li.mw-search-result a")
        .await?
        .attribute("href")
        .await?
        .unwrap();

    let _html = page
        .goto(format!("https://en.wikipedia.org{}", path))
        .await?
        .content()
        .await?;

    handle.await;
    Ok(())
}
