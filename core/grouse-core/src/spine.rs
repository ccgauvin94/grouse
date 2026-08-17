//! The live connection: owns the SDK `Client` + [`WsTransport`], the
//! `initialize` → new/resume handshake, and the notification dispatch
//! (`SessionNotification` → `TranscriptStore` + `CoreListener`).
//!
//! Slices talk to the server ONLY through the pinned seams here:
//!
#![allow(clippy::type_complexity)] // deliberate: hook/listener closure types + side-table tuples
//! * [`Conn::rpc`] — a synchronous request/reply (the SDK's `block_task`,
//!   driven on the core runtime). The reply handlers live with the *caller*:
//!   the SDK routes each response to its pending request, so `session/list`
//!   replies are parsed by the intent that asked, exactly like the desktop's
//!   `AcpClient::response()` dispatch table but with the table split across
//!   the owning slices.
//! * [`Conn::notify`] — a fire-and-forget notification (`session/cancel`).
//! * [`Conn::status`] + the status-change hook (set by the core).
//! * [`current_conn`] / [`set_current_conn`] — the process-wide registry the
//!   unstable shim resolves per call (the shim is constructed independently
//!   of `Core`, possibly before `connect()`).
//! * [`PendingRequest`] / [`answer_request`] — the parked server-request
//!   responders (`recipe/request-params`, `elicitation/create`), answered by
//!   the unstable shim's `respond_*` intents.
//!
//! Server→client *requests* are answered here: `session/request_permission`
//! parks the SDK responder (the UI answers via `respond_permission`), the
//! recipe/elicitation requests park a responder per family (the UI answers via
//! `answer_request`, or the request is auto-answered with defaults when no
//! slice registered a listener), and unknown methods get a real JSON-RPC
//! `-32601` error (desktop `serverRequest` fallback).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;

use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, PermissionOptionKind, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    SessionConfigKind, SessionNotification, SessionUpdate, ToolCallContent, ToolCallStatus,
};
use agent_client_protocol::schema::MaybeUndefined;
use agent_client_protocol::{
    Agent, Client, ConnectionTo, Responder, UntypedMessage, on_receive_notification,
    on_receive_request,
};
use serde_json::{Map, Value, json};
use tokio::sync::oneshot;

use crate::transcript::TranscriptStore;
use crate::transport::WsTransport;
use crate::roam::RoamPeer;
use crate::{
    ConfigChoice, ConfigOption, ConnectionStatus, CoreListener, PermissionOption, PermissionOutcome,
    PermissionRequest, ServerConfig, SessionSummary, ToolCallKind,
};

pub use agent_client_protocol::Error as AcpError;

// ---------------------------------------------------------------------------
// Connection registry (slices reach the live connection through here)
// ---------------------------------------------------------------------------

/// The seam the unstable shim codes against: any live connection (the spine's
/// [`Conn`], or a stub in tests) exposes the synchronous RPC and the bound
/// session.
pub trait RpcConn: Send + Sync {
    /// Synchronous request/reply: `block_task` driven on the core runtime.
    fn rpc(&self, method: &str, params: Value) -> Result<Value, AcpError>;
    /// The session bound by `session/new` or `session/load`, if any.
    fn active_session_id(&self) -> Option<String>;
}

static CURRENT_CONN: Mutex<Option<Weak<dyn RpcConn>>> = Mutex::new(None);

/// The live connection, if one is currently registered. The unstable shim
/// resolves this per call; `None` while disconnected or mid-connect.
pub fn current_conn() -> Option<Arc<dyn RpcConn>> {
    CURRENT_CONN
        .lock()
        .as_ref()
        .and_then(Weak::upgrade)
}

/// Register (or clear) the live connection. Called by the core on connect and
/// when a connection ends; the shim never touches this.
pub(crate) fn set_current_conn(conn: Option<Arc<dyn RpcConn>>) {
    *CURRENT_CONN.lock() = conn.map(|conn| Arc::downgrade(&conn));
}

/// Maps a session id to the owning roam peer (`roam:<peer>:<id>` prefix), so
/// the unstable shim can route session-bound RPCs to the peer's connection.
/// Registered by the Core, which owns the peer list.
static PEER_RESOLVER: Mutex<Option<Arc<dyn Fn(&str) -> Option<Arc<RoamPeer>> + Send + Sync>>> =
    Mutex::new(None);

pub(crate) fn register_peer_resolver(
    f: Arc<dyn Fn(&str) -> Option<Arc<RoamPeer>> + Send + Sync>,
) {
    *PEER_RESOLVER.lock() = Some(f);
}

pub(crate) fn peer_for(session_id: &str) -> Option<Arc<RoamPeer>> {
    PEER_RESOLVER
        .lock()
        .as_ref()
        .and_then(|f| f(session_id))
}

// ---------------------------------------------------------------------------
// Parked server-request responders (recipe-params / elicitation)
// ---------------------------------------------------------------------------

/// Which parked server request a [`respond_*`](crate::GrouseUnstable) intent
/// answers. Single-slot per family: the CONTRACT signatures carry no request
/// key, so the latest request wins (like the desktop's single `m_pending` tag
/// per family would).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingRequest {
    /// `_goose/unstable/session/recipe/request-params`
    RecipeParams,
    /// `elicitation/create`
    Elicitation,
}

#[derive(Default)]
struct Parked {
    recipe_params: Option<Responder<Value>>,
    elicitation: Option<Responder<Value>>,
}

static PARKED: Mutex<Parked> = Mutex::new(Parked {
    recipe_params: None,
    elicitation: None,
});

