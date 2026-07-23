# chromiumoxide

![Build](https://github.com/mattsse/chromiumoxide/workflows/Continuous%20integration/badge.svg)
[![Crates.io](https://img.shields.io/crates/v/chromiumoxide.svg)](https://crates.io/crates/chromiumoxide)
[![Documentation](https://docs.rs/chromiumoxide/badge.svg)](https://docs.rs/chromiumoxide)

chromiumoxide provides a high-level and async API to control Chrome or Chromium over the [DevTools Protocol](https://chromedevtools.github.io/devtools-protocol/). It comes with support for all types of the [Chrome DevTools Protocol](https://chromedevtools.github.io/devtools-protocol/) and can launch a [headless](https://developers.google.com/web/updates/2017/04/headless-chrome) or full (non-headless) Chrome or Chromium instance or connect to an already running instance.

## Usage

```rust
use futures::StreamExt;

use chromiumoxide::browser::{Browser, BrowserConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // create a `Browser` that spawns a `chromium` process running with UI (`with_head()`, headless is default)
    // and the handler that drives the websocket etc.
    let (mut browser, mut handler) =
        Browser::launch(BrowserConfig::builder().with_head().build()?).await?;

    // spawn a new task that continuously polls the handler
    let handle = tokio::spawn(async move {
        while let Some(h) = handler.next().await {
            if h.is_err() {
                break;
            }
        }
    });

    // create a new browser page and navigate to the url
    let page = browser.new_page("https://en.wikipedia.org").await?;

    // find and click the search toggle button to reveal the search bar
    page.find_element(".search-toggle").await?.click().await?;

    // find the search bar type into the search field and hit `Enter`,
    // this triggers a new navigation to the search result page
    page.find_element("input[name='search']")
        .await?
        .click()
        .await?
        .type_str("Rust programming language")
        .await?
        .press_key("Enter")
        .await?;

    let html = page.wait_for_navigation().await?.content().await?;

    browser.close().await?;
    handle.await?;
    Ok(())
}
```

The current API still lacks some functionality, but the [`Page::execute`](src/page.rs) function allows sending all `chromiumoxide_types::Command` types (see [Generated Code](README.md#generated-code)). Most `Element` and `Page` functions are basically just simplified command constructions and combinations, like `Page::pdf`:

```rust
pub async fn pdf(&self, params: PrintToPdfParams) -> Result<Vec<u8>> {
    let res = self.execute(params).await?;
    Ok(base64::decode(&res.data)?)
}
```

If you need something else, the `Page::execute` function allows for writing your own command wrappers. PRs are very welcome if you think a meaningful command is missing a designated function.

## Frames and cross-origin iframes

`Page::main_frame`, `Page::all_frames`, and `Page::frame_by_id` return `Frame`
handles pinned to the CDP session that owned the frame when the handle was created.
That lets the same API work for ordinary child frames and out-of-process iframes
(OOPIFs):

```rust
for frame in page.all_frames().await? {
    println!(
        "frame={} session={} oop={} url={:?}",
        frame.id().as_ref(),
        frame.session_id().as_ref(),
        frame.is_out_of_process(),
        frame.url(),
    );

    if frame.is_out_of_process() {
        let title: String = frame.eval("document.title").await?.into_value()?;
        println!("child title: {title}");
    }
}
```

Frame handles support raw `execute`, JavaScript `eval`, `query_selector`, navigation,
navigation waiting, and parent/child traversal. Elements returned by a frame retain the
same session and can be queried, clicked, hovered, measured, and explicitly disposed.
See [`examples/iframe.rs`](examples/iframe.rs) for a complete example.

Navigate a frame with `Frame::goto`, which pins both the frame and its session. Raw
`Frame::execute` rejects `Page.navigate` with `CdpError::NotAllowed`, because a raw command
loses the frame identity a navigation watcher needs and would target the page's main frame.

A frame can move to another CDP session during a process swap. An old handle then returns
`CdpError::FrameNotReady`; enumerate the frames again or call `frame_by_id` to obtain a
fresh handle. There is a short browser-controlled swap-in window where a frame can be
attached but its OOP session is not yet ready, so retry `FrameNotReady` operations after
refreshing the handle.

When a page target is destroyed or the connection closes, in-flight frame operations are
settled with a typed error rather than a channel cancellation: `Frame::wait_for_navigation`
resolves to `CdpError::FrameNotReady`, while `Page::goto` / `Frame::goto` resolve to
`CdpError::NoResponse`. `Frame::fetch_url` is the one read that follows the frame's current
session instead of failing on a stale handle; see its rustdoc.

## Managed request interception

Use `PausedRequest` as the response capability for Fetch interception. Register the stream
first, await Chrome-side enablement second, and only then navigate:

```rust
let mut paused_requests = page.paused_requests().await?;
page.set_request_interception(true).await?;

while let Some(paused) = paused_requests.next().await {
    if paused.event().request.url.ends_with(".png") {
        paused.fail(ErrorReason::BlockedByClient).await?;
    } else {
        paused.continue_request().await?;
    }
}
```

`continue_request`, `continue_with`, `fulfill`, and `fail` consume the handle and always
use the request id and CDP session captured with the pause. This is required for OOPIF
requests; a raw `Page::execute(ContinueRequestParams { .. })` is a main-session operation.
Dropping a delivered `PausedRequest` has no protocol side effect, so every request that
must be released needs an explicit response. Dropping the stream allows a later stream to
register, but does not answer already delivered requests. See
[`examples/interception.rs`](examples/interception.rs) and
[`examples/block-navigation.rs`](examples/block-navigation.rs).

Await ordering-sensitive state changes such as `authenticate` and
`set_request_interception` before `Page::goto` or `Frame::goto`. Their futures establish
the ordering; launching them concurrently with navigation does not.

If either update returns an error, retry it before navigating. The retry always sends a
fresh idempotent Chrome configuration batch and waits for its acknowledgement. Until that
retry succeeds, a newly attached OOPIF can briefly inherit the requested interception/auth
state even though the main session did not confirm it; the failed caller has already received
`Err`, so applications should treat the configuration as unavailable during that window.

## Compatibility and current limitations

The frame/OOPIF API has two intentional source-compatibility exceptions:

- `Element::node_id` is now `Option<NodeId>` because frame-scoped elements may only have a
  remote object and backend node id.
- `CdpError` is now `#[non_exhaustive]`. Making an existing public enum non-exhaustive is the
  one-time source break here: after upgrading, a downstream exhaustive `match` on `CdpError`
  must add a wildcard (`_`) arm. Once that arm exists, the new variants are additive and cost
  nothing further — `FrameNotReady`, `PausedRequestResponderAlreadyRegistered`, and `NotAllowed`
  (returned when a raw operation is submitted through an entry point that does not support it,
  e.g. `Frame::execute(Page.navigate)` — use `Frame::goto` instead).

Existing `TargetMessage`, `GetExecutionContext`, the five-variant `NetworkEvent`, and legacy
handler signatures remain unchanged.

Current OOPIF limitations include:

- `expose_function`, stealth helper replay, and dynamic user-agent replay remain
  main-session-only.
- High-level frame-scoped console/network event attribution and full cross-session response
  body adoption are not exposed yet; typed raw events remain available.
- Init scripts are replayed when added, but there is no public remove-init-script API.
  The default behavior applies scripts to future documents rather than current contexts;
  `runImmediately=true` is not guaranteed during paused OOPIF initialization.
- `Element::dispose().await` releases its remote object explicitly. Dropping an `Element`
  remains a local operation and does not send a release command.

### Add chromiumoxide to your project

`chromiumoxide` only supports the [`tokio`](https://github.com/tokio-rs/tokio) runtime.

## Generated Code

The [`chromiumoxide_pdl`](chromiumoxide_pdl) crate contains a [PDL parser](chromiumoxide_pdl/src/pdl/parser.rs), which is a rust rewrite of a [python script in the chromium source tree](https://chromium.googlesource.com/deps/inspector_protocol/+/refs/heads/master/pdl.py) and a [`Generator`](chromiumoxide_pdl/src/build/generator.rs) that turns the parsed PDL files into rust code. The [`chromiumoxide_cdp`](chromiumoxide_cdp) crate only purpose is to invoke the generator during its [build process](chromiumoxide_cdp/build.rs) and [include the generated output](chromiumoxide_cdp/src/lib.rs) before compiling the crate itself. This separation is done merely because the generated output is ~60K lines of rust code (not including all the proc macro expansions). So expect the compiling to take some time.
The generator can be configured and used independently, see [chromiumoxide_cdp/build.rs](chromiumoxide_cdp/build.rs).

Every chrome pdl domain is put in its own rust module, the types for the page domain of the browser_protocol are in `chromiumoxide_cdp::cdp::browser_protocol::page`, the runtime domain of the js_protocol in `chromiumoxide_cdp::cdp::js_protocol::runtime` and so on.

[vanilla.aslushnikov.com](https://vanilla.aslushnikov.com/) is a great resource to browse all the types defined in the pdl files. This site displays `Command` types as defined in the pdl files as `Method`. `chromiumoxid` sticks to the `Command` nomenclature. So for everything that is defined as a command type in the pdl (=marked as `Method` on [vanilla.aslushnikov.com](https://vanilla.aslushnikov.com/)) `chromiumoxide` contains a type for command and a designated type for the return type. For every command there is a `<name of command>Params` type with builder support (`<name of command>Params::builder()`) and its corresponding return type: `<name of command>Returns`. All commands share an implementation of the `chromiumoxide_types::Command` trait.
All Events are bundled in single enum (`CdpEvent`)

## Fetcher

By default `chromiumoxide` will try to find an installed version of chromium on the computer it runs on.
It is possible to download and install one automatically for some platforms using the `fetcher` feature.

You need to enable either the `rustls` or the `native-tls` feature and the `zip0` or `zip8` feature to allow the fetcher to download binaries.

```rust
use std::path::Path;

use futures::StreamExt;
use chromiumoxide::browser::{BrowserConfig};
use chromiumoxide::fetcher::{BrowserFetcher, BrowserFetcherOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let download_path = Path::new("./download");
    tokio::fs::create_dir_all(&download_path).await?;
    let fetcher = BrowserFetcher::new(
        BrowserFetcherOptions::builder()
            .with_path(&download_path)
            .build()?,
    );
    let info = fetcher.fetch().await?;

    let config = BrowserConfig::builder()
        .chrome_executable(info.executable_path)
        .build()?,
}
```

## Known Issues

- The rust files generated for the PDL files in [chromiumoxide_cdp](./chromiumoxide_cdp) don't compile when support for experimental types is manually turned off (`export CDP_NO_EXPERIMENTAL=true`). This is because the use of some experimental pdl types in the `*.pdl` files themselves are not marked as experimental.

## Troubleshooting

Q: A new chromium instance is being launched but then times out.

A: Check that your chromium language settings are set to English. `chromiumoxide` tries to parse the debugging port from the chromium process output and that is limited to english.

## License

Licensed under either of these:

- Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <https://opensource.org/licenses/MIT>)

## References

- [chromedp](https://github.com/chromedp/chromedp)
- [rust-headless-chrome](https://github.com/atroche/rust-headless-chrome) which the launch config, `KeyDefinition` and typing support among others is taken from.
- [puppeteer](https://github.com/puppeteer/puppeteer)
