use chromeoxid::browser::Browser;
use futures::StreamExt;

#[async_std::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = "ws://127.0.0.1:55589/devtools/browser/6e583b57-28a9-42a6-8e24-d66fba83a677";

    let (browser, mut fut) = Browser::connect(url).await?;

    println!("here");

    let handle = async_std::task::spawn(async move {
        loop {
            let _res = fut.next().await;
            // dbg!(res);
        }
    });

    let _tab = browser.new_tab("https://news.ycombinator.com/").await?;

    handle.await;

    Ok(())
}
