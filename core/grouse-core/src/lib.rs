//! grouse-core: the stable ACP client for Grouse, built on the official
//! `agent-client-protocol` SDK.
//!
//! This is the durable surface every native UI consumes through a uniffi
//! interface. The core owns the connection, session list, active transcript,
//! caches, reconnect/backoff, and remote-change resync. UIs render state and
//! send intents; they never reimplement client logic.
//!
//! Architecture (see INTERNAL.md for the pinned seams): the uniffi surface
//! lives here — the records/enums, the `CoreListener`/`GrouseUnstableListener`
//! callback interfaces, the `Core` object (intents, status machine, reconnect
//! orchestration, remote-change resync, roam routing) — while the network
//! lives in [`spine`] (the live connection + handshake + notification
//! dispatch), [`roam`] (parallel peers), [`transcript`]/[`cache`] (the
//! stores), and [`unstable`] (the goose-fork shim). All network I/O, reply
//! dispatch, and `CoreListener` callbacks run on the core's single tokio
//! runtime ([`roam::runtime`]); the intents enqueue onto it and return.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::cache::CacheStore;
use crate::roam::{RoamPeer, active_peer};
use crate::spine::ConnectSpec;
use crate::transcript::TranscriptStore;

uniffi::setup_scaffolding!();

pub mod cache;
pub mod roam;
pub mod spine;
pub mod transcript;
pub mod transport;
pub mod unstable;

pub use transport::WsTransport;
pub use unstable::GrouseUnstable;

// ---------------------------------------------------------------------------
// Records & enums (CONTRACT §2, §3.4, §5)
// ---------------------------------------------------------------------------

/// Connection and configuration for a goosed ACP server (CONTRACT §2).
#[derive(uniffi::Record, Clone, Debug)]
pub struct ServerConfig {
    /// Hostname or IP, no scheme.
    pub host: String,
    pub port: u16,
    /// `X-Secret-Key` header value.
    pub secret_key: String,
    /// `true` -> `wss://`, `false` -> `ws://`.
    pub use_tls: bool,
    /// Absolute working directory; must exist in the goose container.
    pub cwd: String,
    pub auto_connect: bool,
    /// `_meta.client`, e.g. "grouse-desktop" | "grouse" | "grouse-cli".
    pub client_id: String,
}

/// Connection lifecycle (CONTRACT §3.3).
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Default)]
pub enum ConnectionStatus {
    #[default]
    Disconnected,
    Connecting,
    Ready,
    Syncing,
    Error { message: String },
}

/// A session list entry (CONTRACT §3.2/§3.3).
#[derive(uniffi::Record, Clone, Debug)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    /// Server timestamp; the core compares it against its transcript cache.
    pub updated_at: String,
    pub last_message_snippet: Option<String>,
}

/// One accumulated transcript bubble (CONTRACT §3.3/§4.3).
#[derive(uniffi::Record, Clone, Debug)]
pub struct Message {
    /// Bubble key (`message_id`); empty for live bubbles without an id.
    pub id: String,
    /// `user` | `agent` | `thought` | `tool` | `error`.
    pub role: String,
    pub content: String,
}

/// Transcript mutation carried by `CoreListener::on_transcript` (CONTRACT §3.2).
#[derive(uniffi::Enum, Clone, Debug)]
pub enum TranscriptEvent {
    Append { message: Message },
    Update { message: Message },
    Clear,
}

/// The flat stream event `CoreListener::on_stream` carries (CONTRACT §3.4).
#[derive(uniffi::Enum, Clone, Debug)]
pub enum StreamEvent {
    AgentChunk { text: String, message_id: String },
    UserChunk { text: String, message_id: String },
    ThoughtChunk { text: String },
    ToolCall { title: String, detail: String, tool_call_id: String, kind: ToolCallKind },
    ToolCallUpdate { id: String, status: String, output: String, live: bool },
    Usage { used: i64, size: i64, cost: f64, currency: String },
    RunEnded { stop_reason: String },
}

/// Collapses the desktop's toolgroup/chart/mcpapp split (CONTRACT §3.4).
#[derive(uniffi::Enum, Clone, Debug)]
pub enum ToolCallKind {
    Plain,
    Chart { spec: String },
    McpApp { app_key: String, uri: String, extension: String, input: String },
}

/// A config entry (`provider` | `model` | `mode` | `thinking_effort`).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct ConfigOption {
    pub id: String,
    pub value: String,
}

/// One permission option (CONTRACT §5 / inventory §3).
#[derive(uniffi::Record, Clone, Debug)]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

/// A server permission request surfaced to the UI (CONTRACT §3.2/§5).
#[derive(uniffi::Record, Clone, Debug)]
pub struct PermissionRequest {
    pub tool_call_id: String,
    pub title: String,
    pub detail: String,
    pub options: Vec<PermissionOption>,
}

/// The UI's answer to a permission request (CONTRACT §5).
#[derive(uniffi::Enum, Clone, Debug)]
pub enum PermissionOutcome {
    Selected { option_id: String },
    Cancelled,
}

/// A project/skill summary from `sources/list` (CONTRACT §5).
#[derive(uniffi::Record, Clone, Debug)]
pub struct ProjectSummary {
    pub path: String,
    pub name: String,
    pub description: Option<String>,
}

/// A prompt to send (CONTRACT §3.1). One or more content blocks.
#[derive(uniffi::Record, Clone, Debug)]
pub struct Prompt {
    pub blocks: Vec<PromptBlock>,
}

