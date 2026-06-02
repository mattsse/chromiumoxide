use chromiumoxide::BrowserConfig;

use crate::{test, test_config};

#[tokio::test]
async fn test_basic() {
    test(async |browser| {
        let page = browser.new_page("about:blank").await.unwrap();
        page.goto("https://www.google.com").await.unwrap();
        let title = page.get_title().await.unwrap().unwrap();
        assert!(title.contains("Google"));
    })
    .await;
}

#[tokio::test]
async fn test_basic_pipes() {
    test_config(
        BrowserConfig::builder().pipes().build().unwrap(),
        async |browser| {
            let page = browser.new_page("about:blank").await.unwrap();
            page.goto("https://www.google.com").await.unwrap();
            let title = page.get_title().await.unwrap().unwrap();
            assert!(title.contains("Google"));
        },
    )
    .await;
}
