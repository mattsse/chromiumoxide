use chromeoxid_pdl::build::Generator;
use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    Generator::default()
        .out_dir("./src")
        .compile_pdls(&[
            dir.join("js_protocol.pdl"),
            dir.join("browser_protocol.pdl"),
        ])
        .unwrap();
}
