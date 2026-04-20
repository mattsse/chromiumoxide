# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Bump CDP to `r1596832`
- Derive `Clone` for `Element`, `ScreenshotParams` and `ScreenshotParamsBuilder`

## [0.9.1] 2026-02-25

### Fixed

- Invalid import when using zip0

## [0.9.0] 2026-02-20

### Breaking Changes

- Removed async-std support

### Changed

- Updated edition to 2024
- Bump fetcher chromium to `r1585606`
- Bump CDP to `r1566079`
- Use a struct `Arg` for arguments to combine flags automatically
- Browser process no longer inherits stdout
- Update `reqwest` to v0.13
- Update `thiserror` to v2
- Update `heck` to v0.5
- Add support for `zip` v8 and make it default

### Added

- Add `add_init_script` to `Page` for scripts before navigation
- Add `click_with` to `Page` and `Element` for custom click behavior

## [0.8.0] 2025-11-28

### Breaking Changes

Due to the support of new browsers in the fetcher, we changed the API.
The changes are mainly related to how you can request a specific version of the browser.
Previously you could only request a chromium revision, now we support chrome versions, channel and milestone.

```rust
BrowserFetcherOptions::builder()
  .with_kind(BrowserKind::Chrome)
  .with_version(BrowserVersion::Channel(Channel::Beta))
  .build()
```

We also change the output format of the `fetch`. It is now called a `BrowserFetcherInstallation`
and contains a `BuildInfo`. We garantee that at least the version or revision will be present
in that struct, but not always both.

```rust
let installation = chromiumoxide_fetcher::BrowserFetcher::new(options).fetch().await?;
println!("Executable path: {}", installation.executable_path.display());
```

Finally, we switched the async runtime tokio by default. We will remove for support for async-std in the next release.
If you want to part of the discussion on other runtime support, see [#273](https://github.com/mattsse/chromiumoxide/issues/273).

### Changed

- Bumped MSRV to 1.85 to support edition 2024
- Update `async-tungstenite` to 0.32.0
- Update `which` to 8.0.0
- Replace `winreg` by `windows-registry`
- Updated PDL to r1519099 (Chromium 142.0.7431.0)
- Updated fetcher to r1520176 (Chromium 142.0.7435.0)
- Fetch now supports `Chrome for testing` and `Chrome Headless Shell`
- Now uses tokio by default

### Added

- Add option to disable automation detection
- Expose the `cmd` module for access to `CommandChain`

### Fixed

- Fixed typo in feature `_fetcher-rustls-tokio`
- More resilient message parsing, it should now not crash on unknown events coming from the browser
- Extensions should only be disabled when no extensions are provided

[Unreleased]: https://github.com/mattsse/chromiumoxide/compare/v0.7.0...HEAD
