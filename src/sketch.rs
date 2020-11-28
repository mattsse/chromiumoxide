use std::collections::HashMap;
use std::sync::Arc;

use crate::cdp::browser_protocol::browser::BrowserContextId;
use crate::cdp::browser_protocol::page::FrameId;
use crate::cdp::browser_protocol::target::{SessionId, TargetId, TargetInfo};

/// Share this among all who need to dispatch
struct Connection {
    sessions: HashMap<SessionId, Session>,
    /// This should be the oneshot sender to the WS connection?
    ws: (),
}

struct Session {
    id: SessionId,
    target_type: String,
    _conn: Arc<Connection>,
}

struct Target {
    info: TargetInfo,
    browser_context: BrowserContext,
    browser_context_id: BrowserContextId,
}

struct Page {
    target: Target,
    client: Session,
    frame_manager: FrameManager,
    workers: HashMap<String, WebWorker>,
}

impl Page {
    // client.on('Target.attachedToTarget', (event) => {
    // if (event.targetInfo.type !== 'worker') {
    // // If we don't detach from service workers, they will never die.
    // client
    // .send('Target.detachFromTarget', {
    // sessionId: event.sessionId,
    // })
    // .catch(debugError);
    // return;
    // }
    // const session = Connection.fromSession(client).session(event.sessionId);
    // const worker = new WebWorker(
    // session,
    // event.targetInfo.url,
    // this._addConsoleMessage.bind(this),
    // this._handleException.bind(this)
    // );
    // this._workers.set(event.sessionId, worker);
    // this.emit(PageEmittedEvents.WorkerCreated, worker);
    // });
    // client.on('Target.detachedFromTarget', (event) => {
    // const worker = this._workers.get(event.sessionId);
    // if (!worker) return;
    // this.emit(PageEmittedEvents.WorkerDestroyed, worker);
    // this._workers.delete(event.sessionId);
    // });

    // client.on('Page.domContentEventFired', () =>
    // this.emit(PageEmittedEvents.DOMContentLoaded)
    // );
    // client.on('Page.loadEventFired', () => this.emit(PageEmittedEvents.Load));
    // client.on('Runtime.consoleAPICalled', (event) => this._onConsoleAPI(event));
    // client.on('Runtime.bindingCalled', (event) => this._onBindingCalled(event));
    // client.on('Page.javascriptDialogOpening', (event) => this._onDialog(event));
    // client.on('Runtime.exceptionThrown', (exception) =>
    // this._handleException(exception.exceptionDetails)
    // );
    // client.on('Inspector.targetCrashed', () => this._onTargetCrashed());
    // client.on('Performance.metrics', (event) => this._emitMetrics(event));
    // client.on('Log.entryAdded', (event) => this._onLogEntryAdded(event));
    // client.on('Page.fileChooserOpened', (event) => this._onFileChooser(event));
}

/// Listens for added frames
struct FrameManager {
    frames: HashMap<FrameId, Frame>,
    main_frame: Frame,
    page_ref: Box<Page>,
}

impl FrameManager {
    // this._client.on('Page.frameAttached', (event) =>
    // this._onFrameAttached(event.frameId, event.parentFrameId)
    // );
    // this._client.on('Page.frameNavigated', (event) =>
    // this._onFrameNavigated(event.frame)
    // );
    // this._client.on('Page.navigatedWithinDocument', (event) =>
    // this._onFrameNavigatedWithinDocument(event.frameId, event.url)
    // );
    // this._client.on('Page.frameDetached', (event) =>
    // this._onFrameDetached(event.frameId)
    // );
    // this._client.on('Page.frameStoppedLoading', (event) =>
    // this._onFrameStoppedLoading(event.frameId)
    // );
    // this._client.on('Runtime.executionContextCreated', (event) =>
    // this._onExecutionContextCreated(event.context)
    // );
    // this._client.on('Runtime.executionContextDestroyed', (event) =>
    // this._onExecutionContextDestroyed(event.executionContextId)
    // );
    // this._client.on('Runtime.executionContextsCleared', () =>
    // this._onExecutionContextsCleared()
    // );
    // this._client.on('Page.lifecycleEvent', (event) =>
    // this._onLifecycleEvent(event)
    // );
    // this._client.on('Target.attachedToTarget', async (event) =>
    // this._onFrameMoved(event)
    // );
}

struct Frame {
    client: Session,
    parent_frame: Option<Box<Frame>>,
}

/// A Browser is created when chromiumoxid connects to a Chromium instance
struct Browser {
    contexts: HashMap<BrowserContextId, BrowserContext>,
    default_context: BrowserContext,
    targets: HashMap<TargetId, Target>,
}

impl Browser {
    // this._connection.on(ConnectionEmittedEvents.Disconnected, () =>
    // this.emit(BrowserEmittedEvents.Disconnected)
    // );
    // this._connection.on('Target.targetCreated', this._targetCreated.bind(this));
    // this._connection.on(
    // 'Target.targetDestroyed',
    // this._targetDestroyed.bind(this)
    // );
    // this._connection.on(
    // 'Target.targetInfoChanged',
    // this._targetInfoChanged.bind(this)
    // );
}

struct WebWorker {}

// Notes
// Main event loop that keeps track of the state of the system (processes all
// the incoming events) and the commandline chromium instance, must run in
// separate task Required a main entry point that interacts with the main state
// Different event types that are sent via channels
// All the types Browser, Page, should be created on the event loop instance and
// sent via channel On request from the main entry site the resp should be a
// Result where Ok(Resp from CDP) and Err(CommandErr|StateError(Closed etc.)) If
// the browser gets dropped then the event loop closes, user can obtain a
// listener/subscription to specific events(session|target) via channel request
// to main loop that responds with Arc<Mutex<Rec>>>
