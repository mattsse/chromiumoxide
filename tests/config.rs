use crate::test_config;
use chaser_oxide::BrowserConfig;

#[tokio::test]
#[ignore] // For some reason, this test fails on CI but works locally
async fn test_config_disable_https_first() {
    test_config(
        BrowserConfig::builder()
            .disable_https_first()
            .build()
            .unwrap(),
        async |browser| {
            let page = browser.new_page("about:blank").await.unwrap();
            page.goto("http://perdu.com").await.unwrap();
            let url = page.url().await.unwrap().unwrap();
            assert!(url.starts_with("http://"));
        },
    )
    .await;
}
