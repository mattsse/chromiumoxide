pub use self::browser::{BrowserFetcher, BrowserFetcherOptions, BrowserFetcherRevisionInfo};
pub use self::error::FetcherError;
pub use self::platform::Platform;
pub use self::revision::Revision;

/// Currently used chromium revision.
/// Matches PDL revision r818844.
pub const CURRENT_REVISION: Revision = Revision(818858);

mod browser;
mod error;
mod platform;
mod revision;
