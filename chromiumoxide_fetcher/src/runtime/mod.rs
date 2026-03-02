use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use self::zip::ZipArchive;

mod zip;

#[derive(Debug, Default)]
pub struct Runtime;

impl Runtime {
    pub async fn exists(folder_path: &Path) -> bool {
        tokio::fs::metadata(folder_path).await.is_ok()
    }

    pub async fn download_json<T>(url: &str) -> anyhow::Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let url = url
            .parse::<reqwest::Url>()
            .context("Invalid metadata url")?;
        let res = request_get(url)
            .await
            .context("Failed to send request to host")?;
        if res.status() != reqwest::StatusCode::OK {
            anyhow::bail!("Invalid metadata url");
        }
        let body = res
            .json::<T>()
            .await
            .context("Failed to read response body")?;
        Ok(body)
    }

    pub async fn download_text(url: &str) -> anyhow::Result<String> {
        let url = url
            .parse::<reqwest::Url>()
            .context("Invalid metadata url")?;
        let res = request_get(url)
            .await
            .context("Failed to send request to host")?;
        if res.status() != reqwest::StatusCode::OK {
            anyhow::bail!("Invalid metadata url");
        }
        let body = res.text().await.context("Failed to read response body")?;
        Ok(body)
    }

    pub async fn download_file(url: &str, archive_path: &Path) -> anyhow::Result<()> {
        // Open file
        let file = tokio::fs::File::create(&archive_path)
            .await
            .context("Failed to create archive file")?;
        let mut file = tokio::io::BufWriter::new(file);

        // Download
        let url = url.parse::<reqwest::Url>().context("Invalid archive url")?;
        let mut res = request_get(url)
            .await
            .context("Failed to send request to host")?;
        if res.status() != reqwest::StatusCode::OK {
            anyhow::bail!("Invalid archive url");
        }
        while let Some(chunk) = res.chunk().await.context("Failed to read response chunk")? {
            file.write(&chunk)
                .await
                .context("Failed to write to archive file")?;
        }

        // Flush to disk
        file.flush().await.context("Failed to flush to disk")?;

        Ok(())
    }

    pub async fn unzip(archive_path: PathBuf, folder_path: PathBuf) -> anyhow::Result<()> {
        tokio::task::spawn_blocking(move || do_unzip(&archive_path, &folder_path)).await?
    }
}

async fn request_get(url: reqwest::Url) -> anyhow::Result<reqwest::Response> {
    let builder = reqwest::Client::builder();
    #[cfg(feature = "native-tls")]
    let builder = builder.use_native_tls();
    #[cfg(all(not(feature = "native-tls"), feature = "rustls"))]
    let builder = builder.use_rustls_tls();
    let client = builder.build().context("Failed to build HTTP client")?;
    client.get(url).send().await.context("Request failed")
}

fn do_unzip(archive_path: &Path, folder_path: &Path) -> anyhow::Result<()> {
    use std::fs;

    // Prepare
    fs::create_dir_all(folder_path).context("Failed to create folder")?;
    let file = fs::File::open(archive_path).context("Failed to open archive")?;

    // Unzip
    let mut archive = ZipArchive::new(file).context("Failed to unzip archive")?;
    archive.extract(folder_path)?;

    // Clean (if possible)
    let _ = fs::remove_file(archive_path);
    Ok(())
}
