chromiumoxid
=====================
![Build](https://github.com/mattsse/chromiumoxid/workflows/Continuous%20integration/badge.svg)
[![Crates.io](https://img.shields.io/crates/v/chromiumoxid.svg)](https://crates.io/crates/chromiumoxid)
[![Documentation](https://docs.rs/chromiumoxid/badge.svg)](https://docs.rs/chromiumoxid)


## Generated Code
`chromiumoxid` generates Rust code from `*.pdl` files.


## Troubleshooting

Q: A new chromium is being launched but then times out.
A: Check that your chromium language settings are set to English. `chromiumoxid` tries to parse the debugging port from the chromium process output and that is limited to english.

## License

Licensed under either of these:

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   https://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   https://opensource.org/licenses/MIT)
   

## References

* [chromedp](https://github.com/chromedp/chromedp)
* [rust-headless-chrome](https://github.com/Edu4rdSHL/rust-headless-chrome)