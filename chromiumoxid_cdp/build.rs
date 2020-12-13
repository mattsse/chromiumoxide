use chromiumoxid_pdl::build::Generator;
use std::env;
use std::path::{Path, PathBuf};

/// Compile the pdl files
fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let js_proto = env::var("CDP_JS_PROTOCOL_PATH")
        .map(|p| PathBuf::from(p))
        .unwrap_or_else(|_| dir.join("js_protocol.pdl"));

    let browser_proto = env::var("CDP_BROWSER_PROTOCOL_PATH")
        .map(|p| PathBuf::from(p))
        .unwrap_or_else(|_| dir.join("browser_protocol.pdl"));

    Generator::default()
        .experimental(!env::var("CDP_NO_EXPERIMENTAL").is_ok())
        .deprecated(env::var("CDP_DEPRECATED").is_ok())
        .compile_pdls(&[js_proto, browser_proto])
        .unwrap();
}