/// A prompt content block: text / image / resource (CONTRACT §3.4).
#[derive(uniffi::Enum, Clone, Debug)]
pub enum PromptBlock {
    Text { text: String },
    /// `data` is base64; `mime_type` e.g. `image/png`.
    Image { mime_type: String, data: String },
    /// `text` and `blob` are mutually exclusive; `blob` is base64.
    Resource { uri: String, mime_type: String, text: Option<String>, blob: Option<String> },
}

/// The session the UI believes is active when sending a prompt (CONTRACT §3.1).
///
/// A mismatch means the UI is showing a different chat than the socket is bound
/// to; the core rejects the send instead of mis-routing it.
#[derive(uniffi::Record, Clone, Debug)]
pub struct SendExpect {
    pub session_id: String,
}

// ---------------------------------------------------------------------------
// Callback interfaces (core -> UI)
// ---------------------------------------------------------------------------

/// Stable events, one method per family (CONTRACT §3.2).
#[uniffi::export(callback_interface)]
pub trait CoreListener: Send + Sync {
    fn on_status(&self, status: ConnectionStatus);
    fn on_sessions(&self, sessions: Vec<SessionSummary>);
    fn on_transcript(&self, event: TranscriptEvent);
    fn on_stream(&self, event: StreamEvent);
    fn on_config(&self, options: Vec<ConfigOption>);
    fn on_permission_request(&self, request: PermissionRequest);
    fn on_session_touched(&self, session_id: String, title: String, updated_at: String);
    fn on_projects(&self, projects: Vec<ProjectSummary>);
    /// A roam peer's lifecycle line (CONTRACT §6).
    fn on_roam_peer_status(&self, label: String, status: String);
    /// A roam peer's `session/list` result, ids prefixed `roam:<peer>:<id>`.
    fn on_roam_sessions(&self, label: String, sessions: Vec<SessionSummary>);
}

/// Unstable events (CONTRACT §5). Retiring with `grouse-unstable`.
#[uniffi::export(callback_interface)]
pub trait GrouseUnstableListener: Send + Sync {
    fn on_export(&self, data: String);
    fn on_recipe_params(&self, parameters: String);
    fn on_elicitation(&self, schema: String);
    fn on_compaction_status(&self, message: String);
    fn on_message_usage(
        &self,
        output_tokens: u64,
        elapsed_ms: u64,
        time_to_first_token_ms: u64,
        cost: f64,
    );
    fn on_app_resource(&self, key: String, html: String);
    // List replies ride raw-JSON strings (the shim's on_export convention):
    // the UI keeps its existing parsers. Added with the GrouseUnstable
    // implementation (CONTRACT §5, flagged: the unstable surface is retiring).
    fn on_recipes(&self, recipes: String);
    fn on_schedules(&self, schedules: String);
    fn on_projects(&self, projects: String);
    fn on_skills(&self, skills: String);
    fn on_tools(&self, session_id: String, tools: String);
    fn on_extensions(&self, extensions: String);
    fn on_session_extensions(&self, session_id: String, extensions: String);
    fn on_config_value(&self, key: String, value: String);
    fn on_supported_models(&self, provider: String, models: String);
    fn on_session_probe(&self, session_id: String, updated_at: String, message_count: i64);
    fn on_tool_result(&self, text: String, is_error: bool);
    fn on_error(&self, method: String, message: String);
}

// ---------------------------------------------------------------------------
// Core internals
// ---------------------------------------------------------------------------

/// Intents that wait for the connection to reach `Ready` (CONTRACT §4:
/// "send_prompt/set_config_option/tool queries queue until ready, then flush
/// in order").
enum PendingIntent {
    SendPrompt(Prompt, Option<SendExpect>),
    SetConfig(String, String),
}

/// The core's authoritative state, guarded by `CoreInner::state`.
#[derive(Default)]
struct CoreState {
    status: ConnectionStatus,
    sessions: Vec<SessionSummary>,
    /// Per-session cwd from `session/list` — resume-cwd resolution (never guess).
    session_cwds: HashMap<String, String>,
    /// Per-session `updatedAt` — drives cache freshness + the touched sidebar.
    session_updated_at: HashMap<String, String>,
    config: Vec<ConfigOption>,
    /// The server the connection (and reconnects) use.
    last_config: Option<ServerConfig>,
    /// A turn is in flight on the main connection; the pending queue waits.
    prompting: bool,
    pending: VecDeque<PendingIntent>,
    /// Exponential backoff state (500ms·2^n, cap 15s, 6 attempts; reset on Ready).
    reconnect_attempts: u32,
    /// Bumped to invalidate pending reconnect timers (a newer connect wins).
    reconnect_gen: u64,
    /// Bumped per connection so a superseded connection's teardown is inert.
    conn_gen: u64,
    /// Explicit `disconnect()`: no reconnect, and statuses surface Disconnected.
    user_disconnect: bool,
    /// Remote-change resync: session_info_update debounce generation.
    touch_gen: u64,
    /// Follow-up probes left in the current resync cycle (desktop m_resyncTicks).
    resync_ticks: i32,
    /// Last probed (updatedAt, messageCount) — the "did it move?" comparison.
    sync_stamp: Option<(String, i64)>,
}

struct CoreInner {
    listener: Arc<dyn CoreListener>,
    /// The shared transcript store (main connection streams into it; the
    /// `transcript()` getter reads it unless a roam peer owns the chat).
    store: Arc<TranscriptStore>,
    /// Dumb per-session transcript I/O under a default data dir (CONTRACT §7.4;
    /// the skeleton's constructor carries no CacheDir, so the core defaults it).
    cache: CacheStore,
    state: Mutex<CoreState>,
    /// The live main connection + its task (recreated per connect).
    conn: Mutex<Option<Arc<crate::spine::Conn>>>,
    conn_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The roam peer registry (CONTRACT §6); chat routes to the last-opened
    /// session's owner.
    peers: Mutex<Vec<Arc<RoamPeer>>>,
    active_peer_label: Arc<RwLock<Option<String>>>,
}

