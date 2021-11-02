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

use chromiumoxide_cdp::cdp::browser_protocol::target::{
    CreateBrowserContextParams, CreateTargetParams, DisposeBrowserContextParams, TargetId,
};
use chromiumoxide_cdp::cdp::CdpEventMessage;
use chromiumoxide_types::*;

use crate::cmd::{to_command_response, CommandMessage};
use crate::conn::Connection;
use crate::error::{CdpError, Result};
use crate::handler::browser::BrowserContext;
use crate::handler::viewport::Viewport;
use crate::handler::{Handler, HandlerConfig, HandlerMessage, REQUEST_TIMEOUT};
use crate::page::Page;
use chromiumoxide_cdp::cdp::browser_protocol::browser::{GetVersionParams, GetVersionReturns};

/// A [`Browser`] is created when chromiumoxide connects to a Chromium instance.
#[derive(Debug)]
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
    /// The context of the browser
    browser_context: BrowserContext,
}

impl Browser {
    /// Connect to an already running chromium instance via websocket
    pub async fn connect(debug_ws_url: impl Into<String>) -> Result<(Self, Handler)> {
        let debug_ws_url = debug_ws_url.into();
        let conn = Connection::<CdpEventMessage>::connect(&debug_ws_url).await?;

        let (tx, rx) = channel(1);

        let fut = Handler::new(conn, rx, HandlerConfig::default());
        let browser_context = fut.default_browser_context().clone();

        let browser = Self {
            sender: tx,
            config: None,
            child: None,
            debug_ws_url,
            browser_context,
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

        cfg_if::cfg_if! {
            if #[cfg(feature = "async-std-runtime")] {
                let debug_ws_url = async_std::future::timeout(dur, get_ws_url)
            .await
            .map_err(|_| CdpError::Timeout)?;
            } else if #[cfg(feature = "tokio-runtime")] {
                let debug_ws_url = tokio::time::timeout(dur, get_ws_url).await
            .map_err(|_| CdpError::Timeout)?;
            }
        }

        let conn = Connection::<CdpEventMessage>::connect(&debug_ws_url).await?;

        let (tx, rx) = channel(1);

        let handler_config = HandlerConfig {
            ignore_https_errors: config.ignore_https_errors,
            viewport: Some(config.viewport.clone()),
            context_ids: Vec::new(),
            request_timeout: config.request_timeout,
        };

        let fut = Handler::new(conn, rx, handler_config);
        let browser_context = fut.default_browser_context().clone();

        let browser = Self {
            sender: tx,
            config: Some(config),
            child: Some(child),
            debug_ws_url,
            browser_context,
        };

