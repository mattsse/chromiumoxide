use std::time::Duration;
use std::{
    collections::HashMap,
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    process::{self, Child, Stdio},
};

use futures::channel::mpsc::{channel, Sender};
use futures::channel::oneshot::channel as oneshot_channel;
use futures::SinkExt;

use chromiumoxid_types::*;

use crate::cmd::{to_command_response, CommandMessage};
use crate::conn::Connection;
use crate::error::{CdpError, Result};
use crate::handler::{Handler, HandlerMessage};
use crate::page::Page;
use chromiumoxid_cdp::cdp::browser_protocol::target::CreateTargetParams;
use chromiumoxid_cdp::cdp::CdpEventMessage;

/// A [`Browser`] is created when chromiumoxid connects to a Chromium instance.
pub struct Browser {
    /// The `Sender` to send messages to the connection handler that drives the
    /// websocket
    sender: Sender<HandlerMessage>,
    /// How the spawned chromium instance was configured, if any
    config: Option<BrowserConfig>,
    /// The spawned chromium instance
    child: Option<Child>,
    /// The debug web socket url of the chromium instance
    debug_ws_url: String,
}

impl Browser {
    /// Connect to an already running chromium instance via websocket
    pub async fn connect(debug_ws_url: impl Into<String>) -> Result<(Self, Handler)> {
        let debug_ws_url = debug_ws_url.into();
        let conn = Connection::<CdpEventMessage>::connect(&debug_ws_url).await?;

        let (tx, rx) = channel(1);

        let fut = Handler::new(conn, rx);
        let browser = Self {
            sender: tx,
            config: None,
            child: None,
            debug_ws_url,
        };
        Ok((browser, fut))
    }

    /// Launches a new instance of `chromium` in the background and attaches to
    /// its debug web socket.
    ///
    /// This fails when no chromium executable could be detected.
    ///
    /// This fails if no web socket url could be detected from the child
    /// processes stderr for more than 20 seconds.
    pub async fn launch(config: BrowserConfig) -> Result<(Self, Handler)> {
        // launch a new chromium instance
        let mut child = config.launch()?;

        // extract the ws:
        let get_ws_url = ws_url_from_output(&mut child);

        let dur = Duration::from_secs(20);
        let debug_ws_url = async_std::future::timeout(dur, get_ws_url)
            .await
            .map_err(|_| CdpError::Timeout)?;

        let conn = Connection::<CdpEventMessage>::connect(&debug_ws_url).await?;

        let (tx, rx) = channel(1);

        let fut = Handler::new(conn, rx);

        let browser = Self {
            sender: tx,
            config: Some(config),
            child: Some(child),
            debug_ws_url,
        };

        Ok((browser, fut))
    }

    /// Returns the address of the websocket this browser is attached to
    pub fn websocket_address(&self) -> &String {
        &self.debug_ws_url
    }

    /// The config of the spawned chromium instance if any.
    pub fn config(&self) -> Option<&BrowserConfig> {
        self.config.as_ref()
    }

    /// Create a new browser page
    pub async fn new_page(&self, params: impl Into<CreateTargetParams>) -> Result<Page> {
        let (tx, rx) = oneshot_channel();

        self.sender
            .clone()
            .send(HandlerMessage::CreatePage(params.into(), tx))
            .await?;

        rx.await?
    }

    pub async fn new_blank_tab(&self) -> anyhow::Result<Page> {
        Ok(self
            .new_page(CreateTargetParams::new("about:blank"))
            .await?)
    }

    /// Call a browser method.
    pub async fn execute<T: Command>(&self, cmd: T) -> Result<CommandResponse<T::Response>> {
        let (tx, rx) = oneshot_channel();
        let method = cmd.identifier();
        let msg = CommandMessage::new(cmd, tx)?;

        self.sender
            .clone()
            .send(HandlerMessage::Command(msg))
            .await?;
        let resp = rx.await??;
        to_command_response::<T>(resp, method)
    }

    /// Return all of the pages of the browser
    pub async fn pages(&self) -> Result<Vec<Page>> {
        let (tx, rx) = oneshot_channel();
        self.sender
            .clone()
            .send(HandlerMessage::GetPages(tx))
            .await?;
        Ok(rx.await?)
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            child.kill().expect("!kill");
        }
    }
}