/// The stable interface (CONTRACT §3).
#[derive(uniffi::Object, Clone)]
pub struct Core {
    inner: Arc<CoreInner>,
}

#[uniffi::export]
impl Core {
    /// Construct the core. The UI's `CacheDir` (CONTRACT §7.4) is not part of
    /// the skeleton constructor, so the core uses a default data dir; the
    /// cache is created lazily on first write.
    #[uniffi::constructor]
    pub fn new(listener: Box<dyn CoreListener>) -> Arc<Self> {
        let listener: Arc<dyn CoreListener> = Arc::from(listener);
        // The store shares one listener with the rest of the core; a tiny
        // forwarder adapts the Box the store's seam asks for.
        let store = Arc::new(TranscriptStore::new(Box::new(CoreListenerForwarder(
            listener.clone(),
        ))));
        Arc::new(Self {
            inner: Arc::new(CoreInner {
                listener,
                store,
                cache: CacheStore::new(default_cache_dir()),
                state: Mutex::new(CoreState::default()),
                conn: Mutex::new(None),
                conn_task: Mutex::new(None),
                peers: Mutex::new(Vec::new()),
                active_peer_label: Arc::new(RwLock::new(None)),
            }),
        })
    }

    // -- intents (CONTRACT §3.1): fire-and-forget into the core's runtime. --

    /// Open the WebSocket, `initialize`, then new-or-resume per §4. Blocks
    /// (bounded) until the connection is ready or fails — the one blocking
    /// intent (INTERNAL.md threading model).
    pub fn connect(&self, config: ServerConfig) {
        let (_, ready_rx) = self.connect_impl(config, ConnectSpec::New { recipe_id: None }, false);
        self.wait_ready(ready_rx);
    }

    /// Explicit close: no reconnect (CONTRACT §3.1).
    pub fn disconnect(&self) {
        {
            let mut state = self.inner.state.lock();
            state.user_disconnect = true;
            state.reconnect_gen += 1;
            state.prompting = false;
            state.sync_stamp = None;
            state.resync_ticks = 0;
        }
        crate::spine::set_current_conn(None);
        if let Some(conn) = self.inner.conn.lock().take() {
            conn.shutdown();
        }
        if let Some(task) = self.inner.conn_task.lock().take() {
            drop(task); // the task ends after the graceful close
        }
        self.emit_status(ConnectionStatus::Disconnected);
        self.inner.store.clear();
    }

    /// `session/new` with `_meta.client` + cwd; replaces the current wire.
    pub fn new_session(&self, recipe_id: Option<String>) {
        *self.inner.active_peer_label.write() = None;
        self.reset_chat_state();
        let config = self.inner.state.lock().last_config.clone();
        let Some(config) = config else { return };
        let (_, _rx) =
            self.connect_impl(config, ConnectSpec::New { recipe_id }, false);
    }

    /// `session/load` with the session's real cwd (resolved: cache → probe →
    /// session/list, never guessed); a fresh cached transcript renders
    /// instantly, otherwise the load replays it.
    pub fn open_session(&self, session_id: String) {
        *self.inner.active_peer_label.write() = None;
        self.reset_chat_state();
        let cwd = self.resolve_cwd(&session_id);
        let (suppress, cached) = match self.inner.cache.load_transcript(&session_id) {
            Some((messages, cached_at)) => {
                let updated = self
                    .inner
                    .state
                    .lock()
                    .session_updated_at
                    .get(&session_id)
                    .cloned()
                    .unwrap_or_default();
                let fresh = !updated.is_empty() && updated == cached_at;
                (fresh, Some(messages))
            }
            None => (false, None),
        };
        if suppress {
            if let Some(messages) = cached {
                self.inner.store.replace(messages);
            }
        } else {
            self.inner.store.clear();
        }
        let config = self.inner.state.lock().last_config.clone();
        let Some(config) = config else { return };
        let (_, _rx) = self.connect_impl(
            config,
            ConnectSpec::Resume { session_id, cwd },
            suppress,
        );
    }

    /// Refresh `session/list` (reply → `on_sessions`).
    pub fn list_sessions(&self) {
        let Some(conn) = self.inner.conn.lock().clone() else { return };
        let core = self.clone();
        crate::roam::runtime().spawn(async move {
            if let Ok(reply) = conn
                .rpc_async("session/list", crate::spine::session_list_params())
                .await
            {
                core.on_sessions_reply(reply);
            }
        });
    }

    /// Send a prompt (text/image/resource blocks). Queues until the socket is
    /// ready; rejected (not queued) when `expect` mismatches the bound
    /// session. Routes to the active roam peer when one owns the chat.
    pub fn send_prompt(&self, prompt: Prompt, expect: Option<SendExpect>) {
        if let Some(peer) = self.active_peer() {
            let params = prompt_params(
                &prompt,
                &peer.active_session_id().unwrap_or_default(),
            );
            crate::roam::runtime().spawn_blocking(move || {
                let _ = peer.rpc("session/prompt", params);
            });
            return;
        }
        match self.try_send_prompt(prompt, expect) {
            Ok(()) => {}
            Err((prompt, expect)) => {
                self.inner.state.lock().pending.push_back(PendingIntent::SendPrompt(prompt, expect));
            }
        }
    }

