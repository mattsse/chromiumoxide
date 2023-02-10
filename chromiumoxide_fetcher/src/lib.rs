pub use self::browser::{BrowserFetcher, BrowserFetcherOptions, BrowserFetcherRevisionInfo};
pub use self::error::FetcherError;
pub use self::platform::Platform;
pub use self::revision::Revision;

/// Currently downloaded chromium revision
pub const CURRENT_REVISION: Revision = Revision(1045629);

mod browser;
mod error;
mod platform;
mod revision;
