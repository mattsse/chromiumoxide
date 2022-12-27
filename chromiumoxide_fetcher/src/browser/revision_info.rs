use std::path::PathBuf;

use crate::Revision;

#[derive(Clone, Debug)]
pub struct BrowserFetcherRevisionInfo {
    pub folder_path: PathBuf,
    pub executable_path: PathBuf,
    pub revision: Revision,
}