    /// `session/cancel` (a notification; never waits for a reply).
    pub fn cancel(&self) {
        if let Some(peer) = self.active_peer() {
            let params = json!({ "sessionId": peer.active_session_id().unwrap_or_default() });
            crate::roam::runtime().spawn_blocking(move || {
                let _ = peer.notify("session/cancel", params);
            });
            return;
        }
        let Some(conn) = self.inner.conn.lock().clone() else { return };
        let Some(sid) = conn.active_session_id() else { return };
        let _ = conn.notify("session/cancel", json!({ "sessionId": sid }));
    }

    /// `session/set_config_option`; queues until ready (CONTRACT §4).
    pub fn set_config_option(&self, config_id: String, value: String) {
        if let Some(peer) = self.active_peer() {
            let params = json!({
                "sessionId": peer.active_session_id().unwrap_or_default(),
                "configId": config_id,
                "value": value,
            });
            crate::roam::runtime().spawn_blocking(move || {
                let _ = peer.rpc("session/set_config_option", params);
            });
            return;
        }
        match self.try_set_config(config_id, value) {
            Ok(()) => {}
            Err((config_id, value)) => {
                self.inner.state.lock().pending.push_back(PendingIntent::SetConfig(config_id, value));
            }
        }
    }

    /// Rename a session; the reply re-lists sessions (desktop response table).
    pub fn rename_session(&self, session_id: String, title: String) {
        self.mutate_session(
            "_goose/unstable/session/rename",
            json!({ "sessionId": session_id, "title": title }),
        );
    }

    /// Archive a session (out of `session/list`, history stays on disk); re-lists.
    pub fn archive_session(&self, session_id: String) {
        self.mutate_session(
            "_goose/unstable/session/archive",
            json!({ "sessionId": session_id }),
        );
    }

    /// Restore an archived session; re-lists.
    pub fn unarchive_session(&self, session_id: String) {
        self.mutate_session(
            "_goose/unstable/session/unarchive",
            json!({ "sessionId": session_id }),
        );
    }

    /// Delete a session outright (irreversible); re-lists. If it was the open
    /// chat, the transcript view is dropped too.
    pub fn delete_session(&self, session_id: String) {
        if self.active_session_id().as_deref() == Some(session_id.as_str()) {
            self.inner.store.clear();
        }
        self.mutate_session("session/delete", json!({ "sessionId": session_id }));
    }

    /// Connect a roam peer in browse mode (CONTRACT §6). The peer's identity
    /// is generated + persisted on first use.
    pub fn roam_connect(&self, card: String, label: String) {
        let mut peers = self.inner.peers.lock();
        peers.retain(|peer| {
            if peer.label() == label {
                peer.close();
                false
            } else {
                true
            }
        });
        let secret = self.roam_identity();
        let active = self.inner.active_peer_label.clone();
        let gate_label = label.clone();
        let is_active: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            *active.read() == Some(gate_label.clone())
        });
        let peer = RoamPeer::connect(secret, card, label, self.inner.listener.clone(), is_active);
        peers.push(peer);
    }

    /// Disconnect a roam peer (parallel peers stay live; only the named one
    /// is closed).
    pub fn roam_disconnect(&self, label: String) {
        let mut peers = self.inner.peers.lock();
        peers.retain(|peer| {
            if peer.label() == label {
                peer.close();
                false
            } else {
                true
            }
        });
        let mut active = self.inner.active_peer_label.write();
        if *active == Some(label) {
            *active = None;
        }
    }

    /// Open a session on a roam peer; the peer becomes the chat owner until a
    /// Main session is opened. Its transcript is peer-owned (never cached).
    pub fn roam_open_session(&self, label: String, session_id: String) {
        let peer = self
            .inner
            .peers
            .lock()
            .iter()
            .find(|peer| peer.label() == label)
            .cloned();
        let Some(peer) = peer else { return };
        *self.inner.active_peer_label.write() = Some(label);
        // Peer sessions are not in the main session list; the working dir is
        // the only known-good cwd (session/load rewrites working_dir when the
        // cwd differs, so the caller's pick wins on the peer side).
        let cwd = self
            .inner
            .state
            .lock()
            .last_config
            .as_ref()
            .map(|config| config.cwd.clone())
            .unwrap_or_default();
        peer.open_session(session_id, cwd);
    }

    /// Answer a permission request (CONTRACT §5); routes to the active peer
    /// when one owns the chat.
    pub fn respond_permission(&self, tool_call_id: String, outcome: PermissionOutcome) {
        if let Some(peer) = self.active_peer() {
            let _ = peer.respond_permission(outcome);
            return;
        }
        if let Some(conn) = self.inner.conn.lock().clone() {
            let _ = conn.respond_permission(&tool_call_id, outcome);
        }
    }

    // -- getters (CONTRACT §3.3): immutable snapshots owned by the core. --

    pub fn status(&self) -> ConnectionStatus {
        self.inner.state.lock().status.clone()
    }

    /// `ready` ⇔ an open session id exists (CONTRACT §4).
    pub fn ready(&self) -> bool {
        self.active_session_id().is_some()
    }

    /// The bound session of the active chat (the roam peer's, when one owns
    /// the chat).
    pub fn active_session_id(&self) -> Option<String> {
        if let Some(peer) = self.active_peer() {
            return peer.active_session_id();
        }
        self.inner.conn.lock().as_ref().and_then(|conn| conn.active_session_id())
    }

    pub fn sessions(&self) -> Vec<SessionSummary> {
        self.inner.state.lock().sessions.clone()
    }

    /// The accumulated active-session transcript (peer-owned when a roam peer
    /// owns the chat).
    pub fn transcript(&self) -> Vec<Message> {
        if let Some(peer) = self.active_peer() {
            return peer.transcript();
        }
        self.inner.store.transcript()
    }

    pub fn config(&self) -> Vec<ConfigOption> {
        self.inner.state.lock().config.clone()
    }
}

