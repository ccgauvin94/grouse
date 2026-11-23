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
//!   responders (`recipe/request-params`, `elicitation/create`), keyed by
//!   `(family, request_id)`, answered by the unstable shim's `respond_*`
//!   intents. A reply with an unknown id cannot touch another request.
//!
//! Server→client *requests* are answered here: `session/request_permission`
//! parks the SDK responder in a registry keyed by tool call id and owned by
//! the wire that raised it (the UI answers via `respond_permission`), the
//! recipe/elicitation requests park one responder per `(family, request_id)`
//! (the UI answers via `answer_request`, or the request is auto-answered with
//! defaults when no slice registered a listener), and unknown methods get a
//! real JSON-RPC `-32601` error (desktop `serverRequest` fallback).

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use agent_client_protocol::schema::v1::{
    RequestPermissionRequest, RequestPermissionResponse, SessionNotification,
};
use agent_client_protocol::{Agent, ConnectionTo, Responder, UntypedMessage};
use serde_json::{json, Map, Value};
use tokio::sync::{oneshot, Notify};

use crate::transcript::TranscriptStore;
use crate::transport::WsTransport;
use crate::{
    ConfigOption, ConnectionStatus, CoreListener, PermissionOutcome, ServerConfig, SessionOwner,
};

pub use agent_client_protocol::Error as AcpError;

// The wire → record translators and ACP parameter builders live in `session`
// so the roam transport shares them verbatim. Re-exported here because the
// pinned seams (INTERNAL.md) name this module.
pub(crate) use crate::session::{parse_config_options, parse_sessions};

// The connection's concerns live in `wire/`; the seams below stay re-exported
// at their existing `spine::` paths.
#[path = "wire/responders.rs"]
mod responders;
#[path = "wire/status.rs"]
mod status;
#[path = "wire/handshake.rs"]
mod handshake;
#[path = "wire/rpc.rs"]
mod rpc;

pub use responders::{
    answer_request, register_notification_listener, register_server_request_listener,
    NotificationListenerGuard, PendingRequest, ServerRequestListenerGuard,
};
#[allow(unused_imports)]
pub(crate) use responders::{
    dispatch_custom_notification, install_alert_hook, park_server_request,
    park_server_request_on, register_permission_answer, register_permission_answer_for,
    request_id_from_params, respond_permission, unregister_permission_answer,
    unregister_permission_answer_owned, ParkedPermission, PermissionAnswer, PermissionReject,
};
use responders::notify_listeners;
#[allow(unused_imports)]
pub(crate) use handshake::{
    handshake_timed_out, initialize_params, session_list_params, session_new_params,
};
pub(crate) use status::reconnect_delay_ms;
pub(crate) use rpc::run_connection;


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
    /// The shared slot is also held by the global owner's answer/reject hooks.
    permission: Mutex<HashMap<String, ParkedPermission>>,
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
        // Global responder tables (permission ids shared by two wires, parked
        // server requests) need an advisory channel. Weak: a dead Core must
        // not be kept alive by a static, and a new wire replaces the hook.
        let alert_listener = Arc::downgrade(&listener);
        install_alert_hook(Arc::new(move |message| {
            if let Some(listener) = alert_listener.upgrade() {
                listener.on_status(ConnectionStatus::Alert { message });
            }
        }));
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


}


// ---------------------------------------------------------------------------
// The SDK client + transport wiring
// ---------------------------------------------------------------------------


// ---------------------------------------------------------------------------
// Wire-format helpers (the desktop's exact JSON shapes)
// ---------------------------------------------------------------------------


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
    fn duplicate_permission_id_keeps_the_first_wire_and_alerts() {
        let first = Arc::new(Mutex::new(Vec::new()));
        let first_seen = first.clone();
        let accepted = register_permission_answer_for(
            "spine-test-dup-call".into(),
            "main",
            Arc::new(move |id, _outcome| {
                first_seen.lock().push(id.to_string());
                Ok(())
            }),
            None,
        );
        assert!(accepted);

        let alerts = Arc::new(Mutex::new(Vec::new()));
        let seen_alerts = alerts.clone();
        install_alert_hook(Arc::new(move |message| seen_alerts.lock().push(message)));
        let rejected = Arc::new(Mutex::new(Vec::new()));
        let seen_reject = rejected.clone();
        let accepted = register_permission_answer_for(
            "spine-test-dup-call".into(),
            "desk", // a roam peer with the same server-chosen id
            Arc::new(|_id, _outcome| Ok(())),
            Some(Arc::new(move |error| seen_reject.lock().push(error.to_string()))),
        );
        assert!(!accepted, "a second wire must not overwrite the answer closure");
        assert!(rejected.lock()[0].contains("duplicate permission request"));
        assert!(alerts.lock().iter().any(|m| m.contains("spine-test-dup-call")));

        // Answering still reaches the FIRST owner; the duplicate never got a
        // registry entry, so its prompt cannot be answered by mistake.
        assert!(respond_permission("spine-test-dup-call", PermissionOutcome::Cancelled).is_ok());
        assert_eq!(*first.lock(), vec!["spine-test-dup-call".to_string()]);
        assert!(respond_permission("spine-test-dup-call", PermissionOutcome::Cancelled).is_err());

        unregister_permission_answer("spine-test-dup-call");
        install_alert_hook(Arc::new(|_| {})); // leave no state for other tests
    }

    #[test]
    fn form_reply_keys_are_scoped_by_family_and_unique_without_a_server_id() {
        let main = SessionOwner { kind: "main".into(), label: String::new() };
        assert_eq!(
            request_id_from_params(PendingRequest::RecipeParams, &json!({"requestId": "srv-7"}), &main),
            "main:srv-7",
            "the server id must survive, namespaced by its wire"
        );
        let roam = SessionOwner { kind: "roam".into(), label: "desk".into() };
        assert_eq!(
            request_id_from_params(PendingRequest::RecipeParams, &json!({"requestId": "srv-7"}), &roam),
            "desk:srv-7",
            "the same server id on another wire is a different request"
        );
        let first = request_id_from_params(PendingRequest::RecipeParams, &json!({}), &main);
        let second = request_id_from_params(PendingRequest::RecipeParams, &json!({}), &main);
        let elicitation = request_id_from_params(PendingRequest::Elicitation, &json!({}), &main);
        assert!(first.starts_with("main:recipe:"));
        assert!(elicitation.starts_with("main:elicit:"));
        assert_ne!(first, second, "two forms must never share one reply key");
        assert_ne!(first, elicitation);
        // An unknown key is an error, never a reply to the last form.
        assert!(!answer_request(PendingRequest::RecipeParams, &first, json!({})));
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
