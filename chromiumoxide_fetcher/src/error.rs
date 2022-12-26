use std::num::ParseIntError;

use thiserror::Error;

pub type Result<T, E = FetcherError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum FetcherError {
    #[error("Invalid browser revision")]
    InvalidRevision(#[source] ParseIntError),

    #[error("No path available to download browsers to")]
    NoPathAvailable,

    #[error("OS {0} {1} is not supported")]
    UnsupportedOs(&'static str, &'static str),
}