// ---------------------------------------------------------------------------
// Core internals: connection lifecycle, status machine, reconnect, resync
// ---------------------------------------------------------------------------

impl Core {
    /// Reset the per-chat state a fresh open/new starts from (desktop
    /// openSession/newChat: clear the queue, drop the prompt flag, cancel any
    /// in-flight resync cycle).
    fn reset_chat_state(&self) {
        let mut state = self.inner.state.lock();
        state.pending.clear();
        state.prompting = false;
        state.resync_ticks = 0;
        state.sync_stamp = None;
        state.touch_gen += 1; // stale debounces no-op
    }

    /// Build a fresh connection: gracefully shut down any current one, wire
    /// the hooks, register the conn for the unstable shim, and spawn the
    /// connection task. Returns the conn and the ready receiver (the bounded
    /// `connect()` waits on it; reconnect/timer paths ignore it).
    fn connect_impl(
        &self,
        config: ServerConfig,
        spec: ConnectSpec,
        suppress_replay: bool,
    ) -> (Arc<crate::spine::Conn>, oneshot::Receiver<Result<(), String>>) {
        let gen = {
            let mut state = self.inner.state.lock();
            state.conn_gen += 1;
            state.conn_gen
        };
        // Graceful teardown of any previous wire.
        if let Some(old) = self.inner.conn.lock().take() {
            old.shutdown();
        }
        if let Some(task) = self.inner.conn_task.lock().take() {
            drop(task); // the old task finishes after its graceful close
        }

        let (conn, ready_rx) = crate::spine::Conn::new(
            self.inner.listener.clone(),
            self.inner.store.clone(),
            config.clone(),
            spec,
        );
        conn.set_suppress_replay(suppress_replay);
        conn.set_on_status(self.status_hook());
        conn.set_on_touched(self.touched_hook());
        conn.set_on_config(self.config_hook());
        conn.set_on_ended(self.ended_hook(gen));
        crate::spine::set_current_conn(Some(conn.clone()));

        let transport =
            WsTransport::new(&config.host, config.port, &config.secret_key, config.use_tls);
        let task_conn = conn.clone();
        let task = crate::roam::runtime().spawn(async move {
            let result = crate::spine::run_connection(task_conn.clone(), transport).await;
            task_conn.on_connection_ended(result);
        });

        {
            let mut state = self.inner.state.lock();
            state.last_config = Some(config);
            state.user_disconnect = false;
            state.reconnect_gen += 1; // a fresh connect supersedes pending timers
            state.reconnect_attempts = 0;
        }
        *self.inner.conn.lock() = Some(conn.clone());
        *self.inner.conn_task.lock() = Some(task);
        (conn, ready_rx)
    }

