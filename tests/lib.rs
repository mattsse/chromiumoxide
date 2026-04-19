use std::panic;
use std::sync::OnceLock;

use chaser_oxide::{Browser, BrowserConfig};
use futures::{FutureExt, StreamExt};
use tokio::sync::Semaphore;

mod basic;
mod config;
mod page;

static BROWSER_SEMAPHORE: OnceLock<Semaphore> = OnceLock::new();

fn browser_semaphore() -> &'static Semaphore {
    BROWSER_SEMAPHORE.get_or_init(|| Semaphore::new(1))
}

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
    let _permit = browser_semaphore().acquire().await.unwrap();
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
