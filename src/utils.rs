use std::path::{Path, PathBuf};

/// Write to file with configured runtime
pub(crate) async fn write<P: AsRef<Path> + Unpin, C: AsRef<[u8]>>(
    path: P,
    contents: C,
) -> std::io::Result<()> {
    tokio::fs::write(path.as_ref(), contents.as_ref()).await
}

/// Canonicalize path
///
/// Chromium sandboxing does not support Window UNC paths which are used by Rust
/// when the path is relative. See https://bugs.chromium.org/p/chromium/issues/detail?id=1415018.
pub(crate) async fn canonicalize<P: AsRef<Path> + Unpin>(path: P) -> std::io::Result<PathBuf> {
    let path = tokio::fs::canonicalize(path.as_ref()).await?;
    Ok(dunce::simplified(&path).to_path_buf())
}

/// Absolute path
///
#[cfg(target_os = "linux")]
fn absolute(path: PathBuf) -> std::io::Result<PathBuf> {
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(dunce::simplified(&path).to_path_buf())
}

/// Check if the given path is a Linux /proc/self/fd/N path
#[cfg(any(target_os = "linux", test))]
fn is_proc_self_fd_path(path: &Path) -> bool {
    let Some(fd) = path
        .to_str()
        .and_then(|path| path.strip_prefix("/proc/self/fd/"))
    else {
        return false;
    };

    fd.parse::<i32>()
        .is_ok_and(|n| n >= 0 && n.to_string() == fd)
}

/// Normalize an executable path without resolving Linux paths that must retain
/// their original form.
pub(crate) async fn normalize_executable_path(path: PathBuf) -> std::io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    if is_proc_self_fd_path(&path) {
        return Ok(path);
    }

    // Canonalize paths to reduce issues with sandboxing
    let executable_cleaned: PathBuf = canonicalize(&path).await?;

    // Handle case where executable is provided by snap, ignore canonicalize result and only make path absolute
    #[cfg(target_os = "linux")]
    if executable_cleaned.to_str().unwrap().ends_with("/snap") {
        return Ok(absolute(path).unwrap());
    }

    Ok(executable_cleaned)
}

pub(crate) mod base64 {
    use base64::engine::general_purpose::STANDARD;
    use base64::{DecodeError, Engine};

    /// Decode base64 using the standard alphabet and padding
    pub fn decode<T: AsRef<[u8]>>(input: T) -> Result<Vec<u8>, DecodeError> {
        STANDARD.decode(input)
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
    format!("({})({params})", function.as_ref())
}

/// Tries to identify whether this a javascript function
pub fn is_likely_js_function(function: impl AsRef<str>) -> bool {
    let mut fun = function.as_ref().trim_start();
    if fun.is_empty() {
        return false;
    }
    let mut offset = 0;

    if fun.starts_with("async ") {
        offset = "async ".len() - 1
    }

    if fun[offset..].trim_start().starts_with("function ") {
        return true;
    } else if skip_args(&mut fun) {
        // attempt to detect arrow functions by stripping the leading arguments and
        // looking for the arrow
        if fun.trim_start().starts_with("=>") {
            return true;
        }
    }
    false
}

/// This attempts to strip any leading pair of parentheses from the input
///
/// `()=>` -> `=>`
/// `(abc, def)=>` -> `=>`
fn skip_args(input: &mut &str) -> bool {
    if !input.starts_with('(') {
        return false;
    }
    let mut open = 1;
    let mut closed = 0;
    *input = &input[1..];
    while !input.is_empty() && open != closed {
        if let Some(idx) = input.find(&['(', ')'] as &[_]) {
            if &input[idx..=idx] == ")" {
                closed += 1;
            } else {
                open += 1;
            }
            *input = &input[idx + 1..];
        } else {
            break;
        }
    }

    open == closed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_proc_self_fd_paths() {
        assert!(is_proc_self_fd_path(Path::new("/proc/self/fd/0")));
        assert!(is_proc_self_fd_path(Path::new("/proc/self/fd/123")));

        for path in [
            "/proc/self/fd/",
            "/proc/self/fd/0001",
            "/proc/self/fd/-1",
            "/proc/self/fd/+1",
            "/proc/self/fd/not-a-fd",
            "/proc/self/fd//123",
            "/proc/self/fd/./123",
            "/proc/self/fd/123/extra",
            "/proc/self/fd/123/../../../456",
            "/proc/self/fd/999999999999999999999999",
        ] {
            assert!(!is_proc_self_fd_path(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn is_js_function() {
        assert!(is_likely_js_function("function abc() {}"));
        assert!(is_likely_js_function("async function abc() {}"));
        assert!(is_likely_js_function("() => {}"));
        assert!(is_likely_js_function("(abc, def) => {}"));
        assert!(is_likely_js_function("((abc), (def)) => {}"));
        assert!(is_likely_js_function("() => Promise.resolve(100 / 25)"));
    }
}