    /// Block (bounded) until the connection is ready or fails. The reconnect
    /// path runs inside the runtime and never blocks.
    fn wait_ready(&self, ready_rx: oneshot::Receiver<Result<(), String>>) {
        if tokio::runtime::Handle::try_current().is_ok() {
            return;
        }
        let rt = crate::roam::runtime();
        let _ = rt.block_on(async {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(15), ready_rx).await;
        });
    }

    /// Mirror a conn status transition: emit it (Disconnected when an explicit
    /// disconnect wins), reset the reconnect budget on Ready.
    fn on_conn_status(&self, status: ConnectionStatus) {
        let mut state = self.inner.state.lock();
        if state.user_disconnect {
            drop(state);
            self.emit_status(ConnectionStatus::Disconnected);
            return;
        }
        state.status = status.clone();
        if matches!(status, ConnectionStatus::Ready) {
            state.reconnect_attempts = 0;
            state.reconnect_gen += 1;
        }
        drop(state);
        self.inner.listener.on_status(status);
    }

    /// A connection ended. Explicit disconnect → nothing more (status already
    /// surfaced as Disconnected). Superseded connection → inert. Otherwise:
    /// unexpected drop → clear the registry and schedule the backoff
    /// reconnect when a session was bound (resume it), else stay Error.
    fn on_connection_ended(&self, gen: u64, _result: Result<(), String>) {
        let state = self.inner.state.lock();
        if state.user_disconnect {
            drop(state);
            crate::spine::set_current_conn(None);
            return;
        }
        if state.conn_gen != gen {
            drop(state);
            return;
        }
        let resume = self
            .inner
            .conn
            .lock()
            .as_ref()
            .and_then(|conn| conn.active_session_id());
        drop(state);
        crate::spine::set_current_conn(None);
        if let Some(session_id) = resume {
            self.schedule_reconnect(session_id);
        }
    }

    /// Exponential backoff: 500ms·2^n capped at 15s, at most 6 attempts; reset
    /// on Ready (desktop maybeReconnect). Surfaced only via `on_status`.
    fn schedule_reconnect(&self, resume: String) {
        let mut state = self.inner.state.lock();
        if state.user_disconnect {
            return;
        }
        if state.reconnect_attempts >= 6 {
            drop(state);
            self.emit_status(ConnectionStatus::Error {
                message: "connection lost; giving up after 6 reconnect attempts".to_string(),
            });
            return;
        }
        let delay_ms = 500u64 * (1u64 << state.reconnect_attempts.min(5)).min(15000 / 500);
        state.reconnect_gen += 1;
        let gen = state.reconnect_gen;
        drop(state);
        let core = self.clone();
        crate::roam::runtime().spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            core.on_reconnect_timer(gen, resume);
        });
    }

    fn on_reconnect_timer(&self, gen: u64, resume: String) {
        let mut state = self.inner.state.lock();
        if state.user_disconnect || state.reconnect_gen != gen {
            return;
        }
        state.reconnect_attempts += 1;
        let config = state.last_config.clone();
        drop(state);
        let Some(config) = config else { return };
        let cwd = self.resolve_cwd(&resume);
        let (_, _rx) = self.connect_impl(
            config,
            ConnectSpec::Resume { session_id: resume, cwd },
            false,
        );
    }

    /// Resolve a session's real cwd: last-known (session/list / previous
    /// open) → `_goose/unstable/session/info` probe → the user's working dir.
    /// Never guess (CONTRACT §4 footgun note).
    fn resolve_cwd(&self, session_id: &str) -> String {
        if let Some(cwd) = self.inner.state.lock().session_cwds.get(session_id).cloned() {
            if !cwd.is_empty() {
                return cwd;
            }
        }
        if let Some(conn) = self.inner.conn.lock().clone() {
            let probe = conn.rpc(
                "_goose/unstable/session/info",
                json!({ "sessionId": session_id }),
            );
            if let Ok(reply) = probe {
                if let Some(cwd) = reply.pointer("/session/cwd").and_then(Value::as_str) {
                    if !cwd.is_empty() {
                        self.inner
                            .state
                            .lock()
                            .session_cwds
                            .insert(session_id.to_string(), cwd.to_string());
                        return cwd.to_string();
                    }
                }
            }
        }
        self.inner
            .state
            .lock()
            .last_config
            .as_ref()
            .map(|config| config.cwd.clone())
            .unwrap_or_default()
    }

    // -- prompt / config plumbing --------------------------------------------

    fn try_send_prompt(
        &self,
        prompt: Prompt,
        expect: Option<SendExpect>,
    ) -> Result<(), (Prompt, Option<SendExpect>)> {
        let Some(conn) = self.inner.conn.lock().clone() else {
            return Err((prompt, expect));
        };
        let Some(sid) = conn.active_session_id() else {
            return Err((prompt, expect));
        };
        if let Some(exp) = &expect {
            if exp.session_id != sid {
                // The UI is showing a different chat than the socket is bound
                // to; reject instead of mis-routing (CONTRACT §3.1). The UI
                // observes the mismatch through its getters.
                eprintln!(
                    "grouse-core: send_prompt rejected — UI expects session {:?}, socket bound to {:?}",
                    exp.session_id, sid
                );
                return Ok(());
            }
        }
        self.spawn_prompt(conn, sid, prompt);
        Ok(())
    }

    fn try_set_config(&self, config_id: String, value: String) -> Result<(), (String, String)> {
        let Some(conn) = self.inner.conn.lock().clone() else {
            return Err((config_id, value));
        };
        let Some(sid) = conn.active_session_id() else {
            return Err((config_id, value));
        };
        self.spawn_set_config(conn, sid, config_id, value);
        Ok(())
    }

    fn spawn_prompt(&self, conn: Arc<crate::spine::Conn>, session_id: String, prompt: Prompt) {
        let params = prompt_params(&prompt, &session_id);
        let core = self.clone();
        let store = self.inner.store.clone();
        self.inner.state.lock().prompting = true;
        crate::roam::runtime().spawn(async move {
            let result = conn.rpc_async("session/prompt", params).await;
            match &result {
                Ok(reply) => {
                    let stop = reply
                        .get("stopReason")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    store.run_ended(&stop);
                }
                Err(error) => {
                    // No stable error event exists; surface the failure as the
                    // turn's end so the queue flushes (the desktop wedges
                    // here — this is deliberately more robust).
                    eprintln!("grouse-core: session/prompt failed: {error}");
                    store.run_ended("error");
                }
            }
            core.on_prompt_done();
        });
    }

    fn spawn_set_config(&self, conn: Arc<crate::spine::Conn>, session_id: String, config_id: String, value: String) {
        let core = self.clone();
        crate::roam::runtime().spawn(async move {
            let params = json!({
                "sessionId": session_id,
                "configId": config_id,
                "value": value,
            });
            if let Ok(reply) = conn.rpc_async("session/set_config_option", params).await {
                let options = crate::spine::parse_config_options(&reply);
                core.on_config_reply(options);
            }
        });
    }

    fn on_prompt_done(&self) {
        self.inner.state.lock().prompting = false;
        self.save_cache();
        self.flush_pending();
    }

    /// Drain the pending queue in order once the socket is ready (CONTRACT §4).
    fn flush_pending(&self) {
        loop {
            let popped = {
                let mut state = self.inner.state.lock();
                if state.prompting {
                    return;
                }
                state.pending.pop_front()
            };
            let Some(intent) = popped else { return };
            let requeue = match intent {
                PendingIntent::SendPrompt(prompt, expect) => {
                    match self.try_send_prompt(prompt, expect) {
                        Ok(()) => None,
                        Err((prompt, expect)) => Some(PendingIntent::SendPrompt(prompt, expect)),
                    }
                }
                PendingIntent::SetConfig(config_id, value) => {
                    match self.try_set_config(config_id, value) {
                        Ok(()) => None,
                        Err((config_id, value)) => Some(PendingIntent::SetConfig(config_id, value)),
                    }
                }
            };
            if let Some(intent) = requeue {
                self.inner.state.lock().pending.push_front(intent);
                return;
            }
        }
    }

    /// Persist the accumulated transcript under the current session's
    /// updatedAt (the freshness check on the next open).
    fn save_cache(&self) {
        let Some(session_id) = self.active_session_id() else { return };
        let messages = self.inner.store.transcript();
        let updated_at = self
            .inner
            .state
            .lock()
            .session_updated_at
            .get(&session_id)
            .cloned()
            .unwrap_or_default();
        self.inner.cache.save_transcript(&session_id, &messages, &updated_at);
    }

    fn on_sessions_reply(&self, reply: Value) {
        let (sessions, cwds, updated) = crate::spine::parse_sessions(&reply);
        {
            let mut state = self.inner.state.lock();
            state.sessions = sessions.clone();
            for (sid, cwd) in cwds {
                state.session_cwds.insert(sid, cwd);
            }
            for (sid, updated_at) in updated {
                state.session_updated_at.insert(sid, updated_at);
            }
        }
        self.inner.listener.on_sessions(sessions);
    }

    fn on_config_reply(&self, options: Vec<ConfigOption>) {
        self.inner.state.lock().config = options.clone();
        self.inner.listener.on_config(options);
    }

    fn mutate_session(&self, method: &str, params: Value) {
        let Some(conn) = self.inner.conn.lock().clone() else { return };
        let core = self.clone();
        let method = method.to_string();
        crate::roam::runtime().spawn(async move {
            // Mutations carry no useful reply body; re-list so the sidebar
            // reflects them (desktop response() table).
            if conn.rpc_async(&method, params).await.is_ok() {
                core.list_sessions();
            }
        });
    }

    // -- remote-change resync (CONTRACT §4) -----------------------------------

    /// `session_info_update` → `on_session_touched` + a debounced resync of
    /// the ACTIVE session (another client bumped it); other sessions just
    /// refresh the sidebar list.
    fn on_session_touched(&self, session_id: String, title: String, updated_at: String) {
        {
            let mut state = self.inner.state.lock();
            if !updated_at.is_empty() {
                state.session_updated_at.insert(session_id.clone(), updated_at.clone());
            }
        }
        self.inner
            .listener
            .on_session_touched(session_id.clone(), title, updated_at);
        if self.active_session_id().as_deref() == Some(session_id.as_str()) {
            self.schedule_touch_debounce(session_id);
        } else {
            self.list_sessions();
        }
    }

    /// 1.5s debounce: a remote turn can bump a session repeatedly; each bump
    /// must not trigger a full resync (desktop m_touchDebounce).
    fn schedule_touch_debounce(&self, session_id: String) {
        let gen = {
            let mut state = self.inner.state.lock();
            state.touch_gen += 1;
            state.touch_gen
        };
        let core = self.clone();
        crate::roam::runtime().spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            core.on_touch_debounced(gen, session_id);
        });
    }

    fn on_touch_debounced(&self, gen: u64, session_id: String) {
        let mut state = self.inner.state.lock();
        if state.touch_gen != gen {
            return;
        }
        // Our own turn owns the transcript; the session re-syncs on open
        // (desktop probeAndMaybeResync).
        if state.prompting {
            return;
        }
        if self.active_session_id().as_deref() != Some(session_id.as_str()) {
            return;
        }
        state.resync_ticks = 4; // initial probe + 3 follow-ups
        drop(state);
        self.probe_and_maybe_resync(session_id);
    }

    fn probe_and_maybe_resync(&self, session_id: String) {
        let Some(conn) = self.inner.conn.lock().clone() else { return };
        let core = self.clone();
        crate::roam::runtime().spawn(async move {
            let probe = conn
                .rpc_async(
                    "_goose/unstable/session/info",
                    json!({ "sessionId": session_id }),
                )
                .await
                .ok()
                .and_then(|reply| reply.get("session").cloned())
                .map(|session| {
                    let updated_at = session
                        .get("updatedAt")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let message_count = session
                        .pointer("/_meta/messageCount")
                        .and_then(Value::as_i64)
                        .unwrap_or(-1);
                    (updated_at, message_count)
                });
            core.on_session_probe(session_id, probe);
        });
    }

    /// The probe moved (updatedAt/messageCount differ) → replay the session
    /// in place, then re-probe at 8s a few times to catch a turn that is still
    /// streaming on the prompting client (desktop onSessionProbe +
    /// turnResyncTick; one implementation replaces both).
    fn on_session_probe(&self, session_id: String, probe: Option<(String, i64)>) {
        let mut state = self.inner.state.lock();
        if self.active_session_id().as_deref() != Some(session_id.as_str()) {
            return; // stale probe for a session we left
        }
        let Some((updated_at, message_count)) = probe else {
            return; // probe failed; leave the transcript as-is
        };
        if updated_at.is_empty() || message_count < 0 {
            return;
        }
        let moved = match &state.sync_stamp {
            Some((stamp_at, stamp_count)) => {
                *stamp_at != updated_at || *stamp_count != message_count
            }
            None => true,
        };
        if !moved {
            return;
        }
        state.sync_stamp = Some((updated_at.clone(), message_count));
        if state.resync_ticks > 0 {
            state.resync_ticks -= 1;
        }
        let follow_up = state.resync_ticks > 0;
        drop(state);
        self.resync_current_session(&session_id);
        if follow_up {
            let core = self.clone();
            crate::roam::runtime().spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                core.probe_and_maybe_resync(session_id);
            });
        }
    }

    /// In-place replay: same state prep as open_session's stale path, minus
    /// the reconnect — the wire is already live (desktop resyncCurrentSession).
    fn resync_current_session(&self, session_id: &str) {
        let Some(conn) = self.inner.conn.lock().clone() else { return };
        let cwd = self.resolve_cwd(session_id);
        self.inner.store.clear();
        conn.set_replaying(true);
        self.emit_status(ConnectionStatus::Syncing);
        let core = self.clone();
        let session_id = session_id.to_string();
        crate::roam::runtime().spawn(async move {
            let params = json!({
                "sessionId": session_id,
                "cwd": cwd,
                "mcpServers": [],
            });
            match conn.rpc_async("session/load", params).await {
                Ok(reply) => {
                    conn.set_replaying(false);
                    let options = crate::spine::parse_config_options(&reply);
                    core.on_config_reply(options);
                    core.emit_status(ConnectionStatus::Ready);
                    core.list_sessions();
                }
                Err(error) => {
                    conn.set_replaying(false);
                    core.emit_status(ConnectionStatus::Error {
                        message: format!("resync failed: {error}"),
                    });
                }
            }
        });
    }

    // -- hooks ----------------------------------------------------------------

    fn status_hook(&self) -> Arc<dyn Fn(ConnectionStatus) + Send + Sync> {
        let core = self.clone();
        Arc::new(move |status| core.on_conn_status(status))
    }

    fn touched_hook(&self) -> Arc<dyn Fn(String, String, String) + Send + Sync> {
        let core = self.clone();
        Arc::new(move |session_id, title, updated_at| {
            core.on_session_touched(session_id, title, updated_at)
        })
    }

    fn config_hook(&self) -> Arc<dyn Fn(Vec<ConfigOption>) + Send + Sync> {
        let core = self.clone();
        Arc::new(move |options| core.on_config_reply(options))
    }

    fn ended_hook(&self, gen: u64) -> Arc<dyn Fn(Result<(), String>) + Send + Sync> {
        let core = self.clone();
        Arc::new(move |result| core.on_connection_ended(gen, result))
    }

    fn emit_status(&self, status: ConnectionStatus) {
        self.inner.state.lock().status = status.clone();
        self.inner.listener.on_status(status);
    }

    // -- roam helpers ---------------------------------------------------------

    fn active_peer(&self) -> Option<Arc<RoamPeer>> {
        let peers = self.inner.peers.lock();
        let active = self.inner.active_peer_label.read().clone();
        active_peer(&peers, &active).cloned()
    }

    /// The device's roam identity (grouse-roam-core), generated + persisted
    /// under the cache dir on first use (desktop m_store roam_identity).
    fn roam_identity(&self) -> String {
        let dir = default_cache_dir();
        let path = dir.join("roam_identity");
        if let Ok(existing) = std::fs::read_to_string(&path) {
            let existing = existing.trim();
            if !existing.is_empty() {
                return existing.to_string();
            }
        }
        let secret = grouse_roam_core::identity_generate();
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(&path, &secret);
        secret
    }
}

