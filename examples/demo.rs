use chromeoxid::browser::Browser;
use futures::StreamExt;

#[async_std::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = "ws://127.0.0.1:53114/devtools/browser/c0606c14-d0ae-4285-90cd-932bbf38bae7";

    let (browser, mut fut) = Browser::connect(url).await?;

    println!("here");

    let handle = async_std::task::spawn(async move {
        loop {
            let res = fut.next().await.unwrap().unwrap();
            dbg!(res);
        }
    });

    let tab = browser.new_tab("about:blank").await?;
    std::thread::sleep(std::time::Duration::from_secs(5));
    let doc = tab.get_document().await?;
    dbg!(doc);
    println!("here3");
    handle.await;

    Ok(())
}
