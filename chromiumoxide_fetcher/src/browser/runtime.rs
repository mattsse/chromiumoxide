use std::path::{Path, PathBuf};

use super::ZipArchive;

pub struct BrowserFetcherRuntime;

#[cfg(feature = "async-std-runtime")]
impl BrowserFetcherRuntime {
    pub async fn exists(folder_path: &Path) -> bool {
        async_std::fs::metadata(folder_path).await.is_ok()
    }

    pub async fn download(url: &str, archive_path: &Path) -> anyhow::Result<()> {
        use async_std::io::WriteExt;

        // Open file
        let file = async_std::fs::File::create(&archive_path).await?;
        let mut file = async_std::io::BufWriter::new(file);

        // Download
        let res = surf::get(url).await.map_err(|e| e.into_inner())?;
        async_std::io::copy(res, &mut file).await?;

        // Flush to disk
        file.flush().await?;
        Ok(())
    }

    pub async fn unzip(archive_path: PathBuf, folder_path: PathBuf) -> anyhow::Result<()> {
        async_std::task::spawn_blocking(move || do_unzip(&archive_path, &folder_path)).await?;
        Ok(())
    }
}

#[cfg(feature = "tokio-runtime")]
impl BrowserFetcherRuntime {
    pub async fn exists(folder_path: &Path) -> bool {
        tokio::fs::metadata(folder_path).await.is_ok()
    }

    pub async fn download(url: &str, archive_path: &Path) -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt;

        // Open file
        let file = tokio::fs::File::create(&archive_path).await?;
        let mut file = tokio::io::BufWriter::new(file);

        // Download
        let mut response = reqwest::get(url).await?;
        while let Some(chunk) = response.chunk().await? {
            file.write(&chunk).await?;
        }

        // Flush to disk
        file.flush().await?;

        Ok(())
    }

    pub async fn unzip(archive_path: PathBuf, folder_path: PathBuf) -> anyhow::Result<()> {
        tokio::task::spawn_blocking(move || do_unzip(&archive_path, &folder_path)).await?
    }
}

fn do_unzip(archive_path: &Path, folder_path: &Path) -> anyhow::Result<()> {
    use std::fs;

    // Prepare
    fs::create_dir_all(folder_path)?;
    let file = fs::File::open(archive_path)?;

    // Unzip
    let mut archive = ZipArchive::new(file)?;
    archive.extract(folder_path)?;

    // Clean (if possible)
    let _ = fs::remove_file(archive_path);
    Ok(())
}