// ---------------------------------------------------------------------------
// Wire-format helpers
// ---------------------------------------------------------------------------

/// Turn `Prompt` content blocks into the ACP `session/prompt` params
/// (desktop sendPrompt: text/image/resource shapes pass through verbatim).
fn prompt_params(prompt: &Prompt, session_id: &str) -> Value {
    let blocks = prompt
        .blocks
        .iter()
        .map(|block| match block {
            PromptBlock::Text { text } => json!({ "type": "text", "text": text }),
            PromptBlock::Image { mime_type, data } => {
                json!({ "type": "image", "mimeType": mime_type, "data": data })
            }
            PromptBlock::Resource { uri, mime_type, text, blob } => {
                let mut resource = json!({ "uri": uri, "mimeType": mime_type });
                if let Some(text) = text {
                    resource["text"] = Value::String(text.clone());
                }
                if let Some(blob) = blob {
                    resource["blob"] = Value::String(blob.clone());
                }
                json!({ "type": "resource", "resource": resource })
            }
        })
        .collect::<Vec<_>>();
    json!({ "sessionId": session_id, "prompt": blocks })
}

/// The cache + identity directory (CONTRACT §7.4 says the UI supplies a
/// CacheDir at construction, but the skeleton constructor does not carry one;
/// the core defaults to the platform data dir).
fn default_cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(dir).join("grouse");
    }
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".local/share/grouse"))
        .unwrap_or_else(|| PathBuf::from(".").join("grouse-cache"))
}

