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

    let sum: usize = page.evaluate("1 + 2").await?.into_value()?;
    assert_eq!(sum, 3);

    handle.await;
    Ok(())
}
