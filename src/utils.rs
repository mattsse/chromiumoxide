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
    fn is_js_function() {
        assert!(is_likely_js_function("function abc() {}"));
        assert!(is_likely_js_function("async function abc() {}"));
        assert!(is_likely_js_function("() => {}"));
        assert!(is_likely_js_function("(abc, def) => {}"));
        assert!(is_likely_js_function("((abc), (def)) => {}"));
        assert!(is_likely_js_function("() => Promise.resolve(100 / 25)"));
    }
}
