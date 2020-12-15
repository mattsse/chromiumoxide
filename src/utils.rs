use std::path::Path;

/// Write to file with configured runtime
pub(crate) async fn write<P: AsRef<Path> + Unpin, C: AsRef<[u8]>>(
    path: P,
    contents: C,
) -> std::io::Result<()> {
    cfg_if::cfg_if! {
        if #[cfg(feature = "async-std-runtime")] {
            async_std::fs::write(path.as_ref(), contents.as_ref()).await
        } else if #[cfg(feature = "tokio-runtime")] {
            tokio::fs::write(path.as_ref(), contents.as_ref()).await
        }
    }
}
