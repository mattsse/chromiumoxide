use serde::Serialize;
use std::borrow::Cow;
use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::channel::oneshot::{channel as oneshot_channel, Sender as OneshotSender};

use chromeoxid_types::*;

use crate::cdp::browser_protocol::target::{CreateTargetParams, SessionId};
use crate::conn::Connection;
use crate::context::CdpFuture;
use crate::tab::Tab;
use futures::SinkExt;
use std::sync::Arc;

// Browser produces all events and communicates over sender/receiver with tabs,
// submitting a method to a browser returns a oneshot receiver for the response
// and a unbounded receiver for the events that can be tracked to the request
pub struct Browser {
    tabs: Vec<Arc<Tab>>,
    sender: Sender<BrowserMessage>,
}

impl Browser {
    pub async fn connect(debug_ws_url: &str) -> Result<(Self, CdpFuture)> {
        let conn = Connection::<CdpEvent>::connect(debug_ws_url).await?;

        let (tx, rx) = channel(1);

        let fut = CdpFuture::new(conn, rx);
        let browser = Self {
            tabs: vec![],
            sender: tx,
        };
        Ok((browser, fut))
    }

    pub async fn new_tab(&self, params: impl Into<CreateTargetParams>) -> anyhow::Result<Tab> {
        let params = params.into();
        let resp = self.execute(params).await?;
        let target_id = resp.result.target_id;
        let (commands, from_commands) = channel(1);

        self.sender
            .clone()
            .send(BrowserMessage::RegisterTab(from_commands))
            .await?;
        Ok(Tab::new(target_id, commands).await?)
    }

    pub async fn new_blank_tab(&self) -> anyhow::Result<Tab> {
        Ok(self.new_tab(CreateTargetParams::new("about:blank")).await?)
    }

    pub async fn execute<T: Command>(
        &self,
        cmd: T,
    ) -> anyhow::Result<CommandResponse<T::Response>> {
        let (tx, rx) = oneshot_channel();
        let method = cmd.identifier();
        let msg = CommandMessage::new(cmd, tx)?;

        self.sender
            .clone()
            .send(BrowserMessage::Command(msg))
            .await?;
        let resp = rx.await?;

        if let Some(res) = resp.result {
            let result = serde_json::from_value(res)?;
            Ok(CommandResponse {
                id: resp.id,
                result,
                method,
            })
        } else if let Some(err) = resp.error {
            Err(err.into())
        } else {
            Err(anyhow::anyhow!("Empty Response"))
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        // TODO
    }
}

/// Messages used internally to communicate with the connection, which is
/// executed in the the background task.
#[derive(Debug, Serialize)]
pub(crate) struct CommandMessage {
    pub method: Cow<'static, str>,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    pub params: serde_json::Value,
    #[serde(skip_serializing)]
    pub sender: OneshotSender<Response>,
}

impl CommandMessage {
    pub fn new<C: Command>(cmd: C, sender: OneshotSender<Response>) -> serde_json::Result<Self> {
        Ok(Self {
            method: cmd.identifier(),
            session_id: None,
            params: serde_json::to_value(cmd)?,
            sender,
        })
    }

    pub fn with_session<C: Command>(
        cmd: C,
        sender: OneshotSender<Response>,
        session_id: Option<SessionId>,
    ) -> serde_json::Result<Self> {
        Ok(Self {
            method: cmd.identifier(),
            session_id,
            params: serde_json::to_value(cmd)?,
            sender,
        })
    }
}

impl Method for CommandMessage {
    fn identifier(&self) -> Cow<'static, str> {
        self.method.clone()
    }
}

pub(crate) enum BrowserMessage {
    Command(CommandMessage),
    RegisterTab(Receiver<CommandMessage>),
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
    port: Option<u16>,
    /// Path for Chrome or Chromium.
    ///
    /// If unspecified, the create will try to automatically detect a suitable
    /// binary.
    path: Option<std::path::PathBuf>,

    /// A list of Chrome extensions to load.
    ///
    /// An extension should be a path to a folder containing the extension code.
    /// CRX files cannot be used directly and must be first extracted.
    ///
    /// Note that Chrome does not support loading extensions in headless-mode.
    /// See https://bugs.chromium.org/p/chromium/issues/detail?id=706008#c5
    extensions: Vec<String>,

    // /// The options to use for fetching a version of chrome when `path` is None.
    // ///
    // /// By default, we'll use a revision guaranteed to work with our API and will
    // /// download and install that revision of chrome the first time a Process is created.
    // #[cfg(feature = "fetch")]
    // #[builder(default)]
    // fetcher_options: FetcherOptions,
    /// How long to keep the WebSocket to the browser for after not receiving
    /// any events from it Defaults to 30 seconds
    pub idle_browser_timeout: Duration,

    /// Environment variables to set for the Chromium process.
    /// Passes value through to std::process::Command::envs.
    pub process_envs: Option<HashMap<String, String>>,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            headless: true,
            sandbox: true,
            window_size: None,
            port: None,
            path: None,
            extensions: vec![],
            idle_browser_timeout: Duration::from_secs(300),
            process_envs: None,
        }
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