/// Answer the parked server request of the given kind with a raw JSON result.
/// Returns `false` when no request is parked (already answered or never
/// arrived).
pub fn answer_request(kind: PendingRequest, result: Value) -> bool {
    let responder = match kind {
        PendingRequest::RecipeParams => PARKED.lock().recipe_params.take(),
        PendingRequest::Elicitation => PARKED.lock().elicitation.take(),
    };
    match responder {
        Some(responder) => responder.respond(result).is_ok(),
        None => false,
    }
}

/// A registered server-request listener (the unstable shim registers one at
/// construction so the UI can opt into answering recipe/elicitation requests).
/// While at least one listener is active the spine parks the request and fans
/// it out; with none it auto-answers (desktop parity).
pub struct ServerRequestListenerGuard {
    entry: Arc<ServerRequestListener>,
}

struct ServerRequestListener {
    active: AtomicBool,
    f: Arc<dyn Fn(&str, Value) + Send + Sync>,
}

impl Drop for ServerRequestListenerGuard {
    fn drop(&mut self) {
        self.entry.active.store(false, Ordering::SeqCst);
    }
}

/// Register a listener for server→client requests. The spine calls
/// `f(method, params)` for every request it receives (parked ones included);
/// the guard deregisters on drop. `Send + Sync` so it can sit in a uniffi
/// object field.
pub fn register_server_request_listener(
    f: Arc<dyn Fn(&str, Value) + Send + Sync + 'static>,
) -> ServerRequestListenerGuard {
    let entry = Arc::new(ServerRequestListener {
        active: AtomicBool::new(true),
        f,
    });
    SERVER_REQUEST_LISTENERS.lock().push(entry.clone());
    ServerRequestListenerGuard { entry }
}

static SERVER_REQUEST_LISTENERS: Mutex<Vec<Arc<ServerRequestListener>>> = Mutex::new(Vec::new());

fn server_request_listeners() -> Vec<Arc<dyn Fn(&str, Value) + Send + Sync>> {
    SERVER_REQUEST_LISTENERS
        .lock()
        .iter()
        .filter(|entry| entry.active.load(Ordering::SeqCst))
        .map(|entry| entry.f.clone())
        .collect()
}

/// A registered notification listener (the unstable shim registers one for
/// the goose-custom `_goose/unstable/session/update` notifications:
/// `status_message` / `message_usage`). Same guard pattern as
/// [`register_server_request_listener`].
pub struct NotificationListenerGuard {
    entry: Arc<NotificationListener>,
}

struct NotificationListener {
    active: AtomicBool,
    f: Arc<dyn Fn(&str, Value) + Send + Sync>,
}

impl Drop for NotificationListenerGuard {
    fn drop(&mut self) {
        self.entry.active.store(false, Ordering::SeqCst);
    }
}

/// Register a listener for notifications the spine does not itself dispatch
/// (custom `_goose/...` methods). The spine calls `f(method, params)` for every
/// untyped notification after its own `session/update` dispatch.
pub fn register_notification_listener(
    f: Arc<dyn Fn(&str, Value) + Send + Sync + 'static>,
) -> NotificationListenerGuard {
    let entry = Arc::new(NotificationListener {
        active: AtomicBool::new(true),
        f,
    });
    NOTIFICATION_LISTENERS.lock().push(entry.clone());
    NotificationListenerGuard { entry }
}

static NOTIFICATION_LISTENERS: Mutex<Vec<Arc<NotificationListener>>> = Mutex::new(Vec::new());

fn notify_listeners(method: &str, params: Value) {
    for f in NOTIFICATION_LISTENERS
        .lock()
        .iter()
        .filter(|entry| entry.active.load(Ordering::SeqCst))
        .map(|entry| entry.f.clone())
    {
        f(method, params.clone());
    }
}

// ---------------------------------------------------------------------------
// The connection
// ---------------------------------------------------------------------------

/// How a connection should start (CONTRACT §4: new-or-resume). The core
/// resolves the resume cwd before building the connection.
#[derive(Debug, Clone)]
pub(crate) enum ConnectSpec {
    /// `session/new` with `_meta.client` (and optionally `recipeId`).
    New { recipe_id: Option<String> },
    /// `session/load` with the session's resolved cwd.
    Resume { session_id: String, cwd: String },
}

struct ConnInner {
    /// The core runtime; `rpc` drives `block_task` on it when called from a
    /// UI thread.
    runtime: tokio::runtime::Handle,
    /// The UI listener (permission requests, config, touched).
    listener: Arc<dyn CoreListener>,
    /// The shared transcript store (chunks + bubbles + stream events).
    store: Arc<TranscriptStore>,
    config: ServerConfig,
    spec: Mutex<Option<ConnectSpec>>,
    /// The live SDK connection handle, set by the handshake.
    connection: Mutex<Option<ConnectionTo<Agent>>>,
    status: Mutex<ConnectionStatus>,
    session_id: Mutex<Option<String>>,
    active_run_id: Mutex<Option<String>>,
    /// Fresh-cache opens suppress the replayed stream; cleared on ready.
    suppress_replay: AtomicBool,
    /// `session/load` replay (open + resync): thought chunks are dropped so a
    /// replayed reasoning trail does not double up (desktop `m_replaying`).
    replaying: AtomicBool,
    /// The handshake signals Core (bounded connect) through this channel.
    ready: Mutex<Option<oneshot::Sender<Result<(), String>>>>,
    /// Core triggers an explicit disconnect through this channel.
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
    /// The handshake's idle select waits on this receiver.
    shutdown_rx: Mutex<Option<oneshot::Receiver<()>>>,
    /// Status-change hook (Core mirrors + fans out to the listener).
    on_status: Mutex<Option<Arc<dyn Fn(ConnectionStatus) + Send + Sync>>>,
    /// `session_info_update` hook (Core: sidebar state + resync debounce).
    on_touched: Mutex<Option<Arc<dyn Fn(String, String, String) + Send + Sync>>>,
    /// Active-run hook: `(session_id, run_id)`; an empty run_id means the run ended.
    on_active_run: Mutex<Option<Arc<dyn Fn(String, String) + Send + Sync>>>,
    /// Slash-command availability hook (`available_commands_update` names).
    on_commands: Mutex<Option<Arc<dyn Fn(Vec<String>) + Send + Sync>>>,
    /// `configOptions` hook (Core mirrors the config getter).
    on_config: Mutex<Option<Arc<dyn Fn(Vec<ConfigOption>) + Send + Sync>>>,
    /// Connection-ended hook (Core: reconnect decision).
    on_ended: Mutex<Option<Arc<dyn Fn(Result<(), String>) + Send + Sync>>>,
    /// Pending `session/request_permission` responders, keyed by tool call id.
    permission: Mutex<HashMap<String, Responder<RequestPermissionResponse>>>,
}

