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

/// Creates a javascript function string as `(<function>)("<param 1>", "<param
/// 2>")`
pub fn evaluation_string(function: impl AsRef<str>, params: &[impl AsRef<str>]) -> String {
    let params = params
        .iter()
        .map(|s| format!("\"{}\"", s.as_ref()))
        .collect::<Vec<_>>()
        .join(",");
    format!("({})({})", function.as_ref(), params)
}
