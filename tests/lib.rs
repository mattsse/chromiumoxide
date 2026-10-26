use std::panic;

use chromiumoxide::{Browser, BrowserConfig};
use futures::{FutureExt, StreamExt};

mod basic;
mod config;
mod page;
mod stealth;
mod target_info_changed;

pub async fn test<T>(test: T)
where
    T: for<'a> AsyncFnOnce(&'a mut Browser),
{
    test_config(BrowserConfig::builder().build().unwrap(), test).await;
}

pub async fn test_config<T>(config: BrowserConfig, test: T)
where
    T: for<'a> AsyncFnOnce(&'a mut Browser),
{
    let (mut browser, mut handler) = Browser::launch(config).await.unwrap();

    let handle = tokio::spawn(async move {
        while let Some(h) = handler.next().await {
            match h {
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    });

    let browser_ref = &mut browser;
    let result = async move {
        panic::AssertUnwindSafe(test(browser_ref))
            .catch_unwind()
            .await
    }
    .await;

    browser.close().await.unwrap();
    handle.await.unwrap();

    assert!(result.is_ok())
}
