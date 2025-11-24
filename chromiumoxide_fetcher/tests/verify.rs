use chromiumoxide_fetcher::{BrowserFetcherOptions, Platform, Revision, CURRENT_REVISION};
use reqwest::{IntoUrl, Response, StatusCode};

pub async fn head<T: IntoUrl>(url: T) -> reqwest::Result<Response> {
    reqwest::Client::builder().build()?.head(url).send().await
}

// Check if the chosen revision has a build available for all platforms.
// That not always the case, that is why we need to make sure of it.
#[tokio::test]
async fn verify_revision_available() {
    for platform in &[
        Platform::Linux,
        Platform::Mac,
        Platform::MacArm,
        Platform::Win32,
        Platform::Win64,
    ] {
        let res = head(&platform.download_url("https://storage.googleapis.com", &CURRENT_REVISION))
            .await
            .unwrap();

        if res.status() != StatusCode::OK {
            panic!(
                "Revision {} is not available for {:?}",
                CURRENT_REVISION, platform
            );
        }
    }
}

#[ignore]
#[tokio::test]
async fn find_revision_available() {
    let min = 1355000; // Enter the minimum revision
    let max = 1356013; // Enter the maximum revision

    'outer: for revision in (min..max).rev() {
        println!("Checking revision {}", revision);

        for platform in &[
            Platform::Linux,
            Platform::Mac,
            Platform::MacArm,
            Platform::Win32,
            Platform::Win64,
        ] {
            let res = head(
                &platform.download_url("https://storage.googleapis.com", &Revision::from(revision)),
            )
            .await
            .unwrap();

            if res.status() != StatusCode::OK {
                println!("Revision {} is not available for {:?}", revision, platform);
                continue 'outer;
            }
        }

        println!("Found revision {}", revision);
        break;
    }
}

#[ignore]
#[tokio::test]
async fn download_revision() {
    let path = "./.cache";

    tokio::fs::create_dir(path).await.unwrap();

    for platform in &[
        Platform::Linux,
        Platform::Mac,
        Platform::MacArm,
        Platform::Win32,
        Platform::Win64,
    ] {
        let revision = chromiumoxide_fetcher::BrowserFetcher::new(
            BrowserFetcherOptions::builder()
                .with_revision(CURRENT_REVISION)
                .with_path(path)
                .with_platform(*platform)
                .build()
                .unwrap(),
        )
        .fetch()
        .await
        .unwrap();

        println!("Downloaded revision {} for {:?}", revision, platform);
    }
}
