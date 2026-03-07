use std::time::Duration;

use chromiumoxide::{BrowserConfig, Page};
use chromiumoxide_cdp::cdp::browser_protocol::page::CreateIsolatedWorldParams;
use chromiumoxide_cdp::cdp::js_protocol::runtime::EvaluateParams;
use serde::Deserialize;
use tokio::time::sleep;

use crate::test_config;

#[derive(Debug, Deserialize)]
struct DetectionRow {
    #[serde(rename = "type")]
    kind: String,
    rating: f64,
    note: String,
}

#[tokio::test]
#[ignore] // Bot tests are flaky but a good reference
async fn test_bot_detection() {
    let config = BrowserConfig::builder().hide().build().unwrap(); //needed .hide()
    test_config(config, async |browser| {
        let page = browser.new_page("about:blank").await.unwrap();
        page.enable_stealth_mode().await.unwrap();

        page.goto("https://bot-detector.rebrowser.net/")
            .await
            .unwrap();
        test_dummyfn(&page).await;
        test_sourceurlleak(&page).await;
        test_runtimeenableleak(&page).await;
        test_exposefnleak(&page).await;
        test_mainworldexecution_isolated_world(&page).await;
        test_navigator_webdriver(&page).await;
        test_viewport(&page).await;
        test_initscripts(&page).await;
        test_csp(&page).await;
    })
    .await;
}

async fn test_csp(page: &Page) {
    let result = get_result(page, "bypassCsp").await.unwrap();
    assert!(result.rating < 0.0, "{}", result.note)
}
async fn test_initscripts(page: &Page) {
    let result = get_result(page, "pwInitScripts").await.unwrap();
    assert!(result.rating < 0.0, "{}", result.note)
}
async fn test_viewport(page: &Page) {
    let result = get_result(page, "viewport").await.unwrap();
    assert!(result.rating < 0.0, "{}", result.note)
}

// Can easily be broken (because extensions may override)
async fn test_navigator_webdriver(page: &Page) {
    let result = get_result(page, "navigatorWebdriver").await.unwrap();
    assert!(result.rating < 0.0, "{}", result.note)
}

// Has issue, can be skipped
async fn test_exposefnleak(page: &Page) {
    page.expose_function("exposedFn", "() => { console.log('exposedFn call') }")
        .await
        .unwrap();
    let result = get_result(page, "exposeFunctionLeak").await.unwrap();
    assert!(result.rating <= 0.0, "{}", result.note)
}

async fn test_runtimeenableleak(page: &Page) {
    let result = get_result(page, "runtimeEnableLeak").await.unwrap();
    assert!(result.rating < 0.0, "{}", result.note)
}

// can be skipped
async fn test_mainworldexecution_isolated_world(page: &Page) {
    eval_in_isolated_world(page, "document.getElementsByClassName('div')")
        .await
        .unwrap();
    let result = get_result(page, "mainWorldExecution").await.unwrap();
    assert!(result.rating <= 0.0, "{}", result.note)
}

async fn test_sourceurlleak(page: &Page) {
    page.evaluate("document.getElementById('detections-json')")
        .await
        .unwrap();
    let result = get_result(page, "sourceUrlLeak").await.unwrap();
    assert!(result.rating < 0.0, "{}", result.note)
}

async fn test_dummyfn(page: &Page) {
    page.evaluate("window.dummyFn()").await.unwrap();
    let result = get_result(page, "dummyFn").await.unwrap();
    assert!(result.rating < 0.0, "{}", result.note)
}

async fn eval_in_isolated_world(page: &Page, script: &str) -> chromiumoxide::error::Result<()> {
    let frame_id = page
        .mainframe()
        .await?
        .ok_or(chromiumoxide::error::CdpError::NotFound)?;

    let isolated_world = page
        .execute(
            CreateIsolatedWorldParams::builder()
                .frame_id(frame_id)
                .world_name("rebrowser_test_world")
                .grant_univeral_access(true)
                .build()
                .unwrap(),
        )
        .await?;

    page.execute(
        EvaluateParams::builder()
            .expression(script)
            .context_id(isolated_world.result.execution_context_id)
            .await_promise(true)
            .return_by_value(true)
            .build()
            .unwrap(),
    )
    .await?;

    Ok(())
}

async fn get_result(page: &Page, target_kind: &str) -> Option<DetectionRow> {
    let timeout_secs = 15;
    let interval = Duration::from_millis(500);
    let mut elapsed = Duration::from_secs(0);

    loop {
        let script = "document.querySelector('#detections-json') ? document.querySelector('#detections-json').value : ''";
        if let Ok(Some(js_value)) = page
            .evaluate(script)
            .await
            .map(|v| v.into_value::<String>().ok())
        {
            if !js_value.trim().is_empty() && js_value.starts_with('[') {
                if let Ok(list) = serde_json::from_str::<Vec<DetectionRow>>(&js_value) {
                    if let Some(found) = list.into_iter().find(|p| p.kind == target_kind) {
                        return Some(found);
                    }
                }
            }
        }

        if elapsed >= Duration::from_secs(timeout_secs) {
            println!("DEBUG: {} timed out!", target_kind);
            return None;
        }

        sleep(interval).await;
        elapsed += interval;
    }
}