        Ok((browser, fut))
    }

    /// If not launched as incognito this creates a new incognito browser
    /// context. After that this browser exists within the incognito session.
    /// New pages created while being in incognito mode will also run in the
    /// incognito context. Incognito contexts won't share cookies/cache with
    /// other browser contexts.
    pub async fn start_incognito_context(&mut self) -> Result<&mut Self> {
        if !self.is_incognito_configured() {
            let resp = self
                .execute(CreateBrowserContextParams::default())
                .await?
                .result;
            self.browser_context = BrowserContext::from(resp.browser_context_id);
            self.sender
                .clone()
                .send(HandlerMessage::InsertContext(self.browser_context.clone()))
                .await?;
        }

        Ok(self)
    }

    /// If a incognito session was created with
    /// `Browser::start_incognito_context` this disposes this context.
    ///
    /// # Note This will also dispose all pages that were running within the
    /// incognito context.
    pub async fn quit_incognito_context(&mut self) -> Result<&mut Self> {
        if let Some(id) = self.browser_context.take() {
            self.execute(DisposeBrowserContextParams::new(id.clone()))
                .await?;
            self.sender
                .clone()
                .send(HandlerMessage::DisposeContext(BrowserContext::from(id)))
                .await?;
        }
        Ok(self)
    }

    /// Whether incognito mode was configured from the start
    fn is_incognito_configured(&self) -> bool {
        self.config
            .as_ref()
            .map(|c| c.incognito)
            .unwrap_or_default()
    }

    /// Returns the address of the websocket this browser is attached to
    pub fn websocket_address(&self) -> &String {
        &self.debug_ws_url
    }

    /// Whether the BrowserContext is incognito.
    pub fn is_incognito(&self) -> bool {
        self.is_incognito_configured() || self.browser_context.is_incognito()
    }

    /// The config of the spawned chromium instance if any.
    pub fn config(&self) -> Option<&BrowserConfig> {
        self.config.as_ref()
    }

    /// Create a new browser page
    pub async fn new_page(&self, params: impl Into<CreateTargetParams>) -> Result<Page> {
        let (tx, rx) = oneshot_channel();
        let mut params = params.into();
        if let Some(id) = self.browser_context.id() {
            if params.browser_context_id.is_none() {
                params.browser_context_id = Some(id.clone());
            }
        }

        self.sender
            .clone()
            .send(HandlerMessage::CreatePage(params, tx))
            .await?;

        rx.await?
    }

    /// Version information about the browser
    pub async fn version(&self) -> Result<GetVersionReturns> {
        Ok(self.execute(GetVersionParams::default()).await?.result)
    }

    /// Returns the user agent of the browser
    pub async fn user_agent(&self) -> Result<String> {
        Ok(self.version().await?.user_agent)
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

    /// Return page of given target_id
    pub async fn get_page(&self, target_id: TargetId) -> Result<Page> {
        let (tx, rx) = oneshot_channel();
        self.sender
            .clone()
            .send(HandlerMessage::GetPage(target_id, tx))
            .await?;
        rx.await?.ok_or(CdpError::NotFound)
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

    fn read_debug_url(stdout: std::process::ChildStderr) -> String {
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
    }
    cfg_if::cfg_if! {
        if #[cfg(feature = "async-std-runtime")] {
            async_std::task::spawn_blocking(|| read_debug_url(stdout)).await
        } else if #[cfg(feature = "tokio-runtime")] {
            tokio::task::spawn_blocking(move || read_debug_url(stdout)).await.expect("Failed to read debug url from process output")
        }
    }
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

    /// Whether to launch the `Browser` in incognito mode
    incognito: bool,

    /// Ignore https errors, default is true
    ignore_https_errors: bool,
    viewport: Viewport,
    /// The duration after a request with no response should time out
    request_timeout: Duration,

    /// Additional command line arguments to pass to the browser instance.
    args: Vec<String>,

    /// Whether to disable DEFAULT_ARGS or not, default is false
    disable_default_args: bool,
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
    incognito: bool,
    ignore_https_errors: bool,
    viewport: Viewport,
    request_timeout: Duration,
    args: Vec<String>,
    disable_default_args: bool,
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
            extensions: Vec::new(),
            process_envs: None,
            user_data_dir: None,
            incognito: false,
            ignore_https_errors: true,
            viewport: Default::default(),
            request_timeout: Duration::from_millis(REQUEST_TIMEOUT),
            args: Vec::new(),
            disable_default_args: false,
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

    pub fn incognito(mut self) -> Self {
        self.incognito = true;
        self
    }

    pub fn respect_https_errors(mut self) -> Self {
        self.ignore_https_errors = false;
        self
    }

    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    pub fn viewport(mut self, viewport: Viewport) -> Self {
        self.viewport = viewport;
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

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for arg in args {
            self.args.push(arg.into());
        }
        self
    }

    pub fn disable_default_args(mut self) -> Self {
        self.disable_default_args = true;
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
            process_envs: self.process_envs,
            user_data_dir: self.user_data_dir,
            incognito: self.incognito,
            ignore_https_errors: self.ignore_https_errors,
            viewport: self.viewport,
            request_timeout: self.request_timeout,
            args: self.args,
            disable_default_args: self.disable_default_args,
        })
    }
}

impl BrowserConfig {
    pub fn launch(&self) -> io::Result<Child> {
        let mut cmd = process::Command::new(&self.executable);

        if self.disable_default_args {
            cmd.args(&self.args);
        } else {
            cmd.args(&DEFAULT_ARGS).args(&self.args);
        }

        if !self
            .args
            .iter()
            .any(|arg| arg.contains("--remote-debugging-port="))
        {
            cmd.arg(format!("--remote-debugging-port={}", self.port));
        }

        cmd.args(
            self.extensions
                .iter()
                .map(|e| format!("--load-extension={}", e)),
        );

        if let Some(ref user_data) = self.user_data_dir {
            cmd.arg(format!("--user-data-dir={}", user_data.display()));
        } else {
            // If the user did not specify a data directory, this would default to the systems default
            // data directory. In most cases, we would rather have a fresh instance of Chromium. Specify
            // a temp dir just for chromiumoxide instead.
            cmd.arg(format!(
                "--user-data-dir={}",
                std::env::temp_dir().join("chromiumoxide-runner").display()
            ));
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

        if self.incognito {
            cmd.arg("--incognito");
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
        if let Some(path) = get_chrome_path_from_windows_registry() {
            if path.exists() {
                return Ok(path);
            }
        }
    }

    Err("Could not auto detect a chrome executable".to_string())
}

#[cfg(windows)]
pub(crate) fn get_chrome_path_from_windows_registry() -> Option<std::path::PathBuf> {
    winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
        .open_subkey("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\chrome.exe")
        .and_then(|key| key.get_value::<String, _>(""))
        .map(std::path::PathBuf::from)
        .ok()
}

/// These are passed to the Chrome binary by default.
/// Via https://github.com/puppeteer/puppeteer/blob/4846b8723cf20d3551c0d755df394cc5e0c82a94/src/node/Launcher.ts#L157
static DEFAULT_ARGS: [&str; 24] = [
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
