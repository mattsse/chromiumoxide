use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    chromeoxid_pdl::build::compile_pdls(&[
        dir.join("js_protocol.pdl"),
        dir.join("browser_protocol.pdl"),
    ])
    .unwrap();
}
