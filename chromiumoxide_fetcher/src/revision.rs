use std::fmt;

use crate::FetcherError;

/// A [`Revision`] represents a version of chromium.
///
/// The revision must be compatible with the Chrome DevTools Protocol (CDP)
/// shipped with chromiumoxide otherwise it will fail to communicate with
/// the browser.
#[derive(Clone, Debug, PartialOrd, Ord, PartialEq, Eq)]
pub struct Revision(pub(crate) u32);

impl From<u32> for Revision {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Revision> for u32 {
    fn from(value: Revision) -> Self {
        value.0
    }
}

impl TryFrom<String> for Revision {
    type Error = FetcherError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value
            .parse::<u32>()
            .map_err(|e| FetcherError::InvalidRevision(e))
            .map(|v| Self(v))
    }
}

impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
