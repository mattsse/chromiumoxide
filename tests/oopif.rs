use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use chromiumoxide::auth::Credentials;
use chromiumoxide::cdp::browser_protocol::fetch::ContinueRequestParams;
use chromiumoxide::cdp::browser_protocol::network::ErrorReason;
use chromiumoxide::cdp::browser_protocol::page::{FrameId, GetFrameTreeParams};
use chromiumoxide::error::CdpError;
use chromiumoxide::{BrowserConfig, Element, Frame, Page};
use futures::StreamExt;

use crate::test_config;

const FRAME_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_PARALLEL_OOPIF_BROWSERS: usize = 2;

static OOPIF_BROWSER_SLOTS: LazyLock<(Mutex<usize>, Condvar)> =
    LazyLock::new(|| (Mutex::new(0), Condvar::new()));

struct OopifBrowserSlot;

impl OopifBrowserSlot {
    fn acquire() -> Self {
        let (active, available) = &*OOPIF_BROWSER_SLOTS;
        let mut active = active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while *active >= MAX_PARALLEL_OOPIF_BROWSERS {
            active = available
                .wait(active)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *active += 1;
        Self
    }
}

impl Drop for OopifBrowserSlot {
    fn drop(&mut self) {
        let (active, available) = &*OOPIF_BROWSER_SLOTS;
        let mut active = active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *active -= 1;
        available.notify_one();
    }
}

struct OopifProfile {
    path: PathBuf,
}

impl OopifProfile {
    fn create(port: u16) -> std::io::Result<Self> {
        let path =
            std::env::temp_dir().join(format!("chromiumoxide-oopif-{}-{port}", std::process::id()));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for OopifProfile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn oopif_config(profile: &OopifProfile) -> BrowserConfig {
    BrowserConfig::builder()
        .new_headless_mode()
        .disable_https_first()
        .request_timeout(FRAME_TIMEOUT)
        .user_data_dir(profile.path())
        .args(["--site-per-process", "--no-proxy-server"])
        .build()
        .expect("browser config is valid")
}

struct OopifServer {
    _browser_slot: OopifBrowserSlot,
    port: u16,
    stop: Arc<AtomicBool>,
    nested_served: Arc<AtomicBool>,
    requests: Arc<Mutex<HashMap<(String, String), usize>>>,
    thread: Option<JoinHandle<()>>,
}

impl OopifServer {
    fn start() -> std::io::Result<Self> {
        let browser_slot = OopifBrowserSlot::acquire();
        let listener = TcpListener::bind("0.0.0.0:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let nested_served = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(HashMap::new()));
        let thread_stop = Arc::clone(&stop);
        let thread_nested_served = Arc::clone(&nested_served);
        let thread_requests = Arc::clone(&requests);
        let thread = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        serve_connection(stream, port, &thread_nested_served, &thread_requests);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            _browser_slot: browser_slot,
            port,
            stop,
            nested_served,
            requests,
            thread: Some(thread),
        })
    }

    fn url(&self, host: &str, path: &str) -> String {
        format!("http://{host}:{}{path}", self.port)
    }

    fn nested_served(&self) -> bool {
        self.nested_served.load(Ordering::Acquire)
    }

    fn request_count(&self, host: &str, path: &str) -> usize {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&(host.to_owned(), path.to_owned()))
            .copied()
            .unwrap_or_default()
    }
}

impl Drop for OopifServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve_connection(
    mut stream: TcpStream,
    port: u16,
    nested_served: &AtomicBool,
    requests: &Mutex<HashMap<(String, String), usize>>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    while request.len() < 16 * 1024 {
        let Ok(read) = stream.read(&mut buffer) else {
            return;
        };
        if read == 0 {
            return;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8_lossy(&request);
    let mut lines = request.lines();
    let path = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|target| target.split('?').next())
        .unwrap_or("/");
    let mut host = "";
    let mut authorization = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("host") {
            host = value.trim().split(':').next().unwrap_or_default();
        } else if name.eq_ignore_ascii_case("authorization") {
            authorization = Some(value.trim());
        }
    }
    *requests
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry((host.to_owned(), path.to_owned()))
        .or_default() += 1;

    let authorized = authorization == Some("Basic dXNlcjpwYXNz");
    let (status, body, delayed_nested) = route(host, path, port, authorized);
    if delayed_nested {
        std::thread::sleep(Duration::from_millis(150));
    }
    let challenge = if status == "401 Unauthorized" {
        "WWW-Authenticate: Basic realm=\"chromiumoxide\"\r\n"
    } else {
        ""
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n{challenge}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    if stream.write_all(response.as_bytes()).is_ok() && delayed_nested {
        nested_served.store(true, Ordering::Release);
    }
}

fn route(host: &str, path: &str, port: u16, authorized: bool) -> (&'static str, String, bool) {
    match (host, path) {
        ("localhost", "/main") => (
            "200 OK",
            format!(
                r#"<!doctype html><style>
                html, body {{ margin: 0; width: 100%; height: 100%; }}
                #oop {{ position: absolute; left: 180px; top: 100px; width: 500px; height: 300px;
                        border: 9px solid rgb(20, 30, 40); padding: 6px; }}
                #swap {{ position: absolute; left: 10px; top: 470px; width: 120px; height: 80px; }}
                </style><body data-role="main">
                <iframe id="oop" src="http://127.0.0.1:{port}/child"></iframe>
                <iframe id="swap" src="http://localhost:{port}/same"></iframe>
                </body>"#
            ),
            false,
        ),
        ("localhost", "/same") => ("200 OK", page("same"), false),
        ("localhost", "/same-waited") => ("200 OK", page("same-waited"), false),
        ("localhost", "/same-return") => ("200 OK", page("same-return"), false),
        ("localhost", "/auth-main") => (
            "200 OK",
            format!(
                r#"<!doctype html><body data-role="auth-main">
                <iframe id="auth" src="http://127.0.0.1:{port}/auth-child"></iframe>
                </body>"#
            ),
            false,
        ),
        ("127.0.0.1", "/child") => (
            "200 OK",
            format!(
                r#"<!doctype html><style>
                html, body {{ margin: 0; width: 100%; height: 100%; }}
                button {{ box-sizing: border-box; }}
                #child-button {{ position: absolute; left: 20px; top: 20px; width: 100px; height: 40px; }}
                #container {{ position: absolute; left: 20px; top: 75px; }}
                #same-descendant {{ position: absolute; left: 20px; top: 130px; width: 180px; height: 120px;
                                    border: 7px solid rgb(50, 60, 70); padding: 3px; }}
                #nested {{ position: absolute; left: 250px; top: 130px; width: 180px; height: 120px;
                           border: 8px solid rgb(80, 90, 100); padding: 4px; }}
                </style><body data-role="oop-child">
                <button id="child-button" onclick="document.body.dataset.clicked='child'">child</button>
                <div id="container"><span class="item">first</span><span class="item">second</span></div>
                <iframe id="same-descendant" src="http://127.0.0.1:{port}/same-descendant"></iframe>
                <iframe id="nested" src="http://127.0.0.2:{port}/nested"></iframe>
                </body>"#
            ),
            false,
        ),
        ("127.0.0.1", "/same-descendant") => (
            "200 OK",
            interactive_page("same-descendant", "same", 15, 12),
            false,
        ),
        ("127.0.0.1", "/swap-cross") => ("200 OK", page("swap-cross"), false),
        ("127.0.0.1", "/child-nav") => ("200 OK", page("child-nav"), false),
        ("127.0.0.1", "/child-nav-2") => ("200 OK", page("child-nav-2"), false),
        ("127.0.0.1", "/child-abort") => ("200 OK", page("child-abort"), false),
        ("127.0.0.1", "/http-error") => ("404 Not Found", page("http-error"), false),
        ("127.0.0.1", "/nested-return") => ("200 OK", page("nested-return"), false),
        ("127.0.0.1", "/auth-child") if authorized => ("200 OK", page("auth-child"), false),
        ("127.0.0.1", "/auth-child") => ("401 Unauthorized", page("auth-required"), false),
        ("127.0.0.2", "/nested") => (
            "200 OK",
            interactive_page("nested-oop", "nested", 18, 14),
            true,
        ),
        _ => ("404 Not Found", page("not-found"), false),
    }
}

