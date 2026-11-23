// SPDX-License-Identifier: AGPL-3.0-or-later

//! The live connection: owns the SDK `Client` + [`WsTransport`], the
//! `initialize` → new/resume handshake, and the notification dispatch
//! (`SessionNotification` → `TranscriptStore` + `CoreListener`).
//!
//! Slices talk to the server ONLY through the pinned seams here:
//!
#![allow(clippy::type_complexity)] // deliberate: hook/listener closure types + side-table tuples
//! * [`Conn::rpc`] — a synchronous request/reply (the SDK's `block_task`,
//!   driven on the core runtime, bounded by the shared
//!   `session::rpc_timeout` policy). The reply handlers live with the *caller*:
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

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;

use agent_client_protocol::schema::v1::{
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification,
};
use agent_client_protocol::{Agent, ConnectionTo, Responder, UntypedMessage};
use serde_json::{json, Map, Value};
use tokio::sync::{oneshot, Notify};

use crate::transcript::TranscriptStore;
use crate::transport::WsTransport;
use crate::{ConfigOption, ConnectionStatus, CoreListener, PermissionOutcome, ServerConfig};

pub use agent_client_protocol::Error as AcpError;

// The wire → record translators and ACP parameter builders live in `session`
// so the roam transport shares them verbatim. Re-exported here because the
// pinned seams (INTERNAL.md) name this module.
pub(crate) use crate::session::{parse_config_options, parse_sessions};

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

    /// Move the chat-event gate to `session_id` ahead of the load that confirms it.
    ///
    /// Only the spine has a gate to move; a roam peer opens through its command
    /// loop, so peers keep the no-op. See [`Conn::prebind_session`] for why the
    /// gate must not wait for the load.
    fn prebind_session(&self, _session_id: &str) {}
}

static CURRENT_CONN: Mutex<Option<Weak<dyn RpcConn>>> = Mutex::new(None);

/// The live connection, if one is currently registered. The unstable shim
/// resolves this per call; `None` while disconnected or mid-connect.
pub fn current_conn() -> Option<Arc<dyn RpcConn>> {
    CURRENT_CONN.lock().as_ref().and_then(Weak::upgrade)
}

/// Register (or clear) the live connection. Called by the core on connect and
/// when a connection ends; the shim never touches this.
pub(crate) fn set_current_conn(conn: Option<Arc<dyn RpcConn>>) {
    *CURRENT_CONN.lock() = conn.map(|conn| Arc::downgrade(&conn));
}

/// Maps a session id to the owning roam peer (`roam:<peer>:<id>` prefix), so
/// the unstable shim can route session-bound RPCs to the peer's connection.
/// Registered by the Core, which owns the peer list.
static PEER_RESOLVER: Mutex<Option<Arc<dyn Fn(&str) -> Option<Arc<dyn RpcConn>> + Send + Sync>>> =
    Mutex::new(None);

pub(crate) fn register_peer_resolver(
    f: Arc<dyn Fn(&str) -> Option<Arc<dyn RpcConn>> + Send + Sync>,
) {
    *PEER_RESOLVER.lock() = Some(f);
}

pub(crate) fn peer_for(session_id: &str) -> Option<Arc<dyn RpcConn>> {
    PEER_RESOLVER.lock().as_ref().and_then(|f| f(session_id))
}

/// The displayed session's id, whichever connection owns it. The unstable shim
/// is constructed independently of `Core`, so the core installs this resolver
/// (same pattern as the peer resolver).
static ACTIVE_SESSION_RESOLVER: Mutex<Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>> =
    Mutex::new(None);

pub(crate) fn register_active_session_resolver(f: Arc<dyn Fn() -> Option<String> + Send + Sync>) {
    *ACTIVE_SESSION_RESOLVER.lock() = Some(f);
}

