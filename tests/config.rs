use crate::{BrowserConfig, test_config};

/// Verifies that Chromiumoxide preserves `/proc/PID/fd/N` executable paths.
///
/// The backing file is unlinked before launch, so preserving the procfs path
/// allows the `exit 0` script to run and produce a successful `LaunchExit`.
/// Canonicalizing it instead resolves to the deleted backing path and fails
/// before the script can be launched.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn preserves_proc_pid_fd_executable_path() {
    use std::os::{fd::AsRawFd, unix::fs::PermissionsExt};
    use std::{env::temp_dir, fs, process::id, time};

    use chromiumoxide::{Browser, error::CdpError};

    let nonce = time::SystemTime::now()
        .duration_since(time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let executable_path = temp_dir().join(format!("chromiumoxide-proc-fd-test-{}-{nonce}", id()));

    // Keep stderr open briefly after the script exits so that Browser::launch
    // observes the successful exit before it observes EOF on stderr.
    fs::write(&executable_path, "#!/bin/sh\nsleep 1 &\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(&executable_path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&executable_path, permissions).unwrap();

    let executable = fs::File::open(&executable_path).unwrap();
    fs::remove_file(&executable_path).unwrap();

    let proc_fd_path = format!("/proc/{}/fd/{}", id(), executable.as_raw_fd());
    let config = BrowserConfig::builder()
        .chrome_executable(proc_fd_path)
        .build()
        .unwrap();

    match Browser::launch(config).await {
        Err(CdpError::LaunchExit(status, _)) => assert!(status.success()),
        Err(error) => panic!("unexpected launch error: {error:?}"),
        Ok(_) => panic!("test executable unexpectedly exposed a DevTools endpoint"),
    }
}

#[tokio::test]
#[ignore] // For some reason, this test fails on CI but works locally
async fn test_config_disable_https_first() {
    test_config(
        BrowserConfig::builder()
            .disable_https_first()
            .build()
            .unwrap(),
        async |browser| {
            let page = browser.new_page("about:blank").await.unwrap();
            page.goto("http://perdu.com").await.unwrap();
            let url = page.url().await.unwrap().unwrap();
            assert!(url.starts_with("http://"));
        },
    )
    .await;
}