/// Adapts the store's `Box<dyn CoreListener>` seam to the shared listener
/// `Arc` the core keeps (the store emits `on_stream`/`on_transcript` through
/// it; all methods forward).
struct CoreListenerForwarder(Arc<dyn CoreListener>);

impl CoreListener for CoreListenerForwarder {
    fn on_status(&self, status: ConnectionStatus) {
        self.0.on_status(status);
    }
    fn on_sessions(&self, sessions: Vec<SessionSummary>) {
        self.0.on_sessions(sessions);
    }
    fn on_transcript(&self, event: TranscriptEvent) {
        self.0.on_transcript(event);
    }
    fn on_stream(&self, event: StreamEvent) {
        self.0.on_stream(event);
    }
    fn on_config(&self, options: Vec<ConfigOption>) {
        self.0.on_config(options);
    }
    fn on_permission_request(&self, request: PermissionRequest) {
        self.0.on_permission_request(request);
    }
    fn on_session_touched(&self, session_id: String, title: String, updated_at: String) {
        self.0.on_session_touched(session_id, title, updated_at);
    }
    fn on_projects(&self, projects: Vec<ProjectSummary>) {
        self.0.on_projects(projects);
    }
    fn on_roam_peer_status(&self, label: String, status: String) {
        self.0.on_roam_peer_status(label, status);
    }
    fn on_roam_sessions(&self, label: String, sessions: Vec<SessionSummary>) {
        self.0.on_roam_sessions(label, sessions);
    }
}