async fn ws_url_from_output(child_process: &mut Child) -> String {
    let stdout = child_process.stderr.take().expect("no stderror");
    let handle = async_std::task::spawn_blocking(|| {
        let mut buf = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            if buf.read_line(&mut line).is_ok() {
                // check for ws in line
                if let Some(ws) = line.rsplit("listening on ").next() {
                    if ws.starts_with("ws") && ws.contains("devtools/browser") {
                        return ws.trim().to_string();
                    }
                }
            } else {
                line = String::new();
            }
        }
    });
    handle.await
}

#[derive(Debug, Clone)]
pub struct BrowserConfig {
    /// Determines whether to run headless version of the browser. Defaults to
    /// true.
    headless: bool,
    /// Determines whether to run the browser with a sandbox.
    sandbox: bool,
    /// Launch the browser with a specific window width and height.
    window_size: Option<(u32, u32)>,
    /// Launch the browser with a specific debugging port.
    port: u16,
    /// Path for Chrome or Chromium.
    ///
    /// If unspecified, the create will try to automatically detect a suitable
    /// binary.
    executable: std::path::PathBuf,

    /// A list of Chrome extensions to load.
    ///
    /// An extension should be a path to a folder containing the extension code.
    /// CRX files cannot be used directly and must be first extracted.
    ///
    /// Note that Chrome does not support loading extensions in headless-mode.
    /// See https://bugs.chromium.org/p/chromium/issues/detail?id=706008#c5
    extensions: Vec<String>,

    /// Environment variables to set for the Chromium process.
    /// Passes value through to std::process::Command::envs.
    pub process_envs: Option<HashMap<String, String>>,

    /// Data dir for user data
    pub user_data_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct BrowserConfigBuilder {
    headless: bool,
    sandbox: bool,
    window_size: Option<(u32, u32)>,
    port: u16,
    executable: Option<PathBuf>,
    extensions: Vec<String>,
    process_envs: Option<HashMap<String, String>>,
    user_data_dir: Option<PathBuf>,
}

impl BrowserConfig {
    pub fn builder() -> BrowserConfigBuilder {
        BrowserConfigBuilder::default()
    }

    pub fn with_executable(path: impl AsRef<Path>) -> Self {
        Self::builder().chrome_executable(path).build().unwrap()
    }
}

impl Default for BrowserConfigBuilder {
    fn default() -> Self {
        Self {
            headless: true,
            sandbox: true,
            window_size: None,
            port: 0,
            executable: None,
            extensions: vec![],
            process_envs: None,
            user_data_dir: None,
        }
    }
}

impl BrowserConfigBuilder {
    pub fn window_size(mut self, width: u32, height: u32) -> Self {
        self.window_size = Some((width, height));
        self
    }

    pub fn no_sandbox(mut self) -> Self {
        self.sandbox = false;
        self
    }

    pub fn with_head(mut self) -> Self {
        self.headless = false;
        self
    }

    pub fn user_data_dir(mut self, data_dir: impl AsRef<Path>) -> Self {
        self.user_data_dir = Some(data_dir.as_ref().to_path_buf());
        self
    }

    pub fn chrome_executable(mut self, path: impl AsRef<Path>) -> Self {
        self.executable = Some(path.as_ref().to_path_buf());
        self
    }

    pub fn extension(mut self, extension: impl Into<String>) -> Self {
        self.extensions.push(extension.into());
        self
    }

    pub fn extensions<I, S>(mut self, extensions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for ext in extensions {
            self.extensions.push(ext.into());
        }
        self
    }

    pub fn env(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.process_envs
            .get_or_insert(HashMap::new())
            .insert(key.into(), val.into());
        self
    }