/// The live connection. Owned by the core; slices reach it through
/// [`current_conn`] or the seams on [`Conn`].
#[derive(Clone)]
pub struct Conn {
    inner: Arc<ConnInner>,
}

impl Conn {
    /// Build a fresh connection object. The SDK client + transport are wired
    /// by [`run_connection`]; `spec` is consumed by the handshake. Returns
    /// the connection and the receiver the bounded `connect()` waits on.
    pub(crate) fn new(
        listener: Arc<dyn CoreListener>,
        store: Arc<TranscriptStore>,
        config: ServerConfig,
        spec: ConnectSpec,
    ) -> (Arc<Self>, oneshot::Receiver<Result<(), String>>) {
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let conn = Arc::new(Self {
            inner: Arc::new(ConnInner {
                runtime: crate::roam::runtime().handle().clone(),
                listener,
                store,
                config,
                spec: Mutex::new(Some(spec)),
                connection: Mutex::new(None),
                status: Mutex::new(ConnectionStatus::Disconnected),
                session_id: Mutex::new(None),
                active_run_id: Mutex::new(None),
                suppress_replay: AtomicBool::new(false),
                replaying: AtomicBool::new(false),
                ready: Mutex::new(Some(ready_tx)),
                shutdown_tx: Mutex::new(Some(shutdown_tx)),
                shutdown_rx: Mutex::new(Some(shutdown_rx)),
                on_status: Mutex::new(None),
                on_touched: Mutex::new(None),
                on_active_run: Mutex::new(None),
                on_commands: Mutex::new(None),
                on_config: Mutex::new(None),
                on_ended: Mutex::new(None),
                permission: Mutex::new(HashMap::new()),
            }),
        });
        (conn, ready_rx)
    }

    /// Signal an explicit disconnect: the handshake's idle select returns and
    /// the SDK shuts the connection down gracefully (Close frame included).
    pub(crate) fn shutdown(&self) {
        if let Some(tx) = self.inner.shutdown_tx.lock().take() {
            let _ = tx.send(());
        }
    }

    /// Current connection status snapshot.
    pub fn status(&self) -> ConnectionStatus {
        self.inner.status.lock().clone()
    }

    /// The session bound by `session/new` / `session/load`, if any.
    pub fn active_session_id(&self) -> Option<String> {
        self.inner.session_id.lock().clone()
    }

    /// The run id of the live turn (`session_info_update` `_meta.goose
    /// activeRunId`), if any. Tracked for steer plumbing: the unstable shim's
    /// `steer` carries the UI's `expected_run_id`, and this is the server's
    /// authoritative id for diagnostics when the two disagree.
    #[allow(dead_code)] // written by the dispatch; read by future steer validation
    pub(crate) fn active_run_id(&self) -> Option<String> {
        self.inner.active_run_id.lock().clone()
    }

    pub(crate) fn set_suppress_replay(&self, suppress: bool) {
        self.inner.suppress_replay.store(suppress, Ordering::SeqCst);
    }

    pub(crate) fn set_replaying(&self, replaying: bool) {
        self.inner.replaying.store(replaying, Ordering::SeqCst);
    }

    pub(crate) fn set_on_status(&self, f: Arc<dyn Fn(ConnectionStatus) + Send + Sync>) {
        *self.inner.on_status.lock() = Some(f);
    }

    pub(crate) fn set_on_touched(&self, f: Arc<dyn Fn(String, String, String) + Send + Sync>) {
        *self.inner.on_touched.lock() = Some(f);
    }

    pub(crate) fn set_on_active_run(&self, f: Arc<dyn Fn(String, String) + Send + Sync>) {
        *self.inner.on_active_run.lock() = Some(f);
    }

    pub(crate) fn set_on_commands(&self, f: Arc<dyn Fn(Vec<String>) + Send + Sync>) {
        *self.inner.on_commands.lock() = Some(f);
    }

    pub(crate) fn set_on_config(&self, f: Arc<dyn Fn(Vec<ConfigOption>) + Send + Sync>) {
        *self.inner.on_config.lock() = Some(f);
    }

    pub(crate) fn set_on_ended(&self, f: Arc<dyn Fn(Result<(), String>) + Send + Sync>) {
        *self.inner.on_ended.lock() = Some(f);
    }

