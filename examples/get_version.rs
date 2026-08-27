//! Minimal smoke test for the blocking CDP wire client.
//!
//! Prerequisites: run Chromium yourself with the devtools port open, e.g.
//!
//! ```sh
//! chromium --headless=new --remote-debugging-port=9222
//! curl -s http://127.0.0.1:9222/json/version | jq -r .webSocketDebuggerUrl
//! ```
//!
//! Then pass that URL as the first argument:
//!
//! ```sh
//! cargo run --example get_version -- ws://127.0.0.1:9222/devtools/browser/...
//! ```

use std::env;
use std::process::ExitCode;

use chromiumoxide::Connection;
use chromiumoxide::cdp::browser_protocol::browser::GetVersionParams;

fn main() -> ExitCode {
    let Some(url) = env::args().nth(1) else {
        eprintln!("usage: get_version <ws-devtools-url>");
        return ExitCode::from(2);
    };

    let (mut conn, _events) = match Connection::connect(url.as_str()) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("connect failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    match conn.send(GetVersionParams::default(), None) {
        Ok(v) => {
            println!("product:  {}", v.product);
            println!("revision: {}", v.revision);
            println!("protocol: {}", v.protocol_version);
            println!("ua:       {}", v.user_agent);
            println!("v8:       {}", v.js_version);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Browser.getVersion failed: {e}");
            ExitCode::FAILURE
        }
    }
}