    pub fn envs<I, K, V>(mut self, envs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.process_envs
            .get_or_insert(HashMap::new())
            .extend(envs.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    pub fn build(self) -> std::result::Result<BrowserConfig, String> {
        let executable = if let Some(e) = self.executable {
            e
        } else {
            default_executable()?
        };

        Ok(BrowserConfig {
            headless: self.headless,
            sandbox: self.sandbox,
            window_size: self.window_size,
            port: self.port,
            executable,
            extensions: self.extensions,
            process_envs: None,
            user_data_dir: None,
        })
    }
}

impl BrowserConfig {
    pub fn launch(&self) -> io::Result<Child> {
        let dbg_port = format!("--remote-debugging-port={}", self.port);

        let args = [
            dbg_port.as_str(),
            "--disable-background-networking",
            "--enable-features=NetworkService,NetworkServiceInProcess",
            "--disable-background-timer-throttling",
            "--disable-backgrounding-occluded-windows",
            "--disable-breakpad",
            "--disable-client-side-phishing-detection",
            "--disable-component-extensions-with-background-pages",
            "--disable-default-apps",
            "--disable-dev-shm-usage",
            "--disable-extensions",
            "--disable-features=TranslateUI",
            "--disable-hang-monitor",
            "--disable-ipc-flooding-protection",
            "--disable-popup-blocking",
            "--disable-prompt-on-repost",
            "--disable-renderer-backgrounding",
            "--disable-sync",
            "--force-color-profile=srgb",
            "--metrics-recording-only",
            "--no-first-run",
            "--enable-automation",
            "--password-store=basic",
            "--use-mock-keychain",
            "--enable-blink-features=IdleDetection",
        ];

        let mut cmd = process::Command::new(&self.executable);
        cmd.args(&args).args(&DEFAULT_ARGS).args(
            self.extensions
                .iter()
                .map(|e| format!("--load-extension={}", e)),
        );

        if let Some(ref user_data) = self.user_data_dir {
            cmd.arg(format!("--user-data-dir={}", user_data.display()));
        }

        if let Some((width, height)) = self.window_size {
            cmd.arg(format!("--window-size={},{}", width, height));
        }

        if !self.sandbox {
            cmd.args(&["--no-sandbox", "--disable-setuid-sandbox"]);
        }

        if self.headless {
            cmd.args(&["--headless", "--hide-scrollbars", "--mute-audio"]);
        }

        if let Some(ref envs) = self.process_envs {
            cmd.envs(envs);
        }
        cmd.stderr(Stdio::piped()).spawn()
    }
}

/// Returns the path to Chrome's executable.
///
/// If the `CHROME` environment variable is set, `default_executable` will
/// use it as the default path. Otherwise, the filenames `google-chrome-stable`
/// `chromium`, `chromium-browser`, `chrome` and `chrome-browser` are
/// searched for in standard places. If that fails,
/// `/Applications/Google Chrome.app/...` (on MacOS) or the registry (on
/// Windows) is consulted. If all of the above fail, an error is returned.
pub fn default_executable() -> Result<std::path::PathBuf, String> {
    if let Ok(path) = std::env::var("CHROME") {
        if std::path::Path::new(&path).exists() {
            return Ok(path.into());
        }
    }

    for app in &[
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "chrome",
        "chrome-browser",
    ] {
        if let Ok(path) = which::which(app) {
            return Ok(path);
        }
    }

    #[cfg(target_os = "macos")]
    {
        let default_paths = &["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"][..];
        for path in default_paths {
            if std::path::Path::new(path).exists() {
                return Ok(path.into());
            }
        }
    }

    #[cfg(windows)]
    {
        use crate::browser::process::get_chrome_path_from_registry;

        if let Some(path) = get_chrome_path_from_registry() {
            if path.exists() {
                return Ok(path);
            }
        }
    }

    Err("Could not auto detect a chrome executable".to_string())
}

/// These are passed to the Chrome binary by default.
/// Via https://github.com/puppeteer/puppeteer/blob/4846b8723cf20d3551c0d755df394cc5e0c82a94/src/node/Launcher.ts#L157
static DEFAULT_ARGS: [&str; 23] = [
    "--disable-background-networking",
    "--enable-features=NetworkService,NetworkServiceInProcess",
    "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows",
    "--disable-breakpad",
    "--disable-client-side-phishing-detection",
    "--disable-component-extensions-with-background-pages",
    "--disable-default-apps",
    "--disable-dev-shm-usage",
    "--disable-extensions",
    "--disable-features=TranslateUI",
    "--disable-hang-monitor",
    "--disable-ipc-flooding-protection",
    "--disable-popup-blocking",
    "--disable-prompt-on-repost",
    "--disable-renderer-backgrounding",
    "--disable-sync",
    "--force-color-profile=srgb",
    "--metrics-recording-only",
    "--no-first-run",
    "--enable-automation",
    "--password-store=basic",
    "--use-mock-keychain",
];
