pub use self::browser::{BrowserFetcher, BrowserFetcherOptions};
pub use self::error::FetcherError;
pub use self::platform::Platform;
pub use self::revision::Revision;

pub const CURRENT_REVISION: Revision = Revision(818844);

mod browser;
mod error;
mod platform;
mod revision;
