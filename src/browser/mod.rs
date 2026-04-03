use std::future::Future;
use std::io;

use futures::SinkExt;
use futures::channel::mpsc::{Sender, channel, unbounded};
use futures::channel::oneshot::channel as oneshot_channel;
use futures::select;

use chromiumoxide_cdp::cdp::browser_protocol::browser::{
    BrowserContextId, CloseReturns, GetVersionParams, GetVersionReturns,
};
use chromiumoxide_cdp::cdp::browser_protocol::network::{Cookie, CookieParam};
use chromiumoxide_cdp::cdp::browser_protocol::storage::{
    ClearCookiesParams, GetCookiesParams, SetCookiesParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::target::{
    CreateBrowserContextParams, CreateTargetParams, DisposeBrowserContextParams, TargetId,
    TargetInfo,
};
use chromiumoxide_cdp::cdp::{CdpEventMessage, IntoEventKind};
use chromiumoxide_types::*;

pub use self::config::{BrowserConfig, BrowserConfigBuilder, LAUNCH_TIMEOUT};
use crate::async_process::{Child, ExitStatus};
use crate::cmd::{CommandMessage, to_command_response};
use crate::conn::Connection;
use crate::error::{BrowserStderr, CdpError, Result};
use crate::handler::browser::BrowserContext;
use crate::handler::{Handler, HandlerConfig, HandlerMessage};
use crate::listeners::{EventListenerRequest, EventStream};
use crate::page::Page;
use crate::utils;

mod argument;
mod config;

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

/// Browser connection information.
#[derive(serde::Deserialize, Debug, Default)]
pub struct BrowserConnection {
    #[serde(rename = "Browser")]
    /// The browser name
    pub browser: String,
    #[serde(rename = "Protocol-Version")]
    /// Browser version
    pub protocol_version: String,
    #[serde(rename = "User-Agent")]
    /// User Agent used by default.
    pub user_agent: String,
    #[serde(rename = "V8-Version")]
    /// The v8 engine version
    pub v8_version: String,
    #[serde(rename = "WebKit-Version")]
    /// Webkit version
    pub webkit_version: String,
    #[serde(rename = "webSocketDebuggerUrl")]
    /// Remote debugging address
    pub web_socket_debugger_url: String,
}

impl Browser {
    /// Connect to an already running chromium instance via the given URL.
    ///
    /// If the URL is a http(s) URL, it will first attempt to retrieve the Websocket URL from the `json/version` endpoint.
    pub async fn connect(url: impl Into<String>) -> Result<(Self, Handler)> {
        Self::connect_with_config(url, HandlerConfig::default()).await
    }

    // Connect to an already running chromium instance with a given `HandlerConfig`.
    ///
    /// If the URL is a http URL, it will first attempt to retrieve the Websocket URL from the `json/version` endpoint.
    pub async fn connect_with_config(
        url: impl Into<String>,
        config: HandlerConfig,
    ) -> Result<(Self, Handler)> {
        let mut debug_ws_url = url.into();

        if debug_ws_url.starts_with("http") {
            match reqwest::Client::new()
                .get(
                    if debug_ws_url.ends_with("/json/version")
                        || debug_ws_url.ends_with("/json/version/")
                    {
                        debug_ws_url.clone()
                    } else {
                        format!(
                            "{}{}json/version",
                            &debug_ws_url,
                            if debug_ws_url.ends_with('/') { "" } else { "/" }
                        )
                    },
                )
                .header("content-type", "application/json")
                .send()
                .await
            {
                Ok(req) => {
                    let socketaddr = req.remote_addr().unwrap();
                    let connection: BrowserConnection =
                        serde_json::from_slice(&req.bytes().await.unwrap_or_default())
                            .unwrap_or_default();

                    if !connection.web_socket_debugger_url.is_empty() {
                        // prevent proxy interfaces from returning local ips to connect to the exact machine
                        debug_ws_url = connection
                            .web_socket_debugger_url
                            .replace("127.0.0.1", &socketaddr.ip().to_string());
                    }
                }
                Err(_) => return Err(CdpError::NoResponse),
            }
        }

        let conn = Connection::<CdpEventMessage>::connect(&debug_ws_url).await?;

        let (tx, rx) = channel(1);

        let fut = Handler::new(conn, rx, config);
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
    /// processes stderr for more than the configured `launch_timeout`
    /// (20 seconds by default).
    pub async fn launch(mut config: BrowserConfig) -> Result<(Self, Handler)> {
        // Canonalize paths to reduce issues with sandboxing
        config.executable = utils::canonicalize_except_snap(config.executable).await?;

        // Launch a new chromium instance
        let mut child = config.launch()?;

        /// Faillible initialization to run once the child process is created.
        ///
        /// All faillible calls must be executed inside this function. This ensures that all
        /// errors are caught and that the child process is properly cleaned-up.
        async fn with_child(
            config: &BrowserConfig,
            child: &mut Child,
        ) -> Result<(String, Connection<CdpEventMessage>)> {
            let dur = config.launch_timeout;
            let timeout_fut = Box::pin(tokio::time::sleep(dur));

            // extract the ws:
            let debug_ws_url = ws_url_from_output(child, timeout_fut).await?;
            let conn = Connection::<CdpEventMessage>::connect(&debug_ws_url).await?;
            Ok((debug_ws_url, conn))
        }

        let (debug_ws_url, conn) = match with_child(&config, &mut child).await {
            Ok(conn) => conn,
            Err(e) => {
                // An initialization error occurred, clean up the process
                if let Ok(Some(_)) = child.try_wait() {
                    // already exited, do nothing, may happen if the browser crashed
                } else {
                    // the process is still alive, kill it and wait for exit (avoid zombie processes)
                    child.kill().await.expect("`Browser::launch` failed but could not clean-up the child process (`kill`)");
                    child.wait().await.expect("`Browser::launch` failed but could not clean-up the child process (`wait`)");
                }
                return Err(e);
            }
        };

        // Only infaillible calls are allowed after this point to avoid clean-up issues with the
        // child process.

        let (tx, rx) = channel(1);

        let handler_config = HandlerConfig {
            ignore_https_errors: config.ignore_https_errors,
            ignore_invalid_messages: config.ignore_invalid_messages,
            viewport: config.viewport.clone(),
            context_ids: Vec::new(),
            request_timeout: config.request_timeout,
            request_intercept: config.request_intercept,
            cache_enabled: config.cache_enabled,
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

    /// Request to fetch all existing browser targets.
    ///
    /// By default, only targets launched after the browser connection are tracked
    /// when connecting to a existing browser instance with the devtools websocket url
    /// This function fetches existing targets on the browser and adds them as pages internally
    ///
    /// The pages are not guaranteed to be ready as soon as the function returns
    /// You should wait a few millis if you need to use a page
    /// Returns [TargetInfo]
    pub async fn fetch_targets(&mut self) -> Result<Vec<TargetInfo>> {
        let (tx, rx) = oneshot_channel();

        self.sender
            .clone()
            .send(HandlerMessage::FetchTargets(tx))
            .await?;

        rx.await?
    }

    /// Request for the browser to close completely.
    ///
    /// If the browser was spawned by [`Browser::launch`], it is recommended to wait for the
    /// spawned instance exit, to avoid "zombie" processes ([`Browser::wait`],
    /// [`Browser::wait_sync`], [`Browser::try_wait`]).
    /// [`Browser::drop`] waits automatically if needed.
    pub async fn close(&mut self) -> Result<CloseReturns> {
        let (tx, rx) = oneshot_channel();

        self.sender
            .clone()
            .send(HandlerMessage::CloseBrowser(tx))
            .await?;

        rx.await?
    }

    /// Asynchronously wait for the spawned chromium instance to exit completely.
    ///
    /// The instance is spawned by [`Browser::launch`]. `wait` is usually called after
    /// [`Browser::close`]. You can call this explicitly to collect the process and avoid
    /// "zombie" processes.
    ///
    /// This call has no effect if this [`Browser`] did not spawn any chromium instance (e.g.
    /// connected to an existing browser through [`Browser::connect`])
    pub async fn wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(child) = self.child.as_mut() {
            Ok(Some(child.wait().await?))
        } else {
            Ok(None)
        }
    }

    /// If the spawned chromium instance has completely exited, wait for it.
    ///
    /// The instance is spawned by [`Browser::launch`]. `try_wait` is usually called after
    /// [`Browser::close`]. You can call this explicitly to collect the process and avoid
    /// "zombie" processes.
    ///
    /// This call has no effect if this [`Browser`] did not spawn any chromium instance (e.g.
    /// connected to an existing browser through [`Browser::connect`])
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(child) = self.child.as_mut() {
            child.try_wait()
        } else {
            Ok(None)
        }
    }

    /// Get the spawned chromium instance
    ///
    /// The instance is spawned by [`Browser::launch`]. The result is a [`async_process::Child`]
    /// value.
    ///A
    /// You may use [`async_process::Child::as_mut_inner`] to retrieve the concrete implementation
    /// for the selected runtime.
    ///
    /// This call has no effect if this [`Browser`] did not spawn any chromium instance (e.g.
    /// connected to an existing browser through [`Browser::connect`])
    pub fn get_mut_child(&mut self) -> Option<&mut Child> {
        self.child.as_mut()
    }

    /// Forcibly kill the spawned chromium instance
    ///
    /// The instance is spawned by [`Browser::launch`]. `kill` will automatically wait for the child
    /// process to exit to avoid "zombie" processes.
    ///
    /// This method is provided to help if the browser does not close by itself. You should prefer
    /// to use [`Browser::close`].
    ///
    /// This call has no effect if this [`Browser`] did not spawn any chromium instance (e.g.
    /// connected to an existing browser through [`Browser::connect`])
    pub async fn kill(&mut self) -> Option<io::Result<()>> {
        match self.child.as_mut() {
            Some(child) => Some(child.kill().await),
            None => None,
        }
    }

    /// If not launched as incognito this creates a new incognito browser
    /// context. After that this browser exists within the incognito session.
    /// New pages created while being in incognito mode will also run in the
    /// incognito context. Incognito contexts won't share cookies/cache with
    /// other browser contexts.
    pub async fn start_incognito_context(&mut self) -> Result<&mut Self> {
        if !self.is_incognito_configured() {
            let browser_context_id = self
                .create_browser_context_inner(CreateBrowserContextParams::default())
                .await?;
            self.browser_context = BrowserContext::from(browser_context_id);
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
            self.dispose_browser_context_inner(id.clone()).await?;
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

    /// Set listener for browser event
    pub async fn event_listener<T: IntoEventKind>(&self) -> Result<EventStream<T>> {
        let (tx, rx) = unbounded();
        self.sender
            .clone()
            .send(HandlerMessage::AddEventListener(
                EventListenerRequest::new::<T>(tx),
            ))
            .await?;

        Ok(EventStream::new(rx))
    }

    /// Creates a new empty browser context.
    pub async fn create_browser_context(
        &self,
        params: CreateBrowserContextParams,
    ) -> Result<BrowserContextId> {
        let browser_context_id = self.create_browser_context_inner(params).await?;
        let browser_context = BrowserContext::from(browser_context_id.clone());
        self.sender
            .clone()
            .send(HandlerMessage::InsertContext(browser_context))
            .await?;
        Ok(browser_context_id)
    }

    async fn create_browser_context_inner(
        &self,
        params: CreateBrowserContextParams,
    ) -> Result<BrowserContextId> {
        let response = self.execute(params).await?;
        Ok(response.result.browser_context_id)
    }

    /// Deletes a browser context.
    pub async fn dispose_browser_context(
        &self,
        browser_context_id: impl Into<BrowserContextId>,
    ) -> Result<()> {
        let browser_context_id = browser_context_id.into();
        self.dispose_browser_context_inner(browser_context_id.clone())
            .await?;
        self.sender
            .clone()
            .send(HandlerMessage::DisposeContext(BrowserContext::from(
                browser_context_id,
            )))
            .await?;
        Ok(())
    }

    async fn dispose_browser_context_inner(
        &self,
        browser_context_id: impl Into<BrowserContextId>,
    ) -> Result<()> {
        self.execute(DisposeBrowserContextParams::new(browser_context_id))
            .await?;
        Ok(())
    }

    /// Clears cookies.
    pub async fn clear_cookies(&self) -> Result<()> {
        self.execute(ClearCookiesParams::default()).await?;
        Ok(())
    }

    /// Returns all browser cookies.
    pub async fn get_cookies(&self) -> Result<Vec<Cookie>> {
        Ok(self
            .execute(GetCookiesParams::default())
            .await?
            .result
            .cookies)
    }

    /// Sets given cookies.
    pub async fn set_cookies(&self, mut cookies: Vec<CookieParam>) -> Result<&Self> {
        for cookie in &mut cookies {
            if let Some(url) = cookie.url.as_ref() {
                crate::page::validate_cookie_url(url)?;
            }
        }

        self.execute(SetCookiesParams::new(cookies)).await?;
        Ok(self)
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if let Ok(Some(_)) = child.try_wait() {
                // Already exited, do nothing. Usually occurs after using the method close or kill.
            } else {
                // We set the `kill_on_drop` property for the child process, so no need to explicitely
                // kill it here. It can't really be done anyway since the method is async.
                //
                // On Unix, the process will be reaped in the background by the runtime automatically
                // so it won't leave any resources locked. It is, however, a better practice for the user to
                // do it himself since the runtime doesn't provide garantees as to when the reap occurs, so we
                // warn him here.
                tracing::warn!(
                    "Browser was not closed manually, it will be killed automatically in the background"
                );
            }
        }
    }
}

/// Resolve devtools WebSocket URL from the provided browser process
///
/// If an error occurs, it returns the browser's stderr output.
///
/// The URL resolution fails if:
/// - [`CdpError::LaunchTimeout`]: `timeout_fut` completes, this corresponds to a timeout
/// - [`CdpError::LaunchExit`]: the browser process exits (or is killed)
/// - [`CdpError::LaunchIo`]: an input/output error occurs when await the process exit or reading
///   the browser's stderr: end of stream, invalid UTF-8, other
async fn ws_url_from_output(
    child_process: &mut Child,
    timeout_fut: impl Future<Output = ()> + Unpin,
) -> Result<String> {
    use futures::{AsyncBufReadExt, FutureExt};
    let mut timeout_fut = timeout_fut.fuse();
    let stderr = child_process.stderr.take().expect("no stderror");
    let mut stderr_bytes = Vec::<u8>::new();
    let mut exit_status_fut = Box::pin(child_process.wait()).fuse();
    let mut buf = futures::io::BufReader::new(stderr);
    loop {
        select! {
            _ = timeout_fut => return Err(CdpError::LaunchTimeout(BrowserStderr::new(stderr_bytes))),
            exit_status = exit_status_fut => {
                return Err(match exit_status {
                    Err(e) => CdpError::LaunchIo(e, BrowserStderr::new(stderr_bytes)),
                    Ok(exit_status) => CdpError::LaunchExit(exit_status, BrowserStderr::new(stderr_bytes)),
                })
            },
            read_res = buf.read_until(b'\n', &mut stderr_bytes).fuse() => {
                match read_res {
                    Err(e) => return Err(CdpError::LaunchIo(e, BrowserStderr::new(stderr_bytes))),
                    Ok(byte_count) => {
                        if byte_count == 0 {
                            let e = io::Error::new(io::ErrorKind::UnexpectedEof, "unexpected end of stream");
                            return Err(CdpError::LaunchIo(e, BrowserStderr::new(stderr_bytes)));
                        }
                        let start_offset = stderr_bytes.len() - byte_count;
                        let new_bytes = &stderr_bytes[start_offset..];
                        match std::str::from_utf8(new_bytes) {
                            Err(_) => {
                                let e = io::Error::new(io::ErrorKind::InvalidData, "stream did not contain valid UTF-8");
                                return Err(CdpError::LaunchIo(e, BrowserStderr::new(stderr_bytes)));
                            }
                            Ok(line) => {
                                if let Some((_, ws)) = line.rsplit_once("listening on ") {
                                    if ws.starts_with("ws") && ws.contains("devtools/browser") {
                                        return Ok(ws.trim().to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