pub(crate) fn active_session() -> Option<String> {
    ACTIVE_SESSION_RESOLVER
        .lock()
        .as_ref()
        .and_then(|resolve| resolve())
        // The shim can be constructed and driven without a Core (and tests do
        // exactly that): fall back to the main connection's bound session.
        .or_else(|| current_conn().and_then(|conn| conn.active_session_id()))
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

// ---------------------------------------------------------------------------
// Permission responders (keyed by tool call, not by whichever peer is active)
// ---------------------------------------------------------------------------

/// How a connection answers one parked `session/request_permission`. The
/// closure captures its owning wire (a `Conn` here, a roam peer there), so the
/// core can route an answer by `tool_call_id` even when the displayed chat is
/// owned by a different connection.
pub(crate) type PermissionAnswer =
    Arc<dyn Fn(&str, PermissionOutcome) -> Result<(), AcpError> + Send + Sync>;

static PERMISSION_OWNERS: std::sync::OnceLock<Mutex<HashMap<String, PermissionAnswer>>> =
    std::sync::OnceLock::new();

fn permission_owners() -> &'static Mutex<HashMap<String, PermissionAnswer>> {
    PERMISSION_OWNERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register the owner of a permission prompt. Called when a connection parks a
/// responder; the owner must also remove the entry when it answers or closes.
pub(crate) fn register_permission_answer(tool_call_id: String, answer: PermissionAnswer) {
    permission_owners().lock().insert(tool_call_id, answer);
}

/// Drop the owner registration (answer already sent, prompt cancelled, or wire
/// closed). Safe to call when no entry exists.
pub(crate) fn unregister_permission_answer(tool_call_id: &str) {
    permission_owners().lock().remove(tool_call_id);
}

/// Answer a permission prompt no matter WHICH connection owns it.
///
/// `Core::respond_permission` used to route by `active_peer()`: a prompt from
/// the main wire could not be answered while a roam chat was displayed, and the
/// answer was silently discarded. The key is the tool call id the UI shows.
pub(crate) fn respond_permission(
    tool_call_id: &str,
    outcome: PermissionOutcome,
) -> Result<(), AcpError> {
    let owner = permission_owners()
        .lock()
        .remove(tool_call_id)
        .ok_or_else(|| AcpError::internal_error().data("no pending permission request"))?;
    owner(tool_call_id, outcome)
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

/// Fan a goose-custom notification out to registered slices. Called by the
/// shared client builder's untyped handler on EVERY transport.
pub(crate) fn dispatch_custom_notification(method: &str, params: Value) {
    notify_listeners(method, params);
}

// ---------------------------------------------------------------------------
// The connection
// ---------------------------------------------------------------------------

/// How a connection should start (CONTRACT §4: new-or-resume). The core
/// resolves the resume cwd before building the connection.
#[derive(Debug, Clone)]
pub(crate) enum ConnectSpec {
    /// `session/new` with `_meta.client` (and optionally `recipeId`/`projectId`).
    New {
        recipe_id: Option<String>,
        project_id: Option<String>,
        /// A client tag written at creation (`grouse.mode`): the server stores it
        /// and filters on it, so it is the only way a chat can carry a mode at all
        /// until upstream grows a mutation.
        mode: Option<String>,
        /// Show the new (empty) session. False for the throwaway session a
        /// cold `connect()` creates on its way to the real resume: the cached
        /// chat the UI already painted must stay on screen.
        display: bool,
    },
    /// `session/load` with the session's resolved cwd.
    Resume { session_id: String, cwd: String },
    /// Handshake and bind NOTHING — no `session/new`, no `session/load`, no
    /// display change.
    ///
    /// This is the reconnect for a wire whose job is the CONNECTION, not a
    /// chat: a client showing a session another transport owns (a roam peer),
    /// or a landing page with no chat at all, still needs `session/list`,
    /// projects and My Day. Resuming a session here would bind it, and binding
    /// makes it the DISPLAYED one (`hub.display` + `replace_rich`), which
    /// repaints the client's whole transcript with a chat it never asked to
    /// see — measured 2026-11-22: returning to the app reconnected the main
    /// wire, resumed the last MAIN chat, and the roam chat on screen was
    /// silently replaced by it.
    Attach,
}

/// The `session/new` params. `_meta.client` present ⇒ `SessionType::User`, so
/// Desktop/CLI can see these chats. `recipeId` starts a recipe session;
/// `projectId` files the chat under a project *at creation* — stock goose
/// persists it to the `project_id` column, which is the create-time bridge off
/// the doomed `session/project/update` mutation (see grouse#5).
fn session_new_params(
    config: &ServerConfig,
    recipe_id: Option<&str>,
    project_id: Option<&str>,
    mode: Option<&str>,
) -> Value {
    crate::session::session_new_params(
        &config.client_id,
        &config.cwd,
        recipe_id,
        project_id,
        mode,
    )
}

struct ConnInner {
    /// The core runtime; `rpc` drives `block_task` on it when called from a
    /// UI thread.
    runtime: tokio::runtime::Handle,
    /// The UI listener (permission requests, config, touched).
    listener: Arc<dyn CoreListener>,
    /// Every session's store, keyed by display id. The spine owns the main
    /// namespace (no prefix).
    hub: Arc<crate::session::SessionHub>,
    /// The one dispatch pipeline both transports share.
    host: Arc<dyn crate::session::DispatchHost>,
    /// Display-id namespace for this connection (`None` = main).
    label: Option<String>,
    config: ServerConfig,
    spec: Mutex<Option<ConnectSpec>>,
    /// The live SDK connection handle, set by the handshake.
    connection: Mutex<Option<ConnectionTo<Agent>>>,
    status: Mutex<ConnectionStatus>,
    session_id: Mutex<Option<String>>,
    /// Fresh-cache opens suppress the replayed stream; cleared on ready.
    suppress_replay: AtomicBool,
    /// Stale-cache opens ask for a bounded TAIL and merge it into the painted
    /// cache (the store anchors the overlap) instead of replaying everything.
    /// Cleared on ready.
    merge_replay: AtomicBool,
    /// The handshake signals Core (bounded connect) through this channel.
    ready: Mutex<Option<oneshot::Sender<Result<(), String>>>>,
    /// Fired by [`Conn::shutdown`]; every handshake await races this, so a
    /// superseded wire cannot finish a request and then bind or emit status.
    shutdown: Notify,
    /// Set by [`Conn::shutdown`]: the end of this wire is deliberate (superseded
    /// by a newer connect, or an explicit disconnect), not a failure — no
    /// terminal `Error` status may surface from it. A "new chat" replaces the
    /// wire while unstable RPCs can sit in flight; before this flag, every such
    /// replacement flashed `Error("connection closed")` at the UI and read as a
    /// failure of whatever was mid-round-trip (project creation rode exactly
    /// this race).
    closing: AtomicBool,
    /// Status-change hook (Core mirrors + fans out to the listener).
    on_status: Mutex<Option<Arc<dyn Fn(ConnectionStatus) + Send + Sync>>>,
    /// Open/resume failure hook: `(session_id, message)`. Fired instead of
    /// silently binding a different session (grouse#19).
    on_open_failed: Mutex<Option<Arc<dyn Fn(String, String) + Send + Sync>>>,
    /// Optional bind signal for the core host. `Core` emits
    /// `CoreListener::on_session_opened` CENTRALLY (on Ready via its open
    /// watch, plus the two live-reuse calls), so this spine hook is a no-op
    /// unless a host chooses to drive the event straight from the bind
    /// instead. It carries the DISPLAY id.
    on_session_bound: Mutex<Option<Arc<dyn Fn(String) + Send + Sync>>>,
    /// `configOptions` hook (Core mirrors the config getter).
    on_config: Mutex<Option<Arc<dyn Fn(Vec<ConfigOption>) + Send + Sync>>>,
    /// Connection-ended hook (Core: reconnect decision).
    on_ended: Mutex<Option<Arc<dyn Fn(Result<(), String>) + Send + Sync>>>,
    /// Pending `session/request_permission` responders, keyed by tool call id.
    permission: Mutex<HashMap<String, Responder<RequestPermissionResponse>>>,
    /// The same ids in the global owner registry, so shutdown can unregister
    /// them all (a dead wire must never keep a prompt the UI can answer).
    permission_ids: Mutex<HashSet<String>>,
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
        hub: Arc<crate::session::SessionHub>,
        host: Arc<dyn crate::session::DispatchHost>,
        config: ServerConfig,
        spec: ConnectSpec,
    ) -> (Arc<Self>, oneshot::Receiver<Result<(), String>>) {
        let (ready_tx, ready_rx) = oneshot::channel();
        let shutdown = Notify::new();
        let conn = Arc::new(Self {
            inner: Arc::new(ConnInner {
                runtime: crate::roam::runtime().handle().clone(),
                listener,
                hub,
                host,
                label: None,
                config,
                spec: Mutex::new(Some(spec)),
                connection: Mutex::new(None),
                status: Mutex::new(ConnectionStatus::Disconnected),
                session_id: Mutex::new(None),
                shutdown,
                closing: AtomicBool::new(false),
                suppress_replay: AtomicBool::new(false),
                merge_replay: AtomicBool::new(false),
                ready: Mutex::new(Some(ready_tx)),
                on_status: Mutex::new(None),
                on_open_failed: Mutex::new(None),
                on_session_bound: Mutex::new(None),
                on_config: Mutex::new(None),
                on_ended: Mutex::new(None),
                permission: Mutex::new(HashMap::new()),
                permission_ids: Mutex::new(HashSet::new()),
            }),
        });
        (conn, ready_rx)
    }

    pub(crate) fn is_ready(&self) -> bool {
        matches!(*self.inner.status.lock(), ConnectionStatus::Ready)
    }

    pub(crate) fn config_matches(&self, other: &ServerConfig) -> bool {
        let c = &self.inner.config;
        c.host == other.host
            && c.port == other.port
            && c.secret_key == other.secret_key
            && c.use_tls == other.use_tls
            && c.accept_invalid_certs == other.accept_invalid_certs
            && c.ca_cert_pem == other.ca_cert_pem
    }

    /// Signal an explicit disconnect — or a newer connect replacing this wire —
    /// marking the end deliberate: the handshake's idle select returns and the
    /// SDK shuts the connection down gracefully (Close frame included), and
    /// `on_connection_ended` surfaces no failure for it.
    pub(crate) fn shutdown(&self) {
        self.inner.closing.store(true, Ordering::SeqCst);
        // A `connect()` still parked on this wire must wake NOW, not after the
        // 15s wait_ready bound — and never with a success for a dead wire.
        if let Some(tx) = self.inner.ready.lock().take() {
            let _ = tx.send(Err("connection cancelled".to_string()));
        }
        self.drop_permission_owners();
        self.inner.shutdown.notify_waiters();
    }

    fn is_closing(&self) -> bool {
        self.inner.closing.load(Ordering::SeqCst)
    }

    /// Wake when this wire is deliberately closed. The first check matters as
    /// much as the notification: `shutdown()` sets `closing` BEFORE waking,
    /// so a waiter registered after the wake still sees the flag.
    async fn cancelled(&self) {
        loop {
            if self.is_closing() {
                return;
            }
            self.inner.shutdown.notified().await;
        }
    }

    fn cancelled_error() -> AcpError {
        AcpError::internal_error().data("connection cancelled by shutdown")
    }

    /// Race one await against shutdown and re-check the flag after it returns.
    /// A superseded handshake must not get as far as binding a session,
    /// emitting config, or signalling Ready.
    async fn race_shutdown<T, F>(&self, fut: F) -> Result<T, AcpError>
    where
        F: Future<Output = T>,
    {
        if self.is_closing() {
            return Err(Self::cancelled_error());
        }
        tokio::select! {
            _ = self.cancelled() => Err(Self::cancelled_error()),
            out = fut => {
                if self.is_closing() {
                    Err(Self::cancelled_error())
                } else {
                    Ok(out)
                }
            }
        }
    }

    /// Release every global permission owner registered by this connection.
    fn drop_permission_owners(&self) {
        let ids: Vec<String> = self.inner.permission_ids.lock().iter().cloned().collect();
        for id in ids {
            unregister_permission_answer(&id);
        }
        self.inner.permission_ids.lock().clear();
        self.inner.permission.lock().clear();
    }

    /// Current connection status snapshot.
    pub fn status(&self) -> ConnectionStatus {
        self.inner.status.lock().clone()
    }

    /// The session bound by `session/new` / `session/load`, if any.
    pub fn active_session_id(&self) -> Option<String> {
        self.inner.session_id.lock().clone()
    }

    /// Bind the chat-event gate to `session_id` NOW rather than when the load
    /// that confirms it lands.
    ///
    /// `session_id` is what the dispatch gate compares against: a chat event for
    /// any other session is treated as a backgrounded turn and dropped. The
    /// handshake assigns it on the runtime thread, but only *after*
    /// `Core::open_session` has resolved the session's cwd (cache → probe →
    /// `session/list`, i.e. a round trip) and spawned the connect. Meanwhile the
    /// app clears its transcript synchronously the moment it asks to switch. In
    /// that gap the gate still names the PREVIOUS chat, so that chat's in-flight
    /// chunks keep passing the gate and paint into the conversation now on
    /// screen. Pre-binding collapses a round trip of exposure to none: the gate
    /// moves in the same call the UI switches in.
    ///
    /// Safe against the load failing: a failed load leaves this id unset again
    /// (`live_load_session_async` / the handshake's Resume arm), so the gate
    /// names no session rather than the one that never loaded — and the failure
    /// is reported to the client instead of a substitute session being bound
    /// (grouse#19).
    pub fn prebind_session(&self, session_id: &str) {
        if self.is_closing() {
            return;
        }
        *self.inner.session_id.lock() = Some(session_id.to_string());
    }

    pub(crate) fn set_suppress_replay(&self, suppress: bool) {
        self.inner.suppress_replay.store(suppress, Ordering::SeqCst);
    }

    pub(crate) fn set_merge_replay(&self, merge: bool) {
        self.inner.merge_replay.store(merge, Ordering::SeqCst);
    }

    /// Bind a session without touching its store (a resume paints first).
    ///
    /// Every successful bind on this wire ends here: the handshake's Resume
    /// arm (including reconnect resume), `start_new_session` (via
    /// `bind_new_session`) and the live `session/load` reuse path all call it.
    /// That makes this the one seam for the optional bind hook. The shipped
    /// core does NOT announce opens from here: it emits
    /// `CoreListener::on_session_opened` centrally on Ready (which covers the
    /// handshake, including reconnect resume) and at the two live-reuse call
    /// sites, with bind-only loads deliberately excluded. `prebind_session`
    /// moves the event gate ahead of the load but is NOT a bind.
    fn bind_session_inner(&self, session_id: &str) -> Option<String> {
        if self.is_closing() {
            return None;
        }
        *self.inner.session_id.lock() = Some(session_id.to_string());
        let key = crate::session::display_id(self.inner.label.as_deref(), session_id);
        self.inner.hub.display(&key);
        Some(key)
    }

    fn bind_session(&self, session_id: &str) {
        if let Some(key) = self.bind_session_inner(session_id) {
            self.report_session_bound(&key);
        }
    }

    /// Notify a host that has opted into bind-driven emission. The shipped
    /// core deliberately installs no hook: it announces the same successful
    /// bind centrally on Ready (`Core::maybe_announce_open`) or at the
    /// live-reuse call site. Emitting here as well would double the event.
    fn report_session_bound(&self, display_id: &str) {
        if self.is_closing() {
            return;
        }
        if let Some(hook) = self.inner.on_session_bound.lock().clone() {
            hook(display_id.to_string());
        }
    }

    /// Bind a NEW session and clear the screen: there is no transcript yet, and
    /// the previous chat's rows must not linger behind the empty one. Any
    /// bind hook fires AFTER the clear, so a host reading the transcript in
    /// its callback sees the new chat's empty rows, not the old chat's.
    fn bind_new_session(&self, session_id: &str) {
        if let Some(key) = self.bind_session_inner(session_id) {
            self.inner.hub.store(&key).clear();
            self.report_session_bound(&key);
        }
    }

    /// The store for a session owned by this connection, in this connection's
    /// display namespace.
    pub(crate) fn store_for(&self, session_id: &str) -> Arc<TranscriptStore> {
        self.inner.hub.store(&crate::session::display_id(
            self.inner.label.as_deref(),
            session_id,
        ))
    }

    pub(crate) fn set_on_status(&self, f: Arc<dyn Fn(ConnectionStatus) + Send + Sync>) {
        *self.inner.on_status.lock() = Some(f);
    }

    pub(crate) fn set_on_open_failed(&self, f: Arc<dyn Fn(String, String) + Send + Sync>) {
        *self.inner.on_open_failed.lock() = Some(f);
    }

    /// Register a direct bind signal (DISPLAY id). The shipped core emits
    /// `on_session_opened` centrally and therefore installs NO hook here; a
    /// host that switches to bind-driven emission must register this AND
    /// remove the central `Core::maybe_announce_open` / live-reuse calls, or
    /// the UI gets the event twice.
    #[allow(dead_code)] // no hook installed by the shipped core
    pub(crate) fn set_on_session_bound(&self, f: Arc<dyn Fn(String) + Send + Sync>) {
        *self.inner.on_session_bound.lock() = Some(f);
    }

    /// Report an open/resume failure to Core. The connection binds NO session
    /// for it — a stale id must not become a fresh chat behind the client's
    /// back (grouse#19).
    fn report_open_failed(&self, session_id: impl Into<String>, message: impl Into<String>) {
        if self.is_closing() {
            return;
        }
        let hook = self.inner.on_open_failed.lock().clone();
        if let Some(hook) = hook {
            hook(session_id.into(), message.into());
        }
    }

    pub(crate) fn set_on_config(&self, f: Arc<dyn Fn(Vec<ConfigOption>) + Send + Sync>) {
        *self.inner.on_config.lock() = Some(f);
    }

    pub(crate) fn set_on_ended(&self, f: Arc<dyn Fn(Result<(), String>) + Send + Sync>) {
        *self.inner.on_ended.lock() = Some(f);
    }

    /// Synchronous request/reply. Safe from any thread: on a UI thread it
    /// parks on the core runtime until the response is dispatched; inside the
    /// runtime it uses `block_in_place`. Async callers use [`Conn::rpc_async`].
    /// Both go through the shared `session::rpc_timeout` policy — a hung
    /// host may pin an intent, never the calling thread.
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
        // `session::request` calls back through `ChatEndpoint::request`, which
        // is the RAW send — no recursion, and exactly one timeout per RPC.
        crate::session::request(self, method, params).await
    }

    /// The wire send itself. Only [`ChatEndpoint::request`] and the bounded
    /// [`Conn::rpc_async`] call this; everything else gets the timeout policy.
    async fn raw_rpc_async(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        if self.is_closing() {
            return Err(Self::cancelled_error());
        }
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
    pub fn respond_permission(
        &self,
        tool_call_id: &str,
        outcome: PermissionOutcome,
    ) -> Result<(), AcpError> {
        unregister_permission_answer(tool_call_id);
        self.inner.permission_ids.lock().remove(tool_call_id);
        let responder = self
            .inner
            .permission
            .lock()
            .remove(tool_call_id)
            .ok_or_else(|| AcpError::internal_error().data("no pending permission request"))?;
        let outcome = match outcome {
            PermissionOutcome::Selected { option_id } => {
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id))
            }
            PermissionOutcome::Cancelled => RequestPermissionOutcome::Cancelled,
        };
        responder
            .respond(RequestPermissionResponse::new(outcome))
            .map_err(|error| {
                AcpError::internal_error().data(format!("permission respond: {error}"))
            })
    }

    /// Signal Core that the connection ended (drop, explicit disconnect, or
    /// handshake failure). Deliberate closes surface no status — see below.
    pub(crate) fn on_connection_ended(&self, result: Result<(), AcpError>) {
        self.drop_permission_owners();
        // A deliberate `shutdown()` (superseded by a newer connect, or an
        // explicit disconnect) is not a failure: emit no terminal Error. The
        // replacement handshake owns the status story from here (Connecting →
        // Ready), or `disconnect()` already emitted Disconnected. The core's
        // ended hook below stays the sole reconnect/inertness decision point.
        if self.inner.closing.load(Ordering::SeqCst) {
            if let Some(hook) = self.inner.on_ended.lock().clone() {
                hook(result.map_err(|error| error.to_string()));
            }
            return;
        }
        let message = match &result {
            Ok(()) => "connection closed".to_string(),
            Err(error) => error.to_string(),
        };
        // Reflect the terminal status first so a UI thread parked in the
        // bounded `connect()` observes the failure the moment it wakes.
        self.set_status(ConnectionStatus::Error {
            message: message.clone(),
        });
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
        // A superseded wire has no status story: it must not overwrite the
        // replacement's Connecting/Ready or flash a stale Error.
        if self.is_closing() {
            return;
        }
        *self.inner.status.lock() = status.clone();
        if let Some(hook) = self.inner.on_status.lock().clone() {
            hook(status);
        }
    }

    fn signal_ready(&self, result: Result<(), String>) {
        if self.is_closing() && result.is_ok() {
            return;
        }
        if let Some(tx) = self.inner.ready.lock().take() {
            let _ = tx.send(result);
        }
    }

    fn on_ready(&self) {
        // Shutdown can land between the last replay await and this call.
        if self.is_closing() {
            return;
        }
        self.inner.suppress_replay.store(false, Ordering::SeqCst);
        self.inner.merge_replay.store(false, Ordering::SeqCst);
        // Still armed here => the replay produced nothing, so the server's copy
        // of this session is empty and the painted rows are gone from it.
        // Replay chunks precede the load reply, so nothing is racing this.
        if let Some(session_id) = self.inner.session_id.lock().clone() {
            self.store_for(&session_id).supersede_provisional();
            let key = crate::session::display_id(self.inner.label.as_deref(), &session_id);
            self.inner.host.end_replay(&key);
        }
        self.set_status(ConnectionStatus::Ready);
        if self.is_closing() {
            return;
        }
        self.signal_ready(Ok(()));
    }

    fn emit_config(&self, reply: &Value) {
        if self.is_closing() {
            return;
        }
        let options = parse_config_options(reply);
        if let Some(hook) = self.inner.on_config.lock().clone() {
            hook(options);
        }
    }

    // -- handshake (runs inside `connect_with`'s foreground) -----------------

    async fn handshake(&self, cx: ConnectionTo<Agent>) -> Result<(), AcpError> {
        if self.is_closing() {
            return Err(Self::cancelled_error());
        }
        *self.inner.connection.lock() = Some(cx.clone());
        self.set_status(ConnectionStatus::Connecting);

        // initialize: protocolVersion 1 + the desktop's exact client caps
        // (goosed hard-fails parameterized session/new without
        // recipeParameterRequests). `rpc_async` applies the shared 30s control
        // timeout; without it a server that answers pings but not initialize
        // left `connect()` parked for the full 15s ready bound with no reason.
        let _init: Value = self
            .rpc_async("initialize", initialize_params())
            .await
            .map_err(|error| {
                AcpError::internal_error().data(format!("initialize failed: {error}"))
            })?;
        if self.is_closing() {
            return Err(Self::cancelled_error());
        }

        let spec = self.inner.spec.lock().take();
        // Labelled so an advisory (non-timeout) load failure can skip the
        // rest of the spec handling and still reach `on_ready` + the idle
        // loop: the wire is usable, only the requested chat failed to load.
        'handshake: {
        match spec {
            Some(ConnectSpec::Resume { session_id, cwd }) => {
                let replay_key =
                    crate::session::display_id(self.inner.label.as_deref(), &session_id);
                // A merge fetched the overlap and the tail, so the store is
                // current up to the server's probe. A full replay (no cache)
                // may still lag it, and a fresh cache is already current.
                let persist_stamp = !self.inner.suppress_replay.load(Ordering::SeqCst)
                    && self.inner.merge_replay.load(Ordering::SeqCst);
                // Set the open-session id BEFORE the load request: the server
                // may stream replay chunks ahead of the reply (the spine's own
                // gateway test does), and the session-membership gate must not
                // drop them. start_new_session overwrites this if load fails.
                if self.is_closing() {
                    return Err(Self::cancelled_error());
                }
                *self.inner.session_id.lock() = Some(session_id.clone());
                let store = self.store_for(&session_id);
                let suppress_replay = self.inner.suppress_replay.load(Ordering::SeqCst);
                let merge_replay = self.inner.merge_replay.load(Ordering::SeqCst);
                // The replay itself (silent ingest → merge → one repaint) is the
                // shared lifecycle, so this wire cannot drift from the peer's.
                // Race it against shutdown: dropping the future lets the
                // replay guard release its hold, and a cancelled wire must
                // never reach bind_session or emit_config below.
                let endpoint: &dyn crate::session::ChatEndpoint = self;
                let replay = self
                    .race_shutdown(crate::session::replay_session(
                        endpoint,
                        self.inner.host.as_ref(),
                        &store,
                        &replay_key,
                        &cwd,
                        suppress_replay,
                        merge_replay,
                    ))
                    .await;
                let reply = match replay {
                    Ok(Ok(reply)) => reply,
                    Ok(Err(error)) => {
                        // A stale/archived session cannot be resumed. Do NOT
                        // fall back to a fresh session: the client asked for
                        // THIS one, and a silent `session/new` is what made a
                        // tapped thread open as a blank chat (grouse#19). Keep
                        // the painted cache (the best copy of the chat we have —
                        // it stays provisional, so it can never suppress the
                        // replay of a later successful load) and report; the
                        // client decides to retry or to start a new chat.
                        *self.inner.session_id.lock() = None;
                        self.report_open_failed(
                            session_id,
                            format!("session/load failed: {error}"),
                        );
                        // A load that never answered is NOT a Ready handshake:
                        // surface the failure on the status channel and unblock
                        // `connect()` with Err through `on_connection_ended`.
                        // Other load failures stay advisory (the wire is fine).
                        if handshake_timed_out(&error) {
                            return Err(AcpError::internal_error().data(format!(
                                "session/load failed: {error}"
                            )));
                        }
                        break 'handshake;
                    }
                    Err(cancelled) => return Err(cancelled),
                };
                if self.is_closing() {
                    return Err(Self::cancelled_error());
                }
                self.bind_session(&session_id);
                self.emit_config(&reply);
                if persist_stamp {
                    // The merge fetched the overlap and tail; stamp the
                    // result with the server's current updatedAt.
                    self.inner.host.stamp_probe(&replay_key, true);
                }
            }
            Some(ConnectSpec::New {
                recipe_id,
                project_id,
                mode,
                display,
            }) => {
                self.race_shutdown(self.start_new_session(recipe_id, project_id, mode, display))
                    .await??;
            }
            // Handshake only: the caller is showing something this wire does not
            // own (a peer's chat) or nothing at all, so there is no session to
            // bind — and binding one would repaint the client's transcript.
            // `session/list` and every connection-level call work without a
            // session; the first `open_session` binds.
            Some(ConnectSpec::Attach) => {}
            None => {
                self.race_shutdown(self.start_new_session(None, None, None, true))
                    .await??;
            }
        }
        }

        if self.is_closing() {
            return Err(Self::cancelled_error());
        }
        self.on_ready();

        // Keep the connection alive until Core disconnects or the transport
        // ends (either way `connect_with` returns and `on_connection_ended`
        // fires). The shutdown race covers the whole idle span too.
        tokio::select! {
            _ = self.cancelled() => Err(Self::cancelled_error()),
            _ = cx.incoming_closed() => Ok(()),
        }
    }

    async fn start_new_session(
        &self,
        recipe_id: Option<String>,
        project_id: Option<String>,
        mode: Option<String>,
        display: bool,
    ) -> Result<(), AcpError> {
        let params = session_new_params(
            &self.inner.config,
            recipe_id.as_deref(),
            project_id.as_deref(),
            mode.as_deref(),
        );
        let reply: Value = self
            .rpc_async("session/new", params)
            .await
            .map_err(|error| {
                AcpError::internal_error().data(format!("session/new failed: {error}"))
            })?;
        if self.is_closing() {
            return Err(Self::cancelled_error());
        }
        let session_id = reply
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // A cold `connect()` may create a throwaway session while the UI still
        // shows the last chat's cached paint; keep that paint. With nothing
        // painted (a first launch), or for an explicit new chat, show it.
        if display || self.inner.hub.displayed_key().is_none() {
            self.bind_new_session(&session_id);
        } else {
            *self.inner.session_id.lock() = Some(session_id);
        }
        self.emit_config(&reply);
        Ok(())
    }

    /// Live-wire `session/new` without tearing the connection down. Used by
    /// `Core::new_session` when a Ready connection with the same host/port/key
    /// already exists — avoids the `old.shutdown()` race that killed in-flight
    /// `sources/create` during project creation. Returns the new session id.
    pub(crate) async fn live_new_session_async(
        &self,
        recipe_id: Option<String>,
        project_id: Option<String>,
        mode: Option<String>,
    ) -> Result<String, AcpError> {
        if self.is_closing() {
            return Err(Self::cancelled_error());
        }
        let params = session_new_params(
            &self.inner.config,
            recipe_id.as_deref(),
            project_id.as_deref(),
            mode.as_deref(),
        );
        let reply: Value = self.rpc_async("session/new", params).await?;
        let session_id = reply
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.bind_new_session(&session_id);
        self.emit_config(&reply);
        Ok(session_id)
    }
    /// `session/new` that does NOT adopt the reply.
    ///
    /// `live_new_session_async` binds the new session to the connection, which is
    /// right for a chat the user asked for and wrong for anything created in the
    /// background: a thread's conversation is minted while the reader may be
    /// mid-sentence in another chat, and binding it would silently move them.
    pub(crate) async fn new_session_detached(
        &self,
        project_id: Option<&str>,
    ) -> Result<String, AcpError> {
        if self.is_closing() {
            return Err(Self::cancelled_error());
        }
        let params = session_new_params(&self.inner.config, None, project_id, None);
        let reply: Value = self.rpc_async("session/new", params).await?;
        Ok(reply
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// Live-wire `session/load` — same wire-reuse rationale as
    /// `live_new_session_async`. The blocking twin that used to sit here had no
    /// caller once session handling became async end to end.
    /// `bind_only` is the session-with-a-live-turn load: bind it on the wire and
    /// discard the replay, verified or not (a verification that downgraded this
    /// to a merge would anchor on the live rows and truncate the reply being
    /// streamed — see `session::PaintPlan::bind_only`).
    pub(crate) async fn live_load_session_async(
        &self,
        session_id: String,
        cwd: String,
        suppress_replay: bool,
        merge_replay: bool,
        bind_only: bool,
    ) -> Result<(), AcpError> {
        if self.is_closing() {
            return Err(Self::cancelled_error());
        }
        // NOTE: the gate/session id is ALREADY this session (`prebind_session`,
        // called by the open before the cwd round trip), so a failure cannot be
        // "restored" to the previous chat from here — see the Err arm below for
        // what a failed load leaves behind.
        self.inner.session_id.lock().replace(session_id.clone());
        let store = self.store_for(&session_id);
        let replay_key = crate::session::display_id(self.inner.label.as_deref(), &session_id);
        // VERIFY a suppression before arming the gates: a fresh-cache claim is
        // built from the baseline this core last saw listed, and another client's
        // writes never move it. Downgraded to a bounded merge here (the one place
        // before the gates are set) so the replayed rows are kept rather than
        // dropped — see `session::verified_ask`.
        let endpoint: &dyn crate::session::ChatEndpoint = self;
        let ask = if bind_only {
            crate::session::ReplayAsk::bind_only()
        } else {
            crate::session::verified_ask(
                endpoint,
                self.inner.host.as_ref(),
                &replay_key,
                crate::session::ReplayAsk {
                    suppress: suppress_replay,
                    merge: merge_replay,
                    tail: crate::session::first_tail(suppress_replay, merge_replay),
                },
            )
            .await
        };
        let (suppress_replay, merge_replay) = (ask.suppress, ask.merge);
        self.inner
            .suppress_replay
            .store(suppress_replay, Ordering::SeqCst);
        self.inner
            .merge_replay
            .store(merge_replay, Ordering::SeqCst);
        // A merge fetched the tail, so its result may be stamped with a newer
        // probe; a full replay (no cache) may still lag it, and a fresh cache
        // is already current.
        let persist_stamp = merge_replay && !suppress_replay;
        // One shared replay lifecycle (see `session::replay_session`): hold the
        // rows, load, close any merge, repaint once — the peer path uses the
        // same function, which is what keeps the two from drifting apart.
        let reply = crate::session::replay_session(
            endpoint,
            self.inner.host.as_ref(),
            &store,
            &replay_key,
            &cwd,
            suppress_replay,
            merge_replay,
        )
        .await;
        // Disarm the replay gates for the NEXT load: a fresh-cache open arms
        // `suppress_replay`, and a later full rebuild (resync) that did not reset
        // it would have every replayed row dropped — an empty transcript.
        endpoint.reset_replay_gates();
        let reply = reply.inspect_err(|_| {
            // The load never happened, so the connection must NOT keep claiming
            // this session (grouse#19): nothing is bound, and the gate names
            // nothing, until a retry or a new chat binds one.
            *self.inner.session_id.lock() = None;
        })?;
        self.bind_session(&session_id);
        self.emit_config(&reply);
        if persist_stamp {
            self.inner.host.stamp_probe(&replay_key, true);
        }
        Ok(())
    }

    // -- notification dispatch (runs on the runtime, never blocks) -----------

    fn dispatch_session_notification(&self, notif: SessionNotification) {
        // The shared pipeline intercepts the hidden summarizer session.
        crate::session::dispatch(self.inner.host.as_ref(), notif);
    }

    // -- server requests (answered on the dispatch loop) ---------------------

    fn on_permission_request(
        &self,
        req: RequestPermissionRequest,
        responder: Responder<RequestPermissionResponse>,
    ) {
        if self.is_closing() {
            let _ = responder.respond_with_error(Self::cancelled_error());
            return;
        }
        let request = crate::session::permission_request(&req);
        let tool_call_id = request.tool_call_id.clone();
        self.inner.permission.lock().insert(tool_call_id.clone(), responder);
        // `Core::respond_permission` used to route by `active_peer()`: a main
        // wire prompt was unanswerable while a roam chat was on screen. The
        // global registry routes by the id the UI actually shows.
        let owner = self.clone();
        register_permission_answer(
            tool_call_id.clone(),
            Arc::new(move |id, outcome| owner.respond_permission(id, outcome)),
        );
        self.inner.permission_ids.lock().insert(tool_call_id);
        self.inner.listener.on_permission_request(request);
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
                    AcpError::method_not_found()
                        .data(format!("not supported by this client: {method}")),
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
        park_server_request(kind, method, params, responder);
    }
}

/// Park a recipe-params / elicitation responder and fan the request out to the
/// registered slices. With no listener the request is auto-answered (recipe
/// defaults) or declined — desktop parity. Shared by every connection: the
/// parked responders are client-global, because the UI answers one request at
/// a time regardless of which wire it arrived on.
pub(crate) fn park_server_request(
    kind: PendingRequest,
    method: &str,
    params: &Value,
    responder: Responder<Value>,
) {
    let listeners = server_request_listeners();
    if listeners.is_empty() {
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
                                let default = obj.get("default").cloned().unwrap_or(Value::Null);
                                Some((key, default))
                            })
                            .collect::<Map<String, Value>>()
                    })
                    .unwrap_or_default();
                let _ = responder.respond(json!({ "action": "submit", "values": values }));
            }
            PendingRequest::Elicitation => {
                let _ = responder.respond_with_error(
                    AcpError::method_not_found()
                        .data("elicitation/create not supported by this client"),
                );
            }
        }
        return;
    }
    let previous = {
        let mut parked = PARKED.lock();
        let slot = match kind {
            PendingRequest::RecipeParams => &mut parked.recipe_params,
            PendingRequest::Elicitation => &mut parked.elicitation,
        };
        slot.replace(responder)
    };
    // The CONTRACT's respond_* intents carry no request key, so a second
    // request of the same family used to overwrite the first responder and
    // leave that server waiting forever. Fail the old one explicitly; the UI
    // can re-offer its form for the request it still knows about.
    if let Some(previous) = previous {
        let _ = previous.respond_with_error(
            AcpError::internal_error().data("superseded by a newer server request"),
        );
    }
    for f in listeners {
        f(method, params.clone());
    }
}

