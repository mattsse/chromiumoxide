use chromiumoxide_cdp::CURRENT_REVISION;
use chromiumoxide_pdl::build::Generator;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Check that the generated files are up to date
#[test]
fn generated_code_is_fresh() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let js_proto = env::var("CDP_JS_PROTOCOL_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dir.join("pdl/js_protocol.pdl"));

    let browser_proto = env::var("CDP_BROWSER_PROTOCOL_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dir.join("pdl/browser_protocol.pdl"));

    let tmp = tempfile::tempdir().unwrap();
    Generator::default()
        .out_dir(tmp.path())
        .experimental(env::var("CDP_NO_EXPERIMENTAL").is_err())
        .deprecated(env::var("CDP_DEPRECATED").is_ok())
        .allowed_deprecated_type("emulateNetworkConditions")
        .compile_pdls(&[js_proto, browser_proto])
        .unwrap();

    let new = fs::read_to_string(tmp.path().join("cdp.rs")).unwrap();
    let src = dir.join("src/cdp.rs");
    let old = fs::read_to_string(&src).unwrap();
    if new != old {
        fs::write(src, new).unwrap();
        panic!("generated code in the repository is outdated, updating...");
    }
}

/// Check that the PDL files are up to date
#[tokio::test]
async fn pdl_is_fresh() {
    const BASE_URL: &str = "https://raw.githubusercontent.com/ChromeDevTools/devtools-protocol";

    // Some domain includes don't have the actual pdl files yet, so ignore them for now
    const IGNORED_DOMAINS: &[&str] = &["SmartCardEmulation", "WebMCP"];

    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut modified = false;

    // JS protocol
    let js_proto = env::var("CDP_JS_PROTOCOL_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dir.join("pdl/js_protocol.pdl"));

    let js_proto_old = fs::read_to_string(&js_proto).unwrap_or_default();
    let js_proto_new = reqwest::get(&format!(
        "{BASE_URL}/{CURRENT_REVISION}/pdl/js_protocol.pdl",
    ))
    .await
    .unwrap()
    .text()
    .await
    .unwrap();
    assert!(js_proto_new.contains("The Chromium Authors"));

    if js_proto_new != js_proto_old {
        fs::write(js_proto, js_proto_new).unwrap();
        modified = true;
    }

    // Browser protocol
    let browser_proto = env::var("CDP_BROWSER_PROTOCOL_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dir.join("pdl/browser_protocol.pdl"));

    let browser_proto_old = fs::read_to_string(&browser_proto).unwrap_or_default();
    let browser_proto_new = reqwest::get(&format!(
        "{BASE_URL}/{CURRENT_REVISION}/pdl/browser_protocol.pdl"
    ))
    .await
    .unwrap()
    .text()
    .await
    .unwrap();
    assert!(browser_proto_new.contains("The Chromium Authors"));

    let browser_proto_new = browser_proto_new
        .lines()
        .map(|line| {
            if !line.starts_with("include") {
                return line.into();
            }

            for domain in IGNORED_DOMAINS {
                if line.contains(domain) {
                    return format!("# no actual pdl file -- {line}");
                }
            }

            line.into()
        })
        .collect::<Vec<_>>()
        .join("\n");

    if browser_proto_new != browser_proto_old {
        fs::write(browser_proto, &browser_proto_new).unwrap();
        modified = true;
    }

    // Browser includes
    let mut browser_includes = Vec::new();
    for line in browser_proto_new.lines() {
        if line.starts_with("include") {
            let name = line.split_whitespace().nth(1).unwrap();
            browser_includes.push(name);
        }
    }

    for include in browser_includes {
        let include_path = dir.join("pdl").join(include);

        let include_proto_old = fs::read_to_string(&include_path).unwrap_or_default();
        let include_proto_new =
            reqwest::get(&format!("{BASE_URL}/{CURRENT_REVISION}/pdl/{include}"))
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
        assert!(include_proto_new.contains("The Chromium Authors"));

        if include_proto_new != include_proto_old {
            if let Some(parent) = include_path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(include_path, include_proto_new).unwrap();
            modified = true;
        }
    }

    if modified {
        panic!("pdl in the repository are outdated, updating...");
    }
}
