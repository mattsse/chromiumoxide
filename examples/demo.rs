use chromiumoxid::browser::{Browser, BrowserConfig};
use chromiumoxid::cdp::browser_protocol;
use futures::StreamExt;

#[async_std::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (browser, mut fut) = Browser::launch(BrowserConfig::builder().build().unwrap()).await?;

    let handle = async_std::task::spawn(async move {
        loop {
            let res = fut.next().await.unwrap().unwrap();
            dbg!(res);
        }
    });

    let tab = browser.new_page("https://en.wikipedia.org").await?;

    tab.execute(browser_protocol::network::EnableParams::default())
        .await?;

    tab.goto("https://news.ycombinator.com/").await;

    // std::thread::sleep(std::time::Duration::from_millis(1000));

    // tab.get_document().await?;

    // dbg!(tab.find_element("input#searchInput").await?);

    handle.await;
    Ok(())
}
