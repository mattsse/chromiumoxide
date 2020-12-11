use chromiumoxid::browser::{Browser, BrowserConfig};

use futures::StreamExt;

#[async_std::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    pretty_env_logger::init();

    let (browser, mut fut) =
        // Browser::connect(std::env::var("CDP_URL").unwrap()).await?;
        Browser::launch(BrowserConfig::builder().with_head().build().unwrap()).await?;

    let handle = async_std::task::spawn(async move {
        loop {
            let _res = fut.next().await.unwrap().unwrap();
        }
    });
    let page = browser.new_page("https://en.wikipedia.org").await?;
    println!("PAGE:    {:?}", page);
    let frame = page.goto("https://news.ycombinator.com/").await?;

    // std::thread::sleep(std::time::Duration::from_secs(5));
    println!("current document {:?}", page.get_document().await);

    let frame = page.goto("https://www.reddit.com/r/rust/").await?;

    dbg!(page);
    // tab.execute(browser_protocol::network::EnableParams::default())
    //     .await?;
    //
    // tab.goto("https://news.ycombinator.com/").await;

    // std::thread::sleep(std::time::Duration::from_millis(1000));

    // tab.get_document().await?;

    // dbg!(tab.find_element("input#searchInput").await?);

    handle.await;
    Ok(())
}
