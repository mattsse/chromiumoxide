use std::path::PathBuf;

use super::{BrowserFetcherOptions, BrowserFetcherRevisionInfo, BrowserFetcherRuntime};
use crate::error::{FetcherError, Result};
use crate::{Platform, Revision};

pub struct BrowserFetcher {
    revision: Revision,
    host: String,
    path: PathBuf,
    platform: Platform,
}

impl BrowserFetcher {
    pub fn new(options: BrowserFetcherOptions) -> Self {
        Self {
            revision: options.revision,
            host: options.host,
            path: options.path,
            platform: options.platform,
        }
    }

    pub async fn fetch(&self) -> Result<BrowserFetcherRevisionInfo> {
        if !self.local().await {
            self.download().await?;
        }

        Ok(self.revision_info())
    }

    async fn local(&self) -> bool {
        let folder_path = self.folder_path();
        BrowserFetcherRuntime::exists(&folder_path).await
    }

    async fn download(&self) -> Result<()> {
        let url = self.platform.download_url(&self.host, &self.revision);
        let folder_path = self.folder_path();
        let archive_path = folder_path.with_extension("zip");

        BrowserFetcherRuntime::download(&url, &archive_path)
            .await
            .map_err(FetcherError::DownloadFailed)?;
        BrowserFetcherRuntime::unzip(archive_path, folder_path)
            .await
            .map_err(FetcherError::InstallFailed)?;

        Ok(())
    }

    fn folder_path(&self) -> PathBuf {
        let mut folder_path = self.path.clone();
        folder_path.push(self.platform.folder_name(&self.revision));
        folder_path
    }

    fn revision_info(&self) -> BrowserFetcherRevisionInfo {
        let folder_path = self.folder_path();
        let executable_path = self.platform.executable(&folder_path, &self.revision);
        BrowserFetcherRevisionInfo {
            folder_path,
            executable_path,
            revision: self.revision.clone(),
        }
    }
}