    /// Synchronous request/reply. Safe from any thread: on a UI thread it
    /// parks on the core runtime until the response is dispatched; inside the
    /// runtime it uses `block_in_place` (the internal async paths use
    /// [`Conn::rpc_async`] directly).
    pub fn rpc(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        let conn = self.clone();
        let method = method.to_string();
        let fut = async move { conn.rpc_async(&method, params).await };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => tokio::task::block_in_place(move || handle.block_on(fut)),
            Err(_) => self.inner.runtime.block_on(fut),
        }
    }

    /// Fire-and-forget notification (`session/cancel`). Never waits for a
    /// reply (the server answers none).
    pub fn notify(&self, method: &str, params: Value) -> Result<(), AcpError> {
        let conn = self
            .inner
            .connection
            .lock()
            .clone()
            .ok_or_else(|| AcpError::internal_error().data("not connected"))?;
        let msg = UntypedMessage::new(method, params)?;
        conn.send_notification(msg)
    }

    pub(crate) async fn rpc_async(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        let conn = self
            .inner
            .connection
            .lock()
            .clone()
            .ok_or_else(|| AcpError::internal_error().data("not connected"))?;
        let msg = UntypedMessage::new(method, params)?;
        conn.send_request(msg).block_task().await
    }

    /// Answer a parked `session/request_permission` (the UI's
    /// `respond_permission` intent routes here).
    pub fn respond_permission(&self, tool_call_id: &str, outcome: PermissionOutcome) -> Result<(), AcpError> {
        let responder = self
            .inner
            .permission
            .lock()
            .remove(tool_call_id)
            .ok_or_else(|| AcpError::internal_error().data("no pending permission request"))?;
        let outcome = match outcome {
            PermissionOutcome::Selected { option_id } => RequestPermissionOutcome::Selected(
                SelectedPermissionOutcome::new(option_id),
            ),
            PermissionOutcome::Cancelled => RequestPermissionOutcome::Cancelled,
        };
        responder
            .respond(RequestPermissionResponse::new(outcome))
            .map_err(|error| {
                AcpError::internal_error().data(format!("permission respond: {error}"))
            })
    }

    /// Signal Core that the connection ended (drop, explicit disconnect, or
    /// handshake failure).
    pub(crate) fn on_connection_ended(&self, result: Result<(), AcpError>) {
        let message = match &result {
            Ok(()) => "connection closed".to_string(),
            Err(error) => error.to_string(),
        };
        // Reflect the terminal status first so a UI thread parked in the
        // bounded `connect()` observes the failure the moment it wakes.
        self.set_status(ConnectionStatus::Error { message: message.clone() });
        // A handshake that never reached ready must unblock the bounded
        // `connect()` wait with the failure instead of letting it time out.
        if !matches!(*self.inner.status.lock(), ConnectionStatus::Ready) {
            if let Some(tx) = self.inner.ready.lock().take() {
                let _ = tx.send(Err(message));
            }
        }
        // The reconnect decision lives in the core's ended hook.
        if let Some(hook) = self.inner.on_ended.lock().clone() {
            hook(result.map_err(|error| error.to_string()));
        }
    }

    // -- status -------------------------------------------------------------

    fn set_status(&self, status: ConnectionStatus) {
        *self.inner.status.lock() = status.clone();
        if let Some(hook) = self.inner.on_status.lock().clone() {
            hook(status);
        }
    }

    fn signal_ready(&self, result: Result<(), String>) {
        if let Some(tx) = self.inner.ready.lock().take() {
            let _ = tx.send(result);
        }
    }

    fn on_ready(&self) {
        self.inner.suppress_replay.store(false, Ordering::SeqCst);
        self.inner.replaying.store(false, Ordering::SeqCst);
        self.set_status(ConnectionStatus::Ready);
        self.signal_ready(Ok(()));
    }

    fn emit_config(&self, reply: &Value) {
        let options = parse_config_options(reply);
        if let Some(hook) = self.inner.on_config.lock().clone() {
            hook(options);
        }
    }

    // -- handshake (runs inside `connect_with`'s foreground) -----------------

    async fn handshake(&self, cx: ConnectionTo<Agent>) -> Result<(), AcpError> {
        *self.inner.connection.lock() = Some(cx.clone());
        self.set_status(ConnectionStatus::Connecting);

        // initialize: protocolVersion 1 + the desktop's exact client caps
        // (goosed hard-fails parameterized session/new without
        // recipeParameterRequests).
        let _init: Value = cx
            .send_request(UntypedMessage::new("initialize", initialize_params())?)
            .block_task()
            .await
            .map_err(|error| {
                AcpError::internal_error().data(format!("initialize failed: {error}"))
            })?;

        let spec = self.inner.spec.lock().take();
        match spec {
            Some(ConnectSpec::Resume { session_id, cwd }) => {
                self.inner.replaying.store(true, Ordering::SeqCst);
                // Set the open-session id BEFORE the load request: the server
                // may stream replay chunks ahead of the reply (the spine's own
                // gateway test does), and the session-membership gate must not
                // drop them. start_new_session overwrites this if load fails.
                *self.inner.session_id.lock() = Some(session_id.clone());
                let params = json!({
                    "sessionId": session_id,
                    "cwd": cwd,
                    "mcpServers": [],
                });
                match cx
                    .send_request(UntypedMessage::new("session/load", params)?)
                    .block_task()
                    .await
                {
                    Ok(reply) => {
                        *self.inner.session_id.lock() = Some(session_id);
                        self.emit_config(&reply);
                    }
                    Err(_) => {
                        // A stale/archived session cannot be resumed — fall
                        // back to a fresh session (desktop response()
                        // session/load error branch) rather than wedging.
                        self.start_new_session(&cx, None).await?;
                    }
                }
            }
            Some(ConnectSpec::New { recipe_id }) => {
                self.start_new_session(&cx, recipe_id).await?;
            }
            None => {
                self.start_new_session(&cx, None).await?;
            }
        }

        self.on_ready();

        // Keep the connection alive until Core disconnects or the transport
        // ends (either way `connect_with` returns and `on_connection_ended`
        // fires).
        let mut shutdown_rx = self.inner.shutdown_rx.lock().take();
        tokio::select! {
            _ = wait_optional(&mut shutdown_rx) => Ok(()),
            _ = cx.incoming_closed() => Ok(()),
        }
    }

    async fn start_new_session(
        &self,
        cx: &ConnectionTo<Agent>,
        recipe_id: Option<String>,
    ) -> Result<(), AcpError> {
        let mut params = json!({
            "cwd": self.inner.config.cwd,
            "mcpServers": [],
        });
        let mut meta = Map::new();
        // _meta.client present => SessionType::User, so Desktop/CLI can see
        // these chats (desktop startNewSession).
        meta.insert("client".into(), Value::String(self.inner.config.client_id.clone()));
        if let Some(recipe) = recipe_id {
            meta.insert("recipeId".into(), Value::String(recipe));
        }
        params["_meta"] = Value::Object(meta);
        let reply: Value = cx
            .send_request(UntypedMessage::new("session/new", params)?)
            .block_task()
            .await
            .map_err(|error| {
                AcpError::internal_error().data(format!("session/new failed: {error}"))
            })?;
        let session_id = reply
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        *self.inner.session_id.lock() = Some(session_id);
        self.emit_config(&reply);
        Ok(())
    }

    // -- notification dispatch (runs on the runtime, never blocks) -----------

    fn dispatch_session_notification(&self, notif: SessionNotification) {
        let session_id = notif.session_id.to_string();
        // Session-membership gate (serve parity with the roam peers): a CHAT
        // event for a session other than the one currently open belongs to a
        // backgrounded turn — never render it in the wrong chat (serve re-loads
        // on every open, so the reply is recovered by the next replay). Applied
        // only to bubble-producing events: session-level config/run updates
        // (SessionInfoUpdate etc.) are not chat content and may arrive before
        // the open/load reply has set session_id (e.g. session/new broadcasts).
        let chat_event = matches!(
            &notif.update,
            SessionUpdate::UserMessageChunk(_)
                | SessionUpdate::AgentMessageChunk(_)
                | SessionUpdate::AgentThoughtChunk(_)
                | SessionUpdate::ToolCall(_)
                | SessionUpdate::ToolCallUpdate(_)
                | SessionUpdate::UsageUpdate(_)
        );
        if chat_event && self.inner.session_id.lock().as_deref() != Some(session_id.as_str()) {
            return;
        }
        match notif.update {
            SessionUpdate::UserMessageChunk(chunk) => {
                if !self.inner.suppress_replay.load(Ordering::SeqCst) {
                    let text = chunk_text(&chunk);
                    if !text.is_empty() {
                        let mid = chunk.message_id.map(|id| id.to_string());
                        self.inner
                            .store
                            .append_chunk("user", &text, mid.as_deref(), false);
                    }
                }
            }
            SessionUpdate::AgentMessageChunk(chunk) => {
                if !self.inner.suppress_replay.load(Ordering::SeqCst) {
                    let text = chunk_text(&chunk);
                    if !text.is_empty() {
                        let mid = chunk.message_id.map(|id| id.to_string());
                        self.inner
                            .store
                            .append_chunk("agent", &text, mid.as_deref(), false);
                    }
                }
            }
            SessionUpdate::AgentThoughtChunk(chunk) => {
                // Replayed thoughts (session/load) are dropped: the reasoning
                // trail would double up in the bubble (desktop m_replaying).
                if !self.inner.suppress_replay.load(Ordering::SeqCst)
                    && !self.inner.replaying.load(Ordering::SeqCst)
                {
                    let text = chunk_text(&chunk);
                    if !text.is_empty() {
                        self.inner.store.append_chunk("thought", &text, None, true);
                    }
                }
            }
            SessionUpdate::ToolCall(tc) => {
                if !self.inner.suppress_replay.load(Ordering::SeqCst) {
                    self.dispatch_tool_call(&tc);
                }
            }
            SessionUpdate::ToolCallUpdate(tcu) => {
                if !self.inner.suppress_replay.load(Ordering::SeqCst) {
                    self.dispatch_tool_update(&tcu);
                }
            }
            SessionUpdate::UsageUpdate(usage) => {
                let (amount, currency) = usage
                    .cost
                    .as_ref()
                    .map(|cost| (cost.amount, cost.currency.clone()))
                    .unwrap_or((0.0, String::new()));
                self.inner
                    .store
                    .usage(usage.used as i64, usage.size as i64, amount, &currency);
            }
            SessionUpdate::SessionInfoUpdate(info) => {
                self.dispatch_session_info_update(&session_id, info);
            }
            SessionUpdate::ConfigOptionUpdate(config) => {
                let options = config
                    .config_options
                    .iter()
                    .map(|option| ConfigOption {
                        id: option.id.to_string(),
                        value: config_option_value(option),
                        name: option.name.clone(),
                        // The typed schema has no choices list — the initial
                        // config (session/new|load replies) carries them.
                        choices: Vec::new(),
                    })
                    .collect::<Vec<_>>();
                if let Some(hook) = self.inner.on_config.lock().clone() {
                    hook(options);
                }
            }
            SessionUpdate::CurrentModeUpdate(_) => {
                // No stable listener event carries this; the desktop patches
                // it in place. Ignored here.
            }
            SessionUpdate::AvailableCommandsUpdate(commands) => {
                // Slash-command autocomplete: surface the command names so a
                // client can offer them without a server round-trip.
                let names = commands
                    .available_commands
                    .iter()
                    .map(|command| command.name.clone())
                    .collect::<Vec<_>>();
                if let Some(hook) = self.inner.on_commands.lock().clone() {
                    hook(names);
                }
            }
            SessionUpdate::Plan(_) => {}
            _ => {} // non_exhaustive
        }
    }

    fn dispatch_tool_call(&self, tc: &agent_client_protocol::schema::v1::ToolCall) {
        let tool_call_id = tc.tool_call_id.to_string();
        let title = tc.title.clone();
        let raw_input = tc.raw_input.clone().unwrap_or(Value::Null);
        let detail = raw_input
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let goose = tc
            .meta
            .as_ref()
            .and_then(|meta| meta.get("goose"))
            .cloned()
            .unwrap_or(Value::Null);

        // MCP-App path: the server names a UI resource for this tool's output
        // (how ALL autovisualiser types work). Until the template is fetched
        // it is still a plain chip.
        let mcp_app = goose.get("mcpApp").cloned().unwrap_or(Value::Null);
        let app_uri = mcp_app
            .get("resourceUri")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let app_ext = mcp_app
            .get("extensionName")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let kind = if !app_uri.is_empty() && !app_ext.is_empty() {
            let input = compact_json(&raw_input);
            ToolCallKind::McpApp {
                app_key: format!("{app_ext}|{app_uri}"),
                uri: app_uri,
                extension: app_ext,
                input,
            }
        } else {
            // Legacy chart fallback only (server too old to send mcpApp meta):
            // `data` arrives as a JSON object, not a string.
            let tool_name = goose
                .pointer("/toolCall/toolName")
                .and_then(Value::as_str)
                .unwrap_or("");
            let chart_data = raw_input
                .get("data")
                .map(|data| match data {
                    Value::Object(_) => compact_json(data),
                    Value::String(s) => s.clone(),
                    _ => String::new(),
                })
                .unwrap_or_default();
            if tool_name == "autovisualiser__show_chart" && !chart_data.is_empty() {
                ToolCallKind::Chart { spec: chart_data }
            } else {
                ToolCallKind::Plain
            }
        };
        self.inner
            .store
            .tool_call(&title, &detail, &tool_call_id, kind);
    }

    fn dispatch_tool_update(&self, tcu: &agent_client_protocol::schema::v1::ToolCallUpdate) {
        let id = tcu.tool_call_id.to_string();
        let status = tcu
            .fields
            .status
            .as_ref()
            .map(tool_call_status_str)
            .unwrap_or_default();
        let meta = tcu.meta.clone().unwrap_or_default();

        // Streaming shell output rides _meta.toolNotification (live_output):
        // appends with live=true so the bubble ACCUMULATES instead of
        // replacing (desktop tool_call_update).
        let notif = meta.get("toolNotification").cloned().unwrap_or(Value::Null);
        if notif.get("type").and_then(Value::as_str) == Some("live_output") {
            let mut chunk = String::new();
            if let Some(parts) = notif.pointer("/params/chunks").and_then(Value::as_array) {
                for part in parts {
                    if let Some(text) = part.pointer("/output/text").and_then(Value::as_str) {
                        chunk.push_str(text);
                    }
                }
            }
            if !chunk.is_empty() {
                self.inner.store.tool_update(&id, status, &chunk, true);
            }
            return;
        }

        let mut outputs = Vec::new();
        if let Some(content) = &tcu.fields.content {
            for entry in content {
                if let ToolCallContent::Content(content) = entry {
                    if let ContentBlock::Text(text) = &content.content {
                        outputs.push(text.text.clone());
                    }
                }
            }
        }
        self.inner
            .store
            .tool_update(&id, status, &outputs.join("\n"), false);
    }

    fn dispatch_session_info_update(
        &self,
        session_id: &str,
        info: agent_client_protocol::schema::v1::SessionInfoUpdate,
    ) {
        // The active-run lifecycle rides _meta.goose.activeRunId; its presence
        // is what makes session/steer possible. Emit both transitions: the run
        // starting (id present) and ending (absent in this update).
        if let Some(goose) = info.meta.as_ref().and_then(|meta| meta.get("goose")) {
            if let Some(run_id) = goose.get("activeRunId").and_then(Value::as_str) {
                *self.inner.active_run_id.lock() = Some(run_id.to_string());
                if let Some(hook) = self.inner.on_active_run.lock().clone() {
                    hook(session_id.to_string(), run_id.to_string());
                }
            } else if self.inner.active_run_id.lock().take().is_some() {
                if let Some(hook) = self.inner.on_active_run.lock().clone() {
                    hook(session_id.to_string(), String::new());
                }
            }
        }
        let title = match info.title {
            MaybeUndefined::Value(title) => title,
            _ => String::new(),
        };
        let updated_at = match info.updated_at {
            MaybeUndefined::Value(updated_at) => updated_at,
            _ => String::new(),
        };
        if let Some(hook) = self.inner.on_touched.lock().clone() {
            hook(session_id.to_string(), title, updated_at);
        }
    }

    // -- server requests (answered on the dispatch loop) ---------------------

    fn on_permission_request(
        &self,
        req: RequestPermissionRequest,
        responder: Responder<RequestPermissionResponse>,
    ) {
        let tool_call_id = req.tool_call.tool_call_id.to_string();
        let title = req.tool_call.fields.title.clone().unwrap_or_default();
        let detail = req
            .tool_call
            .fields
            .raw_input
            .as_ref()
            .and_then(|raw| raw.get("command"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let options = req
            .options
            .iter()
            .map(|option| PermissionOption {
                option_id: option.option_id.to_string(),
                name: option.name.clone(),
                kind: permission_option_kind_str(&option.kind).to_string(),
            })
            .collect();
        self.inner
            .permission
            .lock()
            .insert(tool_call_id.clone(), responder);
        self.inner.listener.on_permission_request(PermissionRequest {
            tool_call_id,
            title,
            detail,
            options,
        });
    }

    fn on_server_request(&self, method: &str, params: &Value, responder: Responder<Value>) {
        match method {
            "_goose/unstable/session/recipe/request-params" => {
                self.park_and_notify(PendingRequest::RecipeParams, method, params, responder);
            }
            "elicitation/create" => {
                self.park_and_notify(PendingRequest::Elicitation, method, params, responder);
            }
            _ => {
                // Unknown server request -> a real JSON-RPC error, not a
                // silent empty result (desktop serverRequest fallback).
                let _ = responder.respond_with_error(
                    AcpError::method_not_found().data(format!("not supported by this client: {method}")),
                );
            }
        }
    }

    fn park_and_notify(
        &self,
        kind: PendingRequest,
        method: &str,
        params: &Value,
        responder: Responder<Value>,
    ) {
        let listeners = server_request_listeners();
        if listeners.is_empty() {
            // No slice opted into forms: auto-answer with the parameter
            // defaults so the session can start (desktop serverRequest); an
            // elicitation without a form is declined the same way the desktop
            // rejects it.
            match kind {
                PendingRequest::RecipeParams => {
                    let values = params
                        .get("parameters")
                        .and_then(Value::as_array)
                        .map(|parameters| {
                            parameters
                                .iter()
                                .filter_map(|p| {
                                    let obj = p.as_object()?;
                                    let key = obj.get("key")?.as_str()?.to_string();
                                    let default = obj
                                        .get("default")
                                        .cloned()
                                        .unwrap_or(Value::Null);
                                    Some((key, default))
                                })
                                .collect::<Map<String, Value>>()
                        })
                        .unwrap_or_default();
                    let _ = responder.respond(json!({ "action": "submit", "values": values }));
                }
                PendingRequest::Elicitation => {
                    let _ = responder.respond_with_error(AcpError::method_not_found().data(
                        "elicitation/create not supported by this client",
                    ));
                }
            }
            return;
        }
        let slot = match kind {
            PendingRequest::RecipeParams => &mut PARKED.lock().recipe_params,
            PendingRequest::Elicitation => &mut PARKED.lock().elicitation,
        };
        *slot = Some(responder);
        for f in listeners {
            f(method, params.clone());
        }
    }
}

impl RpcConn for Conn {
    fn rpc(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        Conn::rpc(self, method, params)
    }

    fn active_session_id(&self) -> Option<String> {
        Conn::active_session_id(self)
    }
}

// ---------------------------------------------------------------------------
// The SDK client + transport wiring
// ---------------------------------------------------------------------------

/// Wire the SDK `Client` (notification + server-request handlers) to the
/// transport and run the connection until it ends. The handlers dispatch into
/// `conn`; the foreground runs the handshake and then idles until shutdown or
/// transport EOF. Returns when the connection closes.
pub(crate) async fn run_connection(
    conn: Arc<Conn>,
    transport: WsTransport,
) -> Result<(), AcpError> {
    install_crypto_provider();
    let notif_conn = conn.clone();
    let permission_conn = conn.clone();
    let request_conn = conn.clone();

    let builder = Client.builder()
        .name("grouse")
        // Typed handler claims `session/update`; the untyped one below claims
        // every other notification (handlers are tried in registration order).
        .on_receive_notification(
            async move |notif: SessionNotification, _cx| {
                notif_conn.dispatch_session_notification(notif);
                Ok(())
            },
            on_receive_notification!(),
        )
        .on_receive_notification(
            async move |notif: UntypedMessage, _cx| {
                // Custom notifications (gooseUpdate: status_message /
                // message_usage, and anything else) go to registered slices.
                notify_listeners(&notif.method, notif.params.clone());
                Ok(())
            },
            on_receive_notification!(),
        )
        // `session/request_permission` — the UI answers via respond_permission.
        .on_receive_request(
            async move |req: RequestPermissionRequest, responder, _cx| {
                permission_conn.on_permission_request(req, responder);
                Ok(())
            },
            on_receive_request!(),
        )
        // Recipe-params / elicitation (parked) + unknown-method fallback.
        .on_receive_request(
            async move |req: UntypedMessage, responder, _cx| {
                request_conn.on_server_request(&req.method, &req.params, responder);
                Ok(())
            },
            on_receive_request!(),
        );

    builder
        .connect_with(transport, move |cx: ConnectionTo<Agent>| {
            let conn = conn.clone();
            async move { conn.handshake(cx).await }
        })
        .await
}

async fn wait_optional(rx: &mut Option<oneshot::Receiver<()>>) {
    if let Some(rx) = rx.as_mut() {
        let _ = rx.await;
    }
}

/// The transport's trust-all TLS uses rustls' `ring` provider (Cargo.toml
/// pins it); `async-tungstenite`'s tokio-rustls feature re-enables the
/// `aws-lc-rs` default, so rustls cannot auto-select. Install `ring`
/// explicitly (once) before any connection builds a `ClientConfig`.
fn install_crypto_provider() {
    use std::sync::OnceLock;
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

// ---------------------------------------------------------------------------
// Wire-format helpers (the desktop's exact JSON shapes)
// ---------------------------------------------------------------------------

/// The `initialize` params: protocolVersion 1 + the client capabilities the
/// goosed server needs (fs, elicitation form, and the goose `_meta` gate for
/// custom notifications + parameterized recipes).
pub(crate) fn initialize_params() -> Value {
    json!({
        "protocolVersion": 1,
        "clientCapabilities": {
            "fs": { "readTextFile": false, "writeTextFile": false },
            "elicitation": { "form": {} },
            "_meta": {
                "goose": {
                    "customNotifications": true,
                    "recipeParameterRequests": true,
                    "toolCallLabelEnrichment": true
                }
            }
        }
    })
}

/// The `session/list` params: list user + ACP sessions (desktop listSessions).
pub(crate) fn session_list_params() -> Value {
    json!({ "_meta": { "types": ["user", "acp"] } })
}

/// Parse a `session/list` reply into summaries, plus side tables the core
/// keeps (cwd for resume resolution, updatedAt for cache freshness).
pub(crate) fn parse_sessions(result: &Value) -> (Vec<SessionSummary>, Vec<(String, String)>, Vec<(String, String)>) {
    let mut sessions = Vec::new();
    let mut cwds = Vec::new();
    let mut updated_at = Vec::new();
    if let Some(list) = result.get("sessions").and_then(Value::as_array) {
        for entry in list {
            let Some(obj) = entry.as_object() else { continue };
            let Some(session_id) = obj.get("sessionId").and_then(Value::as_str) else { continue };
            let title = obj
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if title.is_empty() && session_id.is_empty() {
                continue;
            }
            if title.starts_with("Scheduled job:") {
                continue;
            }
            let meta = obj.get("_meta").and_then(Value::as_object);
            // goose's archive only stamps archivedAt; session/list has no
            // archived filter, so drop them or they reappear on refresh.
            if meta.map(|m| m.contains_key("archivedAt")).unwrap_or(false) {
                continue;
            }
            let snippet = meta
                .and_then(|m| m.get("lastMessageSnippet"))
                .and_then(Value::as_str)
                .map(|s| s.to_string());
            let project_id = meta
                .and_then(|m| m.get("projectId"))
                .and_then(Value::as_str)
                .map(|s| s.to_string());
            let message_count = meta
                .and_then(|m| m.get("messageCount"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let model = meta
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let has_recipe = meta
                .and_then(|m| m.get("hasRecipe"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let updated = obj
                .get("updatedAt")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let cwd = obj
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            sessions.push(SessionSummary {
                id: session_id.to_string(),
                title: if title.is_empty() { session_id.to_string() } else { title },
                updated_at: updated.clone(),
                last_message_snippet: snippet,
                project_id,
                message_count,
                model,
                has_recipe,
                has_new: false,
            });
            if !cwd.is_empty() {
                cwds.push((session_id.to_string(), cwd));
            }
            if !updated.is_empty() {
                updated_at.push((session_id.to_string(), updated));
            }
        }
    }
    (sessions, cwds, updated_at)
}

/// Parse a reply's `configOptions` into the flat `ConfigOption` records the
/// contract exposes (id + current value as a string).
pub(crate) fn parse_config_options(result: &Value) -> Vec<ConfigOption> {
    result
        .get("configOptions")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|option| {
                    let obj = option.as_object()?;
                    let id = obj.get("id")?.as_str()?.to_string();
                    let value = match obj.get("currentValue") {
                        Some(Value::String(s)) => s.clone(),
                        Some(Value::Bool(b)) => b.to_string(),
                        Some(Value::Number(n)) => n.to_string(),
                        _ => String::new(),
                    };
                    let name = obj.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                    // goose sends the selectable values under `options`
                    // (SessionConfigSelect.options); the desktop parses the
                    // same key. `choices` is never sent — reading it made every
                    // dropdown empty (mode pill did nothing).
                    let choices = obj
                        .get("options")
                        .and_then(Value::as_array)
                        .map(|choices| {
                            choices
                                .iter()
                                .filter_map(|c| {
                                    let c = c.as_object()?;
                                    Some(ConfigChoice {
                                        value: c.get("value")?.as_str()?.to_string(),
                                        name: c.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    Some(ConfigOption { id, value, name, choices })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The typed `SessionConfigOption`'s current value, flattened to a string.
pub(crate) fn config_option_value(option: &agent_client_protocol::schema::v1::SessionConfigOption) -> String {
    match &option.kind {
        SessionConfigKind::Select(select) => select.current_value.to_string(),
        SessionConfigKind::Boolean(boolean) => boolean.current_value.to_string(),
        _ => String::new(),
    }
}

/// Chunk text with the desktop's content-type mapping: text passes through,
/// resources are placeholders, anything else is bracketed.
fn chunk_text(chunk: &ContentChunk) -> String {
    match &chunk.content {
        ContentBlock::Text(text) => text.text.clone(),
        ContentBlock::ResourceLink(_) => "[resource]".to_string(),
        ContentBlock::Resource(_) => "[resource]".to_string(),
        ContentBlock::Image(_) => "[image]".to_string(),
        ContentBlock::Audio(_) => "[audio]".to_string(),
        _ => "[unknown]".to_string(),
    }
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn tool_call_status_str(status: &ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::InProgress => "in_progress",
        ToolCallStatus::Completed => "completed",
        ToolCallStatus::Failed => "failed",
        _ => "",
    }
}

fn permission_option_kind_str(kind: &PermissionOptionKind) -> &'static str {
    match kind {
        PermissionOptionKind::AllowOnce => "allow_once",
        PermissionOptionKind::AllowAlways => "allow_always",
        PermissionOptionKind::RejectOnce => "reject_once",
        PermissionOptionKind::RejectAlways => "reject_always",
        _ => "",
    }
}
