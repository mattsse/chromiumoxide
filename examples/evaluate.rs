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
    println!("1 + 2 = {}", sum);

    let mult: usize = page
        .evaluate("() => { return 21 * 2; }")
        .await?
        .into_value()?;
    assert_eq!(mult, 42);
    println!("21 * 2 = {}", mult);

    let promise_div: usize = page
        .evaluate("() => Promise.resolve(100 / 25)")
        .await?
        .into_value()?;
    assert_eq!(promise_div, 4);
    println!("100 / 25 = {}", promise_div);

    handle.await;
    Ok(())
}