impl RpcConn for Conn {
    fn rpc(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        Conn::rpc(self, method, params)
    }

    fn active_session_id(&self) -> Option<String> {
        Conn::active_session_id(self)
    }

    fn prebind_session(&self, session_id: &str) {
        Conn::prebind_session(self, session_id)
    }
}

impl crate::session::ChatEndpoint for Conn {
    fn request(
        &self,
        method: &str,
        params: Value,
    ) -> futures::future::BoxFuture<'static, Result<Value, AcpError>> {
        let conn = self.clone();
        let method = method.to_string();
        Box::pin(async move {
            // The policy lives in `session::request` around this seam; the raw
            // send also races shutdown so a superseded wire cannot pin an intent.
            conn.race_shutdown(conn.raw_rpc_async(&method, params))
                .await
                .and_then(|reply| reply)
        })
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), AcpError> {
        Conn::notify(self, method, params)
    }

    fn wire_id(&self, display: &str) -> String {
        display.to_string()
    }

    fn accepts_prompt(&self) -> bool {
        self.is_ready() && self.active_session_id().is_some()
    }

    fn label(&self) -> Option<String> {
        None
    }

    fn reset_replay_gates(&self) {
        self.inner.suppress_replay.store(false, Ordering::SeqCst);
        self.inner.merge_replay.store(false, Ordering::SeqCst);
    }
}

