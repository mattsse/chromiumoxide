chromiumoxide
=====================
![Build](https://github.com/mattsse/chromiumoxide/workflows/Continuous%20integration/badge.svg)
[![Crates.io](https://img.shields.io/crates/v/chromiumoxide.svg)](https://crates.io/crates/chromiumoxide)
[![Documentation](https://docs.rs/chromiumoxide/badge.svg)](https://docs.rs/chromiumoxide)

chromiumoxide provides a high-level and async API to control Chrome or Chromium over the [DevTools Protocol](https://chromedevtools.github.io/devtools-protocol/). chromiumoxide comes with support for all types of the [Chrome DevTools Protocol](https://chromedevtools.github.io/devtools-protocol/). chromiumoxide can launch [headless](https://developers.google.com/web/updates/2017/04/headless-chrome) or can be configured to run full (non-headless) Chrome or Chromium or connect to running Chrome or Chromium instance.


⚠️ The API is still unstable, subject to change, untested and incomplete. However all message types, as defined in the protocol definition files ([browser_protocol.pdl](chromiumoxide_cdp/browser_protocol.pdl) and [js_protocol.pdl](chromiumoxide_cdp/js_protocol.pdl)) are supported. PRs, feature requests and issues are welcome.


## Usage

```rust
use futures::StreamExt;

use chromiumoxide::browser::{Browser, BrowserConfig};

#[async_std::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    pretty_env_logger::init();
    
   // create a `Browser` that spawns a `chromium` process running with UI (`with_head()`, headless is default) 
   // and the handler that drives the websocket etc.
    let (browser, mut handler) =
        Browser::launch(BrowserConfig::builder().with_head().build()?).await?;
    
   // spawn the handle to its own task
    let handle = async_std::task::spawn(async move {
        loop {
            let _event = handler.next().await.unwrap();
        }
    });
    
   // create a new browser page and navigate to the url
    let page = browser.new_page("https://en.wikipedia.org").await?;
    
   // type into the search field and hit `Enter`
    page.find_element("input#searchInput")
        .await?
        .click()
        .await?
        .type_str("Rust (programming language)")
        .await?
        .press_key("Enter")
        .await?;
   
    let html = page.wait_for_navigation().await?.content().await?;
   
    handle.await;
    Ok(())
}
```

The current API is still rather limited, but the `Page::execute` allows submitting all `Command`s types (see [Generated Code](README.md#generated-code)). Most `Element` and `Page` functions are basically just simplified command constructions and combinations, like `Page::pdf`:

```rust
  pub async fn pdf(&self, opts: PrintToPdfParams) -> Result<Vec<u8>> {
        let res = self.execute(opts).await?;
        Ok(base64::decode(&res.data)?)
    }
```

If you need something else, the `execute` function allows for writing your own command wrappers. PRs are very welcome.

## Generated Code

The [`chromiumoxide_pdl`](chromiumoxide_pdl) crate contains a [PDL parser](chromiumoxide_pdl/src/pdl/parser.rs), which is a rust rewrite of a [python script in the chromium source tree]( https://chromium.googlesource.com/deps/inspector_protocol/+/refs/heads/master/pdl.py) and a [`Generator`](chromiumoxide_pdl/src/build/generator.rs) that turns the parsed PDL files into rust code. The [`chromiumoxide_cdp`](chromiumoxide_cdp) crate only purpose is to integrate the generator during is build process and include the generated output before compiling the crate itself. This separation is done merely because the generated output is ~60K lines of rust code (not including all the Proc macro extensions). So expect the compilation to take some time.
The generator can be configured and used independently, see [chromiumoxide_cdp/build.rs](chromiumoxide_cdp/build.rs).

Every chrome pdl domain is put in its own rust module, the types for the page domain of the browser_protocol are in `chromiumoxide_cdp::cdp::browser_protocol::page`, the runtime domain of the js_protocol in  `chromiumoxide_cdp::cdp::js_protocol::runtime` and so on.
All Events are bundled in single enum (`CdpEvent`) and for every command there is a `<Commandname>Params` type with builder support `<Commandname>Params::builder()` and its corresponding return type: `<Commandname>Returns`.

[https://vanilla.aslushnikov.com/](https://vanilla.aslushnikov.com/) is a great resource to browser all available types.

## Known Issues

* The rust files generated for the PDL files in [chromiumoxide_cdp](./chromiumoxide_cdp) don't compile when support for experimental types is manually turned off (`export CDP_NO_EXPERIMENTAL=true`). This is because the use of some experimental pdl types in the `*.pdl` files themselves are not marked as experimental.
* Navigations triggered by interaction with the page are currently not waited for when requesting content from the page. Thus, a `page.content()` immediately after an interaction that caused the page to navigate (e.g., manually entering a search box) may come up empty. This could be solved by monitoring and optionally buffering such requests until the new mainframe of the page is fully loaded, like navigation requests are currently handled. 
* `chromiumoxide` requires an installed chromium application and may not be able to find it on its own. The option to download chromium certainly would be a handy feature.

## Troubleshooting

Q: A new chromium instance is being launched but then times out.
A: Check that your chromium language settings are set to English. `chromiumoxide` tries to parse the debugging port from the chromium process output and that is limited to english.

## License

Licensed under either of these:

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   https://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   https://opensource.org/licenses/MIT)
   

## References

* [chromedp](https://github.com/chromedp/chromedp)
* [rust-headless-chrome](https://github.com/Edu4rdSHL/rust-headless-chrome) which the launch config, `KeyDefinition` and typing support is taken from.
* [puppeteer](https://github.com/puppeteer/puppeteer)