fn page(role: &str) -> String {
    format!(r#"<!doctype html><body data-role="{role}">{role}</body>"#)
}

fn interactive_page(role: &str, clicked: &str, left: u32, top: u32) -> String {
    format!(
        r#"<!doctype html><style>
        html, body {{ margin: 0; width: 100%; height: 100%; }}
        #target {{ position: absolute; left: {left}px; top: {top}px; width: 90px; height: 36px;
                   box-sizing: border-box; }}
        </style><body data-role="{role}">
        <button id="target"
                onmouseover="document.body.dataset.hovered='{clicked}'"
                onclick="document.body.dataset.clicked='{clicked}'">{role}</button>
        </body>"#
    )
}

async fn wait_for_frame(
    page: &Page,
    description: &str,
    predicate: impl Fn(&Frame) -> bool,
) -> Frame {
    tokio::time::timeout(FRAME_TIMEOUT, async {
        loop {
            if let Some(frame) = page
                .all_frames()
                .await
                .expect("frame enumeration succeeds")
                .into_iter()
                .find(|frame| predicate(frame))
            {
                return frame;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {description}"))
}

async fn wait_for_url(page: &Page, path: &str) -> Frame {
    wait_for_frame(page, path, |frame| {
        frame.url().is_some_and(|url| {
            url::Url::parse(&url)
                .map(|url| url.path() == path)
                .unwrap_or(false)
        })
    })
    .await
}

async fn wait_for_binding(
    page: &Page,
    frame_id: &FrameId,
    description: &str,
    predicate: impl Fn(&Frame) -> bool,
) -> Frame {
    wait_for_frame(page, description, |frame| {
        frame.id() == frame_id && predicate(frame)
    })
    .await
}

async fn wait_for_gone(page: &Page, frame_id: &FrameId) {
    tokio::time::timeout(FRAME_TIMEOUT, async {
        loop {
            match page.frame_by_id(frame_id.clone()).await {
                Ok(None) => return,
                Ok(Some(_)) | Err(CdpError::FrameNotReady) => {}
                Err(error) => panic!("frame lookup failed: {error}"),
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("timed out waiting for frame removal");
}

async fn eval_string(frame: &Frame, expression: &str) -> String {
    tokio::time::timeout(FRAME_TIMEOUT, async {
        loop {
            match frame.eval(expression.to_owned()).await {
                Ok(result) => {
                    return result
                        .into_value::<String>()
                        .expect("evaluation returns a string");
                }
                Err(CdpError::FrameNotReady) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => panic!("frame evaluation failed: {error}"),
            }
        }
    })
    .await
    .expect("timed out waiting for a frame execution context")
}

async fn query_element(frame: &Frame, selector: &str) -> Element {
    tokio::time::timeout(FRAME_TIMEOUT, async {
        loop {
            match frame.query_selector(selector).await {
                Ok(Some(element)) => return element,
                Ok(None) => panic!("selector {selector:?} did not match"),
                Err(CdpError::FrameNotReady) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => panic!("frame selector failed: {error}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for selector {selector:?}"))
}

fn assert_near(actual: f64, expected: f64, label: &str) {
    assert!(
        (actual - expected).abs() < 1.0,
        "{label}: expected {expected}, got {actual}"
    );
}

async fn set_iframe_src(page: &Page, id: &str, url: &str) {
    let id = serde_json::to_string(id).expect("iframe id serializes");
    let url = serde_json::to_string(url).expect("URL serializes");
    page.evaluate(format!("document.getElementById({id}).src = {url}; true"))
        .await
        .expect("iframe navigation script succeeds");
}

async fn assert_stale(frame: &Frame) {
    assert!(matches!(
        frame.eval("1 + 1").await,
        Err(CdpError::FrameNotReady)
    ));
    assert!(matches!(
        frame.query_selector("body").await,
        Err(CdpError::FrameNotReady)
    ));
    assert!(matches!(
        frame.execute(GetFrameTreeParams::default()).await,
        Err(CdpError::FrameNotReady)
    ));
    assert!(matches!(
        frame.goto("about:blank").await,
        Err(CdpError::FrameNotReady)
    ));
    assert!(matches!(
        frame.wait_for_navigation().await,
        Err(CdpError::FrameNotReady)
    ));
}

#[tokio::test]
async fn preload_scripts_replay_once_before_and_after_oopif_attach() {
    let server = OopifServer::start().expect("local OOPIF server starts");
    let profile = OopifProfile::create(server.port).expect("isolated Chrome profile is created");
    let config = oopif_config(&profile);

    test_config(config, async |browser| {
        let page = browser
            .new_page("about:blank")
            .await
            .expect("page creation succeeds");
        page.evaluate_on_new_document(
            "globalThis.__earlyPreloadRuns = (globalThis.__earlyPreloadRuns || 0) + 1;",
        )
        .await
        .expect("pre-attach script registration succeeds");

        page.goto(server.url("localhost", "/main"))
            .await
            .expect("main navigation succeeds");
        let child = wait_for_url(&page, "/child").await;
        assert!(child.is_out_of_process());
        assert_eq!(
            eval_string(&child, "String(globalThis.__earlyPreloadRuns)").await,
            "1"
        );

        page.add_init_script(
            "globalThis.__latePreloadRuns = (globalThis.__latePreloadRuns || 0) + 1;",
        )
        .await
        .expect("post-attach script registration reaches the existing child");
        assert_eq!(
            eval_string(&child, "String(globalThis.__latePreloadRuns)").await,
            "undefined",
            "the public API preserves Chrome's non-immediate default"
        );

        child
            .goto(server.url("127.0.0.1", "/child-nav"))
            .await
            .expect("first child navigation succeeds");
        assert_eq!(
            eval_string(&child, "String(globalThis.__earlyPreloadRuns)").await,
            "1",
            "the snapshot registration is not duplicated by later fan-out"
        );
        assert_eq!(
            eval_string(&child, "String(globalThis.__latePreloadRuns)").await,
            "1",
            "the post-attach registration runs on the next child document"
        );

        child
            .goto(server.url("127.0.0.1", "/child-nav-2"))
            .await
            .expect("second child navigation succeeds");
        assert_eq!(
            eval_string(&child, "String(globalThis.__earlyPreloadRuns)").await,
            "1"
        );
        assert_eq!(
            eval_string(&child, "String(globalThis.__latePreloadRuns)").await,
            "1"
        );
        assert_eq!(server.request_count("127.0.0.1", "/child"), 1);
        assert_eq!(server.request_count("127.0.0.1", "/child-nav"), 1);
        assert_eq!(server.request_count("127.0.0.1", "/child-nav-2"), 1);
    })
    .await;
}

#[tokio::test]
async fn interception_uses_captured_oop_session_and_child_fallback() {
    let server = OopifServer::start().expect("local OOPIF server starts");
    let profile = OopifProfile::create(server.port).expect("isolated Chrome profile is created");
    let config = oopif_config(&profile);

    test_config(config, async |browser| {
        let page = browser
            .new_page("about:blank")
            .await
            .expect("page creation succeeds");
        let mut paused_requests = page
            .paused_requests()
            .await
            .expect("managed pause stream registers before Fetch enable");
        page.set_request_interception(true)
            .await
            .expect("Fetch enable is response-confirmed");

        let main_url = server.url("localhost", "/main");
        let mut navigation = Box::pin(page.goto(main_url.clone()));
        let mut saw_main_document = false;
        loop {
            tokio::select! {
                result = &mut navigation => {
                    result.expect("main navigation succeeds after managed responses");
                    break;
                }
                paused = paused_requests.next() => {
                    let paused = paused.expect("pause stream remains live");
                    if paused.event().request.url == main_url {
                        saw_main_document = true;
                    }
                    paused
                        .continue_request()
                        .await
                        .expect("captured-session continuation succeeds");
                }
            }
        }
        assert!(
            saw_main_document,
            "the first awaited main navigation was intercepted"
        );

        let child = wait_for_url(&page, "/child").await;
        assert!(child.is_out_of_process());
        page.set_request_interception(false)
            .await
            .expect("dynamic Fetch disable is response-confirmed");
        page.set_request_interception(true)
            .await
            .expect("dynamic Fetch enable reaches the existing Done child");
        let child_url = server.url("127.0.0.1", "/child-nav");
        let mut child_navigation = Box::pin(child.goto(child_url.clone()));
        let paused_child = loop {
            tokio::select! {
                result = &mut child_navigation => {
                    panic!("child navigation completed before its pause was answered: {result:?}");
                }
                paused = paused_requests.next() => {
                    let paused = paused.expect("pause stream remains live");
                    if paused.event().request.url == child_url {
                        break paused;
                    }
                    paused
                        .continue_request()
                        .await
                        .expect("unrelated request is released");
                }
            }
        };

        let request_id = paused_child.event().request_id.clone();
        let _raw_main_response = page.execute(ContinueRequestParams::new(request_id)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(250), child_navigation.as_mut())
                .await
                .is_err(),
            "a main-session raw response must not release an OOP child request"
        );
        paused_child
            .continue_request()
            .await
            .expect("the captured child-session response releases the request");
        tokio::time::timeout(FRAME_TIMEOUT, child_navigation)
            .await
            .expect("child navigation stays bounded")
            .expect("child navigation succeeds");

        let aborted_url = server.url("127.0.0.1", "/child-abort");
        let mut aborted_navigation = Box::pin(child.goto(aborted_url.clone()));
        let aborted_pause = loop {
            tokio::select! {
                result = &mut aborted_navigation => {
                    panic!("child abort navigation completed before pause delivery: {result:?}");
                }
                paused = paused_requests.next() => {
                    let paused = paused.expect("pause stream remains live");
                    if paused.event().request.url == aborted_url {
                        break paused;
                    }
                    paused
                        .continue_request()
                        .await
                        .expect("unrelated request is released");
                }
            }
        };
        aborted_pause
            .fail(ErrorReason::Aborted)
            .await
            .expect("captured child-session failure succeeds");
        assert!(
            tokio::time::timeout(FRAME_TIMEOUT, aborted_navigation)
                .await
                .expect("aborted child navigation settles promptly")
                .is_err()
        );

        drop(paused_requests);
        tokio::time::timeout(
            FRAME_TIMEOUT,
            child.goto(server.url("127.0.0.1", "/child-nav-2")),
        )
        .await
        .expect("closed-stream child fallback stays bounded")
        .expect("closed-stream child pause auto-continues");
    })
    .await;
}

#[tokio::test]
async fn authentication_fanout_reaches_an_existing_oopif() {
    let server = OopifServer::start().expect("local OOPIF server starts");
    let profile = OopifProfile::create(server.port).expect("isolated Chrome profile is created");
    let config = oopif_config(&profile);

    test_config(config, async |browser| {
        let page = browser
            .new_page(server.url("localhost", "/main"))
            .await
            .expect("page creation succeeds");
        let child = wait_for_url(&page, "/child").await;
        assert!(child.is_out_of_process());

        page.authenticate(Credentials {
            username: "user".to_owned(),
            password: "pass".to_owned(),
        })
        .await
        .expect("credentials reach the existing child before navigation");
        child
            .goto(server.url("127.0.0.1", "/auth-child"))
            .await
            .expect("authenticated child navigation succeeds");
        assert_eq!(
            eval_string(&child, "document.body.dataset.role").await,
            "auth-child"
        );
        assert!(
            server.request_count("127.0.0.1", "/auth-child") >= 2,
            "the fixture observes the challenge and authenticated retry"
        );
    })
    .await;
}

#[tokio::test]
async fn bootstrap_interception_does_not_deadlock_page_creation() {
    let server = OopifServer::start().expect("local OOPIF server starts");
    let profile = OopifProfile::create(server.port).expect("isolated Chrome profile is created");
    let config = BrowserConfig::builder()
        .new_headless_mode()
        .disable_https_first()
        .enable_request_intercept()
        .request_timeout(FRAME_TIMEOUT)
        .user_data_dir(profile.path())
        .args(["--site-per-process", "--no-proxy-server"])
        .build()
        .expect("browser config is valid");

    test_config(config, async |browser| {
        let page = tokio::time::timeout(
            FRAME_TIMEOUT,
            browser.new_page(server.url("localhost", "/main")),
        )
        .await
        .expect("bootstrap interception must not wait for an unavailable responder")
        .expect("page creation succeeds");
        assert!(wait_for_url(&page, "/child").await.is_out_of_process());
    })
    .await;
}

#[tokio::test]
async fn closing_target_releases_a_delivered_unanswered_child_pause() {
    let server = OopifServer::start().expect("local OOPIF server starts");
    let profile = OopifProfile::create(server.port).expect("isolated Chrome profile is created");
    let config = oopif_config(&profile);

    test_config(config, async |browser| {
        let page = browser
            .new_page(server.url("localhost", "/main"))
            .await
            .expect("page creation succeeds");
        let child = wait_for_url(&page, "/child").await;
        let mut paused_requests = page
            .paused_requests()
            .await
            .expect("managed pause stream registers");
        page.set_request_interception(true)
            .await
            .expect("dynamic Fetch enable reaches the child");

        let child_url = server.url("127.0.0.1", "/child-nav");
        let mut navigation = Box::pin(child.goto(child_url.clone()));
        let paused = loop {
            tokio::select! {
                result = &mut navigation => {
                    panic!("child navigation completed before pause delivery: {result:?}");
                }
                paused = paused_requests.next() => {
                    let paused = paused.expect("pause stream remains live");
                    if paused.event().request.url == child_url {
                        break paused;
                    }
                    paused.continue_request().await.expect("unrelated request is released");
                }
            }
        };

        tokio::time::timeout(FRAME_TIMEOUT, page.clone().close())
            .await
            .expect("page close stays bounded")
            .expect("page closes while the child request is paused");
        assert!(
            tokio::time::timeout(FRAME_TIMEOUT, navigation)
                .await
                .expect("pending child navigation settles on target close")
                .is_err()
        );
        assert!(
            tokio::time::timeout(FRAME_TIMEOUT, paused.continue_request())
                .await
                .expect("stale paused handle fails without timing out")
                .is_err()
        );
    })
    .await;
}

#[tokio::test]
async fn frame_api_supports_nested_oopif_navigation_and_stale_handles() {
    let server = OopifServer::start().expect("local OOPIF server starts");
    let profile = OopifProfile::create(server.port).expect("isolated Chrome profile is created");
    let config = oopif_config(&profile);

    test_config(config, async |browser| {
        let page = browser
            .new_page("about:blank")
            .await
            .expect("page creation succeeds");
        let main_navigation =
            tokio::time::timeout(FRAME_TIMEOUT, page.goto(server.url("localhost", "/main"))).await;
        if main_navigation.is_err() {
            let frames = page
                .all_frames()
                .await
                .map(|frames| {
                    frames
                        .into_iter()
                        .map(|frame| (frame.id().clone(), frame.session_id().clone(), frame.url()))
                        .collect::<Vec<_>>()
                })
                .ok();
            panic!(
                "main navigation timed out; nested_served={}, frames={frames:?}",
                server.nested_served()
            );
        }
        main_navigation
            .expect("checked above")
            .expect("main navigation succeeds");
        assert!(
            server.nested_served(),
            "page navigation returned before the delayed nested OOP document loaded"
        );

        let main = page
            .main_frame()
            .await
            .expect("main frame lookup succeeds")
            .expect("main frame exists");
        assert!(main.is_main_frame());
        assert!(!main.is_out_of_process());
        assert_eq!(main.session_id(), page.session_id());

        let child = wait_for_url(&page, "/child").await;
        let same = wait_for_url(&page, "/same").await;
        let same_descendant = wait_for_url(&page, "/same-descendant").await;
        let nested = wait_for_url(&page, "/nested").await;
        assert!(child.is_out_of_process());
        assert_ne!(child.session_id(), page.session_id());
        assert_eq!(same.session_id(), page.session_id());
        assert_eq!(same_descendant.session_id(), child.session_id());
        assert_ne!(nested.session_id(), child.session_id());
        assert_eq!(
            child
                .parent()
                .await
                .expect("child parent lookup succeeds")
                .expect("child has a parent")
                .id(),
            main.id()
        );
        let child_frames = child
            .child_frames()
            .await
            .expect("child frame lookup succeeds");
        assert!(
            child_frames
                .iter()
                .any(|frame| frame.id() == same_descendant.id())
        );
        assert!(child_frames.iter().any(|frame| frame.id() == nested.id()));
        assert_eq!(
            page.frame_by_id(child.id().clone())
                .await
                .expect("frame lookup succeeds")
                .expect("child remains present")
                .session_id(),
            child.session_id()
        );
        assert_eq!(
            eval_string(&child, "document.body.dataset.role").await,
            "oop-child"
        );
        assert_eq!(
            eval_string(&same_descendant, "document.body.dataset.role").await,
            "same-descendant"
        );
        assert_eq!(
            eval_string(&nested, "document.body.dataset.role").await,
            "nested-oop"
        );

        let nested_id = nested.id().clone();
        let nested_return_url = server.url("127.0.0.1", "/nested-return");
        let nested_return_script = format!(
            "document.getElementById('nested').src = {}; true",
            serde_json::to_string(&nested_return_url).expect("URL serializes")
        );
        child
            .eval(nested_return_script)
            .await
            .expect("nested child swap-back trigger succeeds");
        let nested_return =
            wait_for_binding(&page, &nested_id, "nested S2-to-S1 swap-back", |frame| {
                frame.session_id() == child.session_id()
                    && frame
                        .url()
                        .is_some_and(|url| url.contains("/nested-return"))
            })
            .await;
        assert_stale(&nested).await;
        assert_eq!(
            eval_string(&nested_return, "document.body.dataset.role").await,
            "nested-return"
        );

        let child_tree = child
            .execute(GetFrameTreeParams::default())
            .await
            .expect("raw child-session command succeeds")
            .result;
        assert_eq!(child_tree.frame_tree.frame.id, *child.id());

        let same_document = same
            .goto(format!("{}#updated", server.url("localhost", "/same")))
            .await
            .expect("same-document frame navigation succeeds");
        assert_eq!(same_document.frame_id, *same.id());
        assert_eq!(eval_string(&same, "location.hash").await, "#updated");

        let waited_url = server.url("localhost", "/same-waited");
        let wait_script = format!(
            "setTimeout(() => location.href = {}, 0); true",
            serde_json::to_string(&waited_url).expect("URL serializes")
        );
        let (wait_result, trigger_result) = tokio::time::timeout(FRAME_TIMEOUT, async {
            tokio::join!(same.wait_for_navigation(), same.eval(wait_script))
        })
        .await
        .expect("anticipated frame navigation stays bounded");
        wait_result.expect("frame navigation waiter succeeds");
        trigger_result.expect("navigation trigger evaluates before commit");
        let same_after_wait = wait_for_url(&page, "/same-waited").await;
        assert_eq!(same_after_wait.id(), same.id());

        let swap_id = same_after_wait.id().clone();
        set_iframe_src(&page, "swap", &server.url("127.0.0.1", "/swap-cross")).await;
        let cross_handle = wait_for_binding(&page, &swap_id, "same-to-OOP swap", |frame| {
            frame.is_out_of_process() && frame.url().is_some_and(|url| url.contains("/swap-cross"))
        })
        .await;
        assert_stale(&same_after_wait).await;
        assert_eq!(
            eval_string(&cross_handle, "document.body.dataset.role").await,
            "swap-cross"
        );

        set_iframe_src(&page, "swap", &server.url("localhost", "/same-return")).await;
        let returned_handle = wait_for_binding(&page, &swap_id, "OOP-to-parent swap", |frame| {
            !frame.is_out_of_process()
                && frame.url().is_some_and(|url| url.contains("/same-return"))
        })
        .await;
        assert_stale(&cross_handle).await;
        assert_eq!(
            eval_string(&returned_handle, "document.body.dataset.role").await,
            "same-return"
        );

        page.evaluate("document.getElementById('swap').remove(); true")
            .await
            .expect("frame removal script succeeds");
        wait_for_gone(&page, &swap_id).await;
        assert_stale(&returned_handle).await;

        let same_descendant_id = same_descendant.id().clone();
        let error_navigation = tokio::time::timeout(
            FRAME_TIMEOUT,
            child.goto(server.url("127.0.0.1", "/http-error")),
        )
        .await
        .expect("HTTP error-page navigation stays bounded")
        .expect("HTTP status failures still commit their document");
        assert_eq!(error_navigation.frame_id, *child.id());
        assert_eq!(
            eval_string(&child, "document.body.dataset.role").await,
            "http-error"
        );
        wait_for_gone(&page, &nested_id).await;
        wait_for_gone(&page, &same_descendant_id).await;

        let navigation = tokio::time::timeout(
            FRAME_TIMEOUT,
            child.goto(server.url("127.0.0.1", "/child-nav")),
        )
        .await
        .expect("child navigation stays bounded")
        .expect("child navigation succeeds");
        assert_eq!(navigation.frame_id, *child.id());
        assert_eq!(
            child.fetch_url().await.expect("fresh child URL succeeds"),
            Some(server.url("127.0.0.1", "/child-nav"))
        );
        assert_eq!(
            eval_string(&child, "document.body.dataset.role").await,
            "child-nav"
        );
    })
    .await;
}

#[tokio::test]
async fn element_api_supports_frame_selection_subqueries_disposal_and_oop_geometry() {
    let server = OopifServer::start().expect("local OOPIF server starts");
    let profile = OopifProfile::create(server.port).expect("isolated Chrome profile is created");
    let config = oopif_config(&profile);

    test_config(config, async |browser| {
        let page = browser
            .new_page(server.url("localhost", "/main"))
            .await
            .expect("page creation and navigation succeed");
        let child = wait_for_url(&page, "/child").await;
        let same_descendant = wait_for_url(&page, "/same-descendant").await;
        let nested = wait_for_url(&page, "/nested").await;

        let page_element = page
            .find_element("#oop")
            .await
            .expect("page selector succeeds");
        assert!(page_element.node_id.is_some());

        let child_button = query_element(&child, "#child-button").await;
        assert!(child_button.node_id.is_none());
        let child_box = child_button
            .bounding_box()
            .await
            .expect("child bounding box succeeds");
        assert_near(child_box.x, 215.0, "child x");
        assert_near(child_box.y, 135.0, "child y");
        assert_near(child_box.width, 100.0, "child width");
        assert_near(child_box.height, 40.0, "child height");

        let same_button = query_element(&same_descendant, "#target").await;
        let same_box = same_button
            .bounding_box()
            .await
            .expect("same-session descendant bounding box succeeds");
        assert_near(same_box.x, 240.0, "same-session descendant x");
        assert_near(same_box.y, 267.0, "same-session descendant y");

        let nested_button = query_element(&nested, "#target").await;
        let nested_box = nested_button
            .bounding_box()
            .await
            .expect("nested OOP bounding box succeeds");
        assert_near(nested_box.x, 475.0, "nested OOP x");
        assert_near(nested_box.y, 271.0, "nested OOP y");

        let container = query_element(&child, "#container").await;
        let first = container
            .find_element(".item")
            .await
            .expect("object-centric single subquery succeeds");
        assert_eq!(
            first.inner_text().await.expect("text succeeds").as_deref(),
            Some("first")
        );
        assert!(matches!(
            container.find_element(".missing").await,
            Err(CdpError::NotFound)
        ));
        let items = container
            .find_elements(".item")
            .await
            .expect("object-centric multi subquery succeeds");
        let mut texts = Vec::new();
        for item in &items {
            texts.push(
                item.inner_text()
                    .await
                    .expect("item text succeeds")
                    .expect("item has text"),
            );
        }
        assert_eq!(texts, ["first", "second"]);

        assert!(
            child
                .query_selector("#does-not-exist")
                .await
                .expect("null frame selector succeeds")
                .is_none()
        );
        assert!(matches!(
            child.query_selector("[").await,
            Err(CdpError::JavascriptException(_))
        ));

        child_button
            .click()
            .await
            .expect("one-boundary child click succeeds");
        assert_eq!(
            eval_string(&child, "document.body.dataset.clicked").await,
            "child"
        );
        same_button
            .click()
            .await
            .expect("same-session descendant click succeeds");
        assert_eq!(
            eval_string(&same_descendant, "document.body.dataset.clicked").await,
            "same"
        );
        nested_button
            .hover()
            .await
            .expect("two-boundary nested hover succeeds");
        assert_eq!(
            eval_string(&nested, "document.body.dataset.hovered").await,
            "nested"
        );
        nested_button
            .click()
            .await
            .expect("two-boundary nested click succeeds");
        assert_eq!(
            eval_string(&nested, "document.body.dataset.clicked").await,
            "nested"
        );

        let drop_probe = query_element(&child, "#child-button").await;
        let drop_probe_clone = drop_probe.clone();
        drop(drop_probe);
        assert_eq!(
            drop_probe_clone
                .inner_text()
                .await
                .expect("Drop does not release the remote object")
                .as_deref(),
            Some("child")
        );

        let disposed_clone = drop_probe_clone.clone();
        drop_probe_clone
            .dispose()
            .await
            .expect("explicit remote-object disposal succeeds");
        assert!(disposed_clone.inner_text().await.is_err());
    })
    .await;
}