/// Did a handshake RPC fail because the host never answered (as opposed to
/// answering with an error)? Only a timeout gets a terminal status and a
/// `ready` error; a rejected `session/load` stays advisory (grouse#19).
fn handshake_timed_out(error: &AcpError) -> bool {
    let text = error.to_string().to_lowercase();
    text.contains("timed out") || text.contains("gave no answer")
}

/// The core's exponential reconnect backoff curve: 500ms·2^n capped at 15s.
/// Shared by the main connection (`Core::schedule_reconnect`) and the roam
/// peer supervisor (`roam::RoamPeer::connect`) so both retry on the same
/// rhythm; only their attempt budgets differ.
pub(crate) fn reconnect_delay_ms(attempt: u32) -> u64 {
    500u64 * (1u64 << attempt.min(5)).min(15000 / 500)
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
    let handlers = crate::session::ClientHandlers {
        notification: Arc::new(move |notif| notif_conn.dispatch_session_notification(notif)),
        custom_notification: Arc::new(notify_listeners),
        permission: Arc::new(move |req, responder| {
            permission_conn.on_permission_request(req, responder)
        }),
        request: Arc::new(move |method, params, responder| {
            request_conn.on_server_request(method, params, responder)
        }),
    };
    let foreground = move |cx: ConnectionTo<Agent>| -> futures::future::BoxFuture<'static, Result<(), AcpError>> {
        let conn = conn.clone();
        Box::pin(async move { conn.handshake(cx).await })
    };
    crate::session::connect_client(transport, handlers, foreground).await
}

/// The transport's TLS uses rustls' `ring` provider (Cargo.toml
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
/// custom notifications + parameterized recipes + MCP-App hosting.
///
/// `mcpHostCapabilities.extensions["io.modelcontextprotocol/ui"]` is what makes
/// the server's `host_supports_mcp_apps()` gate true over ACP; without it goose
/// never hydrates a tool's `_meta.ui.resourceUri` into the trusted
/// `_meta.goose.mcpApp` on the tool_call update, and every MCP App (the
/// autovisualiser charts included) silently degrades to a plain tool chip.
pub(crate) fn initialize_params() -> Value {
    crate::session::initialize_params()
}

/// The `session/list` params: list user + ACP sessions (desktop listSessions).
pub(crate) fn session_list_params(cursor: Option<&str>, tags: &[(String, String)]) -> Value {
    crate::session::session_list_params(cursor, tags)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The initialize handshake MUST declare the MCP-Apps host capability:
    /// goose gates `mcpApp` metadata hydration on the client listing
    /// `io.modelcontextprotocol/ui` under `_meta.goose.mcpHostCapabilities.
    /// extensions` — without it every MCP App (autovisualiser charts, custom
    /// dashboards) silently degrades to a plain tool chip (2026-09-18).
    /// The UI answers a prompt by tool call id, not by which chat/peer is on
    /// screen. Two prompts from two wires must reach their own owners.
    #[test]
    fn permission_answers_route_by_tool_call_id() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        for id in ["spine-test-perm-a", "spine-test-perm-b"] {
            let seen = seen.clone();
            let owned = id.to_string();
            register_permission_answer(
                id.to_string(),
                Arc::new(move |id, _outcome| {
                    seen.lock().push(format!("{owned}:{id}"));
                    Ok(())
                }),
            );
        }
        assert!(respond_permission(
            "spine-test-perm-b",
            PermissionOutcome::Cancelled
        )
        .is_ok());
        assert_eq!(*seen.lock(), vec!["spine-test-perm-b:spine-test-perm-b".to_string()]);
        // The same id cannot be answered twice, and the other owner survives.
        assert!(respond_permission(
            "spine-test-perm-b",
            PermissionOutcome::Cancelled
        )
        .is_err());
        assert!(respond_permission(
            "spine-test-perm-a",
            PermissionOutcome::Cancelled
        )
        .is_ok());
        assert_eq!(seen.lock().len(), 2);
    }

    #[test]
    fn a_timeout_is_distinguished_from_a_rejected_load() {
        let timeout = AcpError::internal_error().data("session/load gave no answer in 600s");
        assert!(handshake_timed_out(&timeout));
        let rejected = AcpError::internal_error().data("session/load failed: -32602 not found");
        assert!(!handshake_timed_out(&rejected));
    }

    #[test]
    fn initialize_declares_mcp_apps_host_capability() {
        let p = initialize_params();
        assert_eq!(
            p.pointer(
                "/clientCapabilities/_meta/goose/mcpHostCapabilities/\
                       extensions/io.modelcontextprotocol~1ui/mimeTypes/0"
            )
            .and_then(Value::as_str),
            Some("text/html;profile=mcp-app"),
            "lost the MCP-Apps capability declaration — the server will stop \
             attaching _meta.goose.mcpApp to tool calls"
        );
    }

    /// grouse#5 P0: `session/new` must carry `_meta.projectId` when a project
    /// chat is created, so stock goose persists it at creation instead of us
    /// calling the doomed `session/project/update`.
    #[test]
    fn session_new_params_stamps_project_id() {
        let cfg = ServerConfig {
            host: "h".into(),
            port: 1,
            secret_key: "k".into(),
            use_tls: false,
            accept_invalid_certs: false,
            ca_cert_pem: None,
            cwd: "/w".into(),
            auto_connect: false,
            client_id: "grouse-desktop".into(),
            initial_recipe_id: None,
            initial_project_id: None,
        };
        let p = session_new_params(&cfg, Some("r-1"), Some("proj-9"), Some("dev"));
        assert_eq!(
            p.pointer("/_meta/client").and_then(Value::as_str),
            Some("grouse-desktop")
        );
        assert_eq!(
            p.pointer("/_meta/recipeId").and_then(Value::as_str),
            Some("r-1")
        );
        assert_eq!(
            p.pointer("/_meta/projectId").and_then(Value::as_str),
            Some("proj-9")
        );

        // Absent/empty ids are omitted, never emitted as `""`.
        let p = session_new_params(&cfg, None, Some(""), None);
        assert!(p.pointer("/_meta/recipeId").is_none());
        assert!(p.pointer("/_meta/projectId").is_none());
    }

    /// Ordering/freshness must track MESSAGE activity (_meta.lastMessageAt),
    /// not open/select touch (updatedAt) — selecting a session must never
    /// reorder the sidebar.
    #[test]
    fn sessions_order_by_message_activity_not_touch() {
        let reply = json!({"sessions": [
            {
                "sessionId": "a",
                "title": "recently messaged",
                "cwd": "/x",
                "updatedAt": "2026-08-22T12:00:00+00:00", // later: opened, not messaged
                "_meta": { "lastMessageAt": "2026-08-22T06:00:00Z" }
            },
            {
                "sessionId": "b",
                "title": "no message time yet (fallback)",
                "cwd": "/y",
                "updatedAt": "2026-08-22T11:00:00+00:00",
                "_meta": {}
            }
        ]});
        let (sessions, _, updated) = parse_sessions(&reply);
        assert_eq!(sessions[0].updated_at, "2026-08-22T06:00:00Z");
        // The bump-on-open updatedAt must NOT leak into the ordering key.
        assert_ne!(sessions[0].updated_at, "2026-08-22T12:00:00+00:00");
        // No message time -> fall back to updatedAt (unchanged previous behavior).
        assert_eq!(sessions[1].updated_at, "2026-08-22T11:00:00+00:00");
        assert_eq!(updated.len(), 2);
    }
}
