// SPDX-License-Identifier: AGPL-3.0-or-later

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
pub mod capi;
pub mod notify;
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
#[derive(uniffi::Record, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ServerConfig {
    /// Hostname or IP, no scheme.
    pub host: String,
    pub port: u16,
    /// `X-Secret-Key` header value.
    pub secret_key: String,
    /// `true` -> `wss://`, `false` -> `ws://`.
    pub use_tls: bool,
    /// `true` -> accept any server certificate (historical trust-all). When
    /// `false` (the default) the transport verifies the chain and hostname
    /// against WebPKI roots plus any `ca_cert_pem`. Set on a self-signed host.
    pub accept_invalid_certs: bool,
    /// PEM-encoded CA certificate(s) added to the verifier's trust store when
    /// verifying (ignored when `accept_invalid_certs` is `true`).
    pub ca_cert_pem: Option<String>,
    /// Absolute working directory; must exist in the goose container.
    pub cwd: String,
    pub auto_connect: bool,
    /// `_meta.client`, e.g. "grouse-desktop" | "grouse" | "grouse-cli".
    pub client_id: String,
    /// Start the fresh session as a recipe session (`session/new` recipeId),
    /// for recipe runs on a cold start (gap 4: no connect happened yet).
    pub initial_recipe_id: Option<String>,
}

/// Connection lifecycle (CONTRACT §3.3).
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub enum ConnectionStatus {
    #[default]
    Disconnected,
    Connecting,
    Ready,
    Syncing,
    Error { message: String },
}

/// A session list entry (CONTRACT §3.2/§3.3).
#[derive(uniffi::Record, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    /// Server timestamp; the core compares it against its transcript cache.
    pub updated_at: String,
    pub last_message_snippet: Option<String>,
    /// The session's project, from the reply's `_meta.projectId` (absent for
    /// un-filed sessions and for roam peer sessions).
    pub project_id: Option<String>,
    /// From the reply's `_meta.messageCount`.
    pub message_count: i64,
    /// From the reply's `_meta.model`.
    pub model: String,
    /// From the reply's `_meta.hasRecipe`.
    pub has_recipe: bool,
    /// True while the session has backgrounded (staged) content the UI has
    /// not shown yet — the green-dot indicator (roam staging, serve parity).
    pub has_new: bool,
    /// True when the session is archived (goose stamps `_meta.archivedAt` and
    /// `session/list` has no archived filter — the flag lets UIs list and
    /// restore archived chats instead of dropping them). Always false for
    /// roam peer sessions.
    pub archived: bool,
}

/// One accumulated transcript bubble (CONTRACT §3.3/§4.3).
#[derive(uniffi::Record, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Message {
    /// Bubble key (`message_id`); empty for live bubbles without an id.
    pub id: String,
    /// `user` | `agent` | `thought` | `tool` | `error`.
    pub role: String,
    /// Bubble text. For `tool` this is the TITLE ONLY (serve shape); the
    /// tool's output lives in [`Message::output`], delivered separately so a
    /// chip never renders the result in its header.
    pub content: String,
    /// Tool-role only: the tool's output/result text (live chunks appended,
    /// completion replaces). Empty for other roles and for serve's transcript
    /// projection (serve streams output separately, §4).
    pub output: String,
}

/// The kind of a rich transcript [`Item`] (docs/TRANSCRIPT_MODEL.md). One
/// variant per thing a client draws, so a chart stays a chart and an MCP app
/// keeps its identity through replay and restart.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ItemKind {
    User,
    Agent,
    Thought,
    Tool,
    ToolGroup,
    Chart,
    McpApp,
    Error,
}

/// One tool call inside a [`ItemKind::ToolGroup`] item.
#[derive(uniffi::Record, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub title: String,
    /// The tool's input/arguments (desktop "detail").
    pub detail: String,
    pub output: String,
    pub status: String,
}

/// A rich transcript item — the unit the new clients render
/// (docs/TRANSCRIPT_MODEL.md). Self-describing and round-trips through the
/// cache, so an MCP app or chart restores with full fidelity.
#[derive(uniffi::Record, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Item {
    /// Stable key: the server `message_id` for text, the `tool_call_id` for
    /// tool-ish rows, or a core-assigned `@n` for a text row the server sent
    /// without an id.
    pub id: String,
    pub kind: ItemKind,
    /// Body text; the tool/app/chart TITLE for tool-ish rows.
    pub text: String,
    /// Tool input, chart spec, or MCP-app input.
    pub detail: String,
    /// Tool result.
    pub output: String,
    /// Tool lifecycle (`in_progress` / `completed` / `failed`).
    pub status: String,
    /// `ItemKind::McpApp` only: `<extension>|<uri>`, the resource-read key.
    pub app_key: String,
    /// `ItemKind::ToolGroup` only: the collapsed calls.
    pub calls: Vec<ToolCall>,
}

/// One transcript mutation carried by `CoreListener::on_item`
/// (docs/TRANSCRIPT_MODEL.md). The single stream: `Upsert` is authoritative and
/// idempotent, the `Append*` ops are O(chunk) streaming shortcuts.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TranscriptOp {
    /// The window is being replaced wholesale (session switch, cache paint, or
    /// a replay that could not merge). Clients clear their item store.
    Reset { session_id: String },
    /// Insert or replace by id (system of record).
    Upsert { item: Item },
    /// Live text delta appended to an existing item.
    AppendText { id: String, chunk: String },
    /// Live tool-output delta appended to an existing item.
    AppendOutput { id: String, chunk: String },
    /// Drop an item by id.
    Remove { id: String },
    /// The pagination cursor: the oldest item the client holds, and whether the
    /// core can still produce older items (`load_older`).
    Window { oldest_id: String, has_older: bool },
}

/// The core's window over the active session's transcript
/// (docs/TRANSCRIPT_MODEL.md). `oldest_id` is empty when the window is empty.
#[derive(uniffi::Record, Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TranscriptWindow {
    pub oldest_id: String,
    pub newest_id: String,
    pub has_older: bool,
}

/// Collapses the desktop's toolgroup/chart/mcpapp split (CONTRACT §3.4).
#[derive(uniffi::Enum, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum ToolCallKind {
    Plain,
    Chart { spec: String },
    McpApp { app_key: String, uri: String, extension: String, input: String },
}

/// A config entry (`provider` | `model` | `mode` | `thinking_effort`).
#[derive(uniffi::Record, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigOption {
    pub id: String,
    pub value: String,
    /// Human-readable label (the SDK's SessionConfigOption.name).
    pub name: String,
    /// Selectable choices for dropdowns, from the reply's `choices` array
    /// ({value, name}); empty when the server doesn't send them (the
    /// config_option_update notification path has no choices in the schema).
    pub choices: Vec<ConfigChoice>,
}

/// One selectable config choice ({value, name} from the raw reply).
#[derive(uniffi::Record, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigChoice {
    pub value: String,
    pub name: String,
}

/// One permission option (CONTRACT §5 / inventory §3).
#[derive(uniffi::Record, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

/// A server permission request surfaced to the UI (CONTRACT §3.2/§5).
#[derive(uniffi::Record, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PermissionRequest {
    pub tool_call_id: String,
    pub title: String,
    pub detail: String,
    pub options: Vec<PermissionOption>,
}

/// The UI's answer to a permission request (CONTRACT §5).
#[derive(uniffi::Enum, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum PermissionOutcome {
    Selected { option_id: String },
    Cancelled,
}

/// A project/skill summary from `sources/list` (CONTRACT §5).
#[derive(uniffi::Record, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ProjectSummary {
    pub path: String,
    pub name: String,
    pub description: Option<String>,
}

/// A prompt to send (CONTRACT §3.1). One or more content blocks.
#[derive(uniffi::Record, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Prompt {
    pub blocks: Vec<PromptBlock>,
}

/// A prompt content block: text / image / resource (CONTRACT §3.4).
#[derive(uniffi::Enum, Clone, Debug, serde::Serialize, serde::Deserialize)]
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
#[derive(uniffi::Record, Clone, Debug, serde::Serialize, serde::Deserialize)]
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
    /// The item stream (docs/TRANSCRIPT_MODEL.md): every transcript row.
    fn on_item(&self, op: TranscriptOp);
    /// Context-window usage + cost (`used`/`size` in tokens).
    fn on_usage(&self, used: i64, size: i64, cost: f64, currency: String);
    /// A turn finished (`stop_reason` is the server's).
    fn on_run_ended(&self, stop_reason: String);
    fn on_config(&self, options: Vec<ConfigOption>);
    fn on_permission_request(&self, request: PermissionRequest);
    fn on_session_touched(&self, session_id: String, title: String, updated_at: String);
    fn on_projects(&self, projects: Vec<ProjectSummary>);
    /// A roam peer's lifecycle line (CONTRACT §6).
    fn on_roam_peer_status(&self, label: String, status: String);
    /// A roam peer's `session/list` result, ids prefixed `roam:<peer>:<id>`.
    fn on_roam_sessions(&self, label: String, sessions: Vec<SessionSummary>);
    /// A fresh session created on a roam peer (`session/new` reply), raw id
    /// (NOT prefixed — the UI prefixes it for routing).
    fn on_peer_new_session(&self, label: String, session_id: String);
    /// The live turn's run id for a session, or empty when the run ended
    /// (gap 1: makes session/steer reachable).
    fn on_active_run(&self, session_id: String, run_id: String);
    /// Slash commands the server can execute right now (gap 2: autocomplete).
    fn on_commands(&self, commands: Vec<String>);
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
    /// The server's provider inventory (`_goose/unstable/providers/list`): the
    /// catalog, which entries are configured, and each one's models. Raw JSON
    /// array of ProviderInventoryEntryDto.
    fn on_providers(&self, providers: String);
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
    SendPrompt(Prompt, Option<SendExpect>, String),
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
    /// Per-session pending intents (key is sessionId, "" for global/no-session).
    /// Prompts queued in one chat must not leak into another.
    pending: HashMap<String, VecDeque<PendingIntent>>,
    next_pending_id: u64,
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
    /// Which session the transcript store's current content BELONGS to.
    ///
    /// Not always the active session: a cold start paints the last chat's cache
    /// and only then opens a connection, which creates a throwaway session
    /// first — so between those two the store holds one session's rows while
    /// another is active. Persisting then would file the wrong transcript under
    /// the wrong id, so [`Core::save_cache`] requires the two to agree.
    store_session_id: Option<String>,
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
    cache: Arc<CacheStore>,
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
    /// Construct the core. The UI supplies its `CacheDir` (CONTRACT §7.4):
    /// every transcript/directory/tools cache file and the roam identity live
    /// under it. An empty string falls back to the platform data dir — on
    /// Android that resolves relative to the read-only process CWD, so the
    /// UI must pass a real absolute dir (context.filesDir).
    #[uniffi::constructor]
    pub fn new(listener: Box<dyn CoreListener>, cache_dir: String) -> Arc<Self> {
        let listener: Arc<dyn CoreListener> = Arc::from(listener);
        // The store shares one listener with the rest of the core; a tiny
        // forwarder adapts the Box the store's seam asks for AND carries the
        // display-ownership gate (chat painting is suppressed while a roam
        // peer owns the screen — see CoreListenerForwarder).
        let active_peer_label = Arc::new(RwLock::new(None));
        let store = Arc::new(TranscriptStore::new(Box::new(CoreListenerForwarder {
            inner: listener.clone(),
            active_peer: active_peer_label.clone(),
        })));
        let cache_dir = if cache_dir.is_empty() {
            default_cache_dir()
        } else {
            PathBuf::from(cache_dir)
        };
        let core = Arc::new(Self {
            inner: Arc::new(CoreInner {
                listener,
                store,
                cache: Arc::new(CacheStore::new(cache_dir)),
                state: Mutex::new(CoreState::default()),
                conn: Mutex::new(None),
                conn_task: Mutex::new(None),
                peers: Mutex::new(Vec::new()),
                active_peer_label, // THE SAME Arc the store's forwarder holds — a second one would make the paint gate blind
            }),
        });
        // Seed the session directory from cache: the drawer renders the
        // names immediately (before the first session/list round trip), the
        // updatedAt table makes a cold-start resume's freshness check match
        // the transcript cache stamp, and the cwds resolve without a probe.
        if let Some((sessions, cwds)) = core.inner.cache.load_directory() {
            {
                let mut state = core.inner.state.lock();
                for (sid, cwd) in &cwds {
                    state.session_cwds.insert(sid.clone(), cwd.clone());
                }
                for s in &sessions {
                    if !s.updated_at.is_empty() {
                        state
                            .session_updated_at
                            .insert(s.id.clone(), s.updated_at.clone());
                    }
                }
            }
            core.inner.listener.on_sessions(sessions);
        }
        // Peer routing for the unstable shim (CONTRACT §6): resolve the peer
        // owning a `roam:<label>:<id>` session so session-bound RPCs reach it.
        let weak = Arc::downgrade(&core);
        crate::spine::register_peer_resolver(Arc::new(move |session_id: &str| {
            let rest = session_id.strip_prefix("roam:")?;
            let label = rest.split(':').next()?;
            let core = weak.upgrade()?;
            let peers = core.inner.peers.lock();
            peers
                .iter()
                .find(|peer| peer.label() == label)
                .cloned()
                .map(|peer: Arc<crate::roam::RoamPeer>| peer as Arc<dyn crate::spine::RpcConn>)
        }));
        core
    }

    // -- intents (CONTRACT §3.1): fire-and-forget into the core's runtime. --

    /// Open the WebSocket, `initialize`, then new-or-resume per §4. Blocks
    /// (bounded) until the connection is ready or fails — the one blocking
    /// intent (INTERNAL.md threading model).
    pub fn connect(&self, config: ServerConfig) {
        let (_, ready_rx) = self.connect_impl(
            config.clone(),
            ConnectSpec::New {
                recipe_id: config.initial_recipe_id.clone(),
            },
            false,
            false,
        );
        self.wait_ready(ready_rx);
    }

    /// Connect and RESUME a specific session on the first handshake — the cold
    /// start when the app already knows which chat to show. Where `connect()`
    /// binds a throwaway `session/new` (which the deferred open then abandoned,
    /// littering the server's list with an empty "New Chat" on every reopen),
    /// this mints NO session: the target is bound directly via
    /// `ConnectSpec::Resume`. It replays `open_session`'s pre-connect preamble
    /// (paint the cached transcript now, decide suppression from freshness,
    /// pre-bind) so the cold chat still paints instantly and a fresh cache
    /// skips the wire replay — identical fast path, minus the orphan.
    pub fn connect_resume(&self, config: ServerConfig, session_id: String) {
        *self.inner.active_peer_label.write() = None;
        self.reset_chat_state();
        let cwd = self.resolve_cwd(&session_id);
        let (suppress, cached) = match self.inner.cache.load_transcript(&session_id) {
            Some((messages, cached_at)) => {
                let fresh = self.transcript_is_fresh(&session_id, &cached_at);
                (fresh, Some(messages))
            }
            None => (false, None),
        };
        // The cache is now the rich item list (`docs/TRANSCRIPT_MODEL.md`), so a
        // paint keeps charts/apps/toolgroups intact. A fresh cache suppresses the
        // replay outright; a STALE one paints and asks the server for a bounded
        // tail that merges into the paint, so opening a long chat no longer
        // re-streams the whole history.
        let merge = cached.is_some() && !suppress;
        self.inner.store.set_session(&session_id);
        match cached {
            Some(items) if suppress => self.inner.store.replace_rich(items, false),
            Some(items) => self.inner.store.replace_rich_for_merge(items),
            None => self.inner.store.clear(),
        };
        {
            let mut state = self.inner.state.lock();
            state.store_session_id = Some(session_id.clone());
        }
        let (_, ready_rx) = self.connect_impl(
            config,
            ConnectSpec::Resume { session_id, cwd },
            suppress,
            merge,
        );
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
        self.inner.state.lock().store_session_id = None;
    }

    /// `session/new` with `_meta.client` + cwd; replaces the current wire.
    /// When a Ready wire with the same host/port/key already exists, reuse it
    /// live — no `old.shutdown()` race. Only falls back to a full reconnect
    /// when the wire is down or the server identity changed.
    pub fn new_session(&self, recipe_id: Option<String>) {
        *self.inner.active_peer_label.write() = None;
        self.reset_chat_state();
        self.inner.store.clear();
        let config = {
            let mut state = self.inner.state.lock();
            state.store_session_id = None;
            state.last_config.clone()
        };
        let Some(config) = config else { return };
        // Live reuse: same host/port/key, already Ready — session/new on the
        // existing wire. This is the project-creation path
        // (createProject → newChatInProject) and every "new chat".
        if let Some(conn) = self.inner.conn.lock().clone() {
            if conn.is_ready() && conn.config_matches(&config) {
                let this = self.clone();
                let cfg = config.clone();
                let rid = recipe_id.clone();
                crate::roam::runtime().spawn(async move {
                    match conn.live_new_session_async(rid.clone()).await {
                        Ok(session_id) => {
                            {
                                let mut state = this.inner.state.lock();
                                state.store_session_id = Some(session_id);
                            }
                            this.save_cache();
                            this.probe_stamp_and_save();
                            // Live reuse doesn't go through the handshake's
                            // on_ready → on_conn_status(Ready) path, so the UI
                            // would stay in `connecting`/`Loading 0` forever.
                            // Emit Ready to drive `live=true`/`connecting=false`.
                            this.on_conn_status(ConnectionStatus::Ready);
                        }
                        Err(_) => {
                            let _ =
                                this.connect_impl(cfg, ConnectSpec::New { recipe_id: rid }, false, false);
                        }
                    }
                });
                return;
            }
        }
        let (_, _rx) =
            self.connect_impl(config, ConnectSpec::New { recipe_id }, false, false);
    }

    /// Whether the cached transcript for a session is up to date with the
    /// session's last-known `updatedAt` (from the directory cache seed, a live
    /// session/list, or a session_info_update). A mismatch means the session
    /// changed remotely and a replay is owed; equality means the cache is
    /// fresh and any load can suppress its replay.
    fn transcript_is_fresh(&self, session_id: &str, cached_at: &str) -> bool {
        if cached_at.is_empty() {
            return false;
        }
        let updated = self
            .inner
            .state
            .lock()
            .session_updated_at
            .get(session_id)
            .cloned()
            .unwrap_or_default();
        !updated.is_empty() && updated == cached_at
    }

    /// `session/load` with the session's real cwd (resolved: cache → probe →
    /// session/list, never guessed); a fresh cached transcript renders
    /// instantly, otherwise the load replays it.
    pub fn open_session(&self, session_id: String) {
        *self.inner.active_peer_label.write() = None;
        self.reset_chat_state();
        // Move the chat-event gate BEFORE anything that can block. Everything
        // below this line runs before the connection rebinds the gate itself, and
        // `resolve_cwd` is the expensive part of "everything below" — so without
        // this the previous chat keeps winning the gate for the duration of a
        // round trip and its streaming lands in the chat the user just opened.
        // See spine::RpcConn::prebind_session.
        if let Some(conn) = self.inner.conn.lock().clone() {
            conn.prebind_session(&session_id);
        }
        let cwd = self.resolve_cwd(&session_id);
        let (suppress, cached) = match self.inner.cache.load_transcript(&session_id) {
            Some((messages, cached_at)) => {
                let fresh = self.transcript_is_fresh(&session_id, &cached_at);
                (fresh, Some(messages))
            }
            None => (false, None),
        };
        // ALWAYS paint the cached transcript instantly — never clear it and wait
        // on the wire. A fresh cache suppresses the replay outright and its rows
        // are authoritative; a stale one is painted PROVISIONAL, so the first
        // real row of the replay this load owes drops it wholesale — the replay
        // APPENDS, so painted rows left in place would be followed by replayed
        // ones. The store owns that handoff (`replace_provisional`).
        //
        // A fresh cache suppresses the replay; a stale one paints and merges a
        // bounded tail (see `connect_resume`). `end_merge` emits one Clear so the
        // rebuild drops any stale suffix the anchor truncated.
        let merge = cached.is_some() && !suppress;
        self.inner.store.set_session(&session_id);
        match cached {
            Some(items) if suppress => self.inner.store.replace_rich(items, false),
            Some(items) => self.inner.store.replace_rich_for_merge(items),
            None => self.inner.store.clear(),
        };
        let config = {
            let mut state = self.inner.state.lock();
            state.store_session_id = Some(session_id.clone());
            state.last_config.clone()
        };
        let Some(config) = config else { return };
        // Live reuse: same host/port/key, already Ready — session/load on the
        // existing wire. Avoids the `old.shutdown()` churn that made every
        // session switch a full reconnect.
        if let Some(conn) = self.inner.conn.lock().clone() {
            if conn.is_ready() && conn.config_matches(&config) {
                let this = self.clone();
                let sid = session_id.clone();
                let c = cwd.clone();
                let cfg = config.clone();
                crate::roam::runtime().spawn(async move {
                    if conn.live_load_session_async(sid.clone(), c.clone(), suppress, merge).await.is_ok() {
                        // Live reuse doesn't go through the handshake's
                        // on_ready → on_conn_status(Ready) path, so without this
                        // the UI stayed in `Loading…` (replayActive never
                        // finalized) and queued prompts never flushed. This
                        // Ready used to arrive indirectly from a spurious
                        // resync replay of the session we just loaded.
                        this.on_conn_status(ConnectionStatus::Ready);
                        return;
                    }
                    // Stale session — try live new session before full reconnect.
                    let conn2_opt = {
                        let g = this.inner.conn.lock();
                        g.clone()
                    };
                    if let Some(conn2) = conn2_opt {
                        if conn2.is_ready() && conn2.config_matches(&cfg) {
                            if let Ok(new_id) = conn2.live_new_session_async(None).await {
                                {
                                    let mut state = this.inner.state.lock();
                                    state.store_session_id = Some(new_id);
                                }
                                this.on_conn_status(ConnectionStatus::Ready);
                                return;
                            }
                        }
                    }
                    let _ = this.connect_impl(cfg, ConnectSpec::Resume { session_id: sid, cwd: c }, suppress, merge);
                });
                return;
            }
        }
        let (_, _rx) =
            self.connect_impl(config, ConnectSpec::Resume { session_id, cwd }, suppress, merge);
    }

    /// Render the cached transcript for a session WITHOUT connecting (cold
    /// start: the UI shows the conversation instantly while the connection
    /// establishes, instead of "Connecting…" over an empty transcript). Emits
    /// the same `Clear` the open path would; the later open is a no-op when
    /// the cache is fresh.
    pub fn load_cached_transcript(&self, session_id: String) {
        if let Some((items, _)) = self.inner.cache.load_transcript(&session_id) {
            // Authoritative, NOT provisional: this is the cold-start
            // placeholder for the chat the user was last in, painted before any
            // connect. `connect()` binds a throwaway session first and the app
            // opens the real one afterwards, so nothing here owes a replay. The
            // path that DOES owe one (`open_session`, and the reconnect's stale
            // resume) repaints with the provisional mode it decides on, and
            // `replace` adopts that mode when the content is identical.
            self.inner.store.set_session(&session_id);
            self.inner.store.replace_rich(items, false);
            // These rows are this session's, whatever session the connection
            // that follows happens to bind first (see `store_session_id`).
            self.inner.state.lock().store_session_id = Some(session_id);
        }
    }


    /// Persist every open transcript NOW: the main session's and each roam
    /// peer's.
    ///
    /// The UI calls this when the app leaves the foreground. Nothing else
    /// guarantees a write before the process dies — the main session saves on
    /// ready and at the end of a turn, and a peer saves when its session is
    /// closed or switched away from, so a chat that was simply left open when
    /// Android reclaimed the process was never written.
    pub fn flush_caches(&self) {
        self.save_cache();
        let peers = self.inner.peers.lock().clone();
        for peer in peers {
            peer.save_open_transcript();
        }
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
                let mut state = self.inner.state.lock();
                let id = state.next_pending_id.to_string();
                state.next_pending_id += 1;
                let key = expect.as_ref().map(|e| e.session_id.clone()).unwrap_or_default();
                state.pending.entry(key).or_default().push_back(PendingIntent::SendPrompt(prompt, expect, id));
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
                self.inner.state.lock().pending.entry(String::new()).or_default().push_back(PendingIntent::SetConfig(config_id, value));
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
        let peer = RoamPeer::connect(
            secret,
            card,
            label,
            self.inner.listener.clone(),
            is_active,
            self.config_hook(),
            self.inner.cache.clone(),
        );
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

    /// Create a fresh session on a roam peer; the peer becomes the chat owner
    /// until a Main session is opened. Uses the only known-good cwd, exactly
    /// like `roam_open_session` (the remote goose has no default working dir).
    pub fn roam_new_session(&self, label: String) {
        let peer = self
            .inner
            .peers
            .lock()
            .iter()
            .find(|peer| peer.label() == label)
            .cloned();
        let Some(peer) = peer else { return };
        *self.inner.active_peer_label.write() = Some(label);
        let cwd = self
            .inner
            .state
            .lock()
            .last_config
            .as_ref()
            .map(|config| config.cwd.clone())
            .unwrap_or_default();
        peer.new_session(cwd);
    }

    /// Create a fresh session on a roam peer in a caller-chosen working dir.
    /// goose natively honors the cwd on `session/new` (a `serve --roam` host
    /// otherwise defaults to $HOME); the UI long-presses the new-chat button
    /// on a roam endpoint to supply it. Blank/whitespace falls back to the
    /// same config cwd `roam_new_session` uses.
    pub fn roam_new_session_in(&self, label: String, cwd: String) {
        let peer = self
            .inner
            .peers
            .lock()
            .iter()
            .find(|peer| peer.label() == label)
            .cloned();
        let Some(peer) = peer else { return };
        *self.inner.active_peer_label.write() = Some(label);
        let trimmed = cwd.trim();
        let cwd = if trimmed.is_empty() {
            self.inner
                .state
                .lock()
                .last_config
                .as_ref()
                .map(|config| config.cwd.clone())
                .unwrap_or_default()
        } else {
            trimmed.to_string()
        };
        peer.new_session(cwd);
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

    /// The rich item snapshot of the active session (docs/TRANSCRIPT_MODEL.md).
    /// Both the main connection and roam peers hold rich items now.
    pub fn rich_transcript(&self) -> Vec<Item> {
        if let Some(peer) = self.active_peer() {
            return peer.items();
        }
        self.inner.store.rich_transcript()
    }

    /// One rich item by id from the main store.
    pub fn item(&self, id: String) -> Option<Item> {
        self.inner.store.item(&id)
    }

    /// The pagination cursor for the active session.
    pub fn window(&self) -> TranscriptWindow {
        self.inner.store.window()
    }

    /// Extend the client's window backward by `count` items
    /// (docs/TRANSCRIPT_MODEL.md). The store keeps the whole transcript, so the
    /// items come from memory — no cache read and no wire call. Emits `Upsert`
    /// per older item (oldest-first) + a refreshed `Window`; `has_older: false`
    /// means there is nothing further back.
    pub fn load_older(&self, count: u32) {
        // A peer owns its own rich window.
        if let Some(peer) = self.active_peer() {
            peer.load_older(count as usize);
            return;
        }
        self.inner.store.load_older(count as usize);
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
        // Per-session pending stays queued for its session; don't clear the
        // whole map when switching chats (a prompt queued in Chat A while
        // busy must not vanish when you open Chat B).
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
        merge_replay: bool,
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
        conn.set_merge_replay(merge_replay);
        conn.set_on_status(self.status_hook());
        conn.set_on_touched(self.touched_hook());
        conn.set_on_active_run(self.active_run_hook());
        conn.set_on_commands(self.commands_hook());
        conn.set_on_config(self.config_hook());
        conn.set_on_ended(self.ended_hook(gen));
        crate::spine::set_current_conn(Some(conn.clone()));

        let transport = WsTransport::new(
            &config.host,
            config.port,
            &config.secret_key,
            config.use_tls,
            config.accept_invalid_certs,
            config.ca_cert_pem.clone(),
        );
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
        rt.block_on(async {
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
        if matches!(status, ConnectionStatus::Ready) {
            // A session/load replay just finished — fresh-cache opens, resume
            // after reconnect, and in-place resync all land here with the full
            // replayed transcript in the store. Persist it so the NEXT open
            // renders from cache (freshness matches) instead of replaying the
            // same delta again. on_prompt_done alone only covered the
            // prompt-turn case, which is why reopening a replayed session
            // re-streamed it every time.
            // An unowned store belongs to whatever session just bound: a fresh
            // session/new starts empty and its rows accumulate from here.
            {
                let active = self.active_session_id();
                let mut state = self.inner.state.lock();
                if state.store_session_id.is_none() {
                    state.store_session_id = active;
                }
            }
            self.save_cache();
            self.flush_pending();
            // doesn't), so the stamp above can race the session/list reply and
            // land empty — which made every later open look stale. Re-stamp
            // from a session/info probe: deterministic, and its updatedAt is
            // byte-identical to the session/list entry, so the freshness check
            // survives process restarts (list on the next cold start == this
            // stamp when nothing changed).
            self.probe_stamp_and_save();
        }
        self.inner.listener.on_status(status);
    }

    /// `session/info` probe for the active session → re-stamp the cache's
    /// updatedAt and persist (see [`Self::on_conn_status`]) and, when the probe
    /// carries a message count, refresh the resync baseline (`sync_stamp`) so
    /// our own just-completed change does not read as a remote one.
    fn probe_stamp_and_save(&self) {
        let Some(session_id) = self.active_session_id() else { return };
        let Some(conn) = self.inner.conn.lock().clone() else { return };
        let core = self.clone();
        crate::roam::runtime().spawn(async move {
            let probed = conn
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
            if let Some((updated_at, message_count)) = probed {
                let mut state = core.inner.state.lock();
                if !updated_at.is_empty() {
                    state
                        .session_updated_at
                        .insert(session_id.clone(), updated_at.clone());
                }
                // Baseline for the move test in `on_session_probe`. Without
                // this, the first `session_info_update` after a prompt (the
                // server's own run-end touch) probes "moved" and replays the
                // session we just finished streaming.
                if !updated_at.is_empty() && message_count >= 0 {
                    state.sync_stamp = Some((updated_at, message_count));
                }
            }
            core.save_cache();
        });
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
        let delay_ms = crate::spine::reconnect_delay_ms(state.reconnect_attempts);
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
        // The freshness gate open_session applies: a session whose cached
        // transcript is up to date must not replay just because the socket
        // dropped (screen-off / background reconnects re-streamed the whole
        // convo every time). The store kept the live transcript across the
        // drop, so a fresh suppress keeps the UI exactly as it was; a stale
        // one replays (the id-gated chunks dedupe against the store).
        let suppress = match self.inner.cache.load_transcript(&resume) {
            Some((_, cached_at)) => self.transcript_is_fresh(&resume, &cached_at),
            None => false,
        };
        // A stale resume replays the whole history onto the live transcript the
        // store kept across the drop. The replay APPENDS — chunks reset the
        // stream anchor, they do NOT dedupe against existing rows — so the rows
        // held now have to be dropped when the replay's first real row lands,
        // or replayed history would land after them. Arm that in the store.
        if !suppress {
            self.inner.store.mark_provisional();
        }
        let (_, _rx) = self.connect_impl(
            config,
            ConnectSpec::Resume { session_id: resume, cwd },
            suppress,
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
                //
                // Neither id is printed: a session id is a bearer-like
                // identifier, and a log is the wrong place for one even when
                // the mismatch is the thing being diagnosed (the caller knows
                // both ids — it passed one in).
                eprintln!(
                    "grouse-core: send_prompt rejected — UI expects a different session than the socket is bound to"
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
        // Re-baseline the resync stamp from the server after OUR OWN turn. The
        // server also emits a run-end `session_info_update`, and its debounced
        // probe otherwise sees a moved (updatedAt, messageCount) against the
        // stamp captured at open time and replays the whole session in place —
        // a full-history reload of the turn we just streamed (painful on a
        // long chat). Recording the post-turn stamp here makes that probe a
        // no-op while a genuine remote change still differs and resyncs.
        self.probe_stamp_and_save();
        self.flush_pending();
    }

    /// Drain the pending queue in order once the socket is ready (CONTRACT §4).
    fn flush_pending(&self) {
        loop {
            let (_key, intent) = {
                let mut state = self.inner.state.lock();
                if state.prompting {
                    return;
                }
                // Global SetConfig queue first (key ""), then current session's SendPrompt queue.
                let sid = self.active_session_id().unwrap_or_default();
                if let Some(q) = state.pending.get_mut("") {
                    if let Some(intent) = q.pop_front() {
                        if q.is_empty() { state.pending.remove(""); }
                        (String::new(), intent)
                    } else {
                        // No global pending, try current session
                        if let Some(q) = state.pending.get_mut(&sid) {
                            if let Some(intent) = q.pop_front() {
                                if q.is_empty() { state.pending.remove(&sid); }
                                (sid.clone(), intent)
                            } else { return; }
                        } else { return; }
                    }
                } else if let Some(q) = state.pending.get_mut(&sid) {
                    if let Some(intent) = q.pop_front() {
                        if q.is_empty() { state.pending.remove(&sid); }
                        (sid.clone(), intent)
                    } else { return; }
                } else {
                    return;
                }
            };
            let requeue = match intent {
                PendingIntent::SendPrompt(prompt, expect, id) => {
                    match self.try_send_prompt(prompt, expect) {
                        Ok(()) => None,
                        Err((prompt, expect)) => {
                            let k = expect.as_ref().map(|e| e.session_id.clone()).unwrap_or_default();
                            Some((k, PendingIntent::SendPrompt(prompt, expect, id)))
                        }
                    }
                }
                PendingIntent::SetConfig(config_id, value) => {
                    match self.try_set_config(config_id, value) {
                        Ok(()) => None,
                        Err((config_id, value)) => Some((String::new(), PendingIntent::SetConfig(config_id, value))),
                    }
                }
            };
            if let Some((k, intent)) = requeue {
                self.inner.state.lock().pending.entry(k).or_default().push_front(intent);
                return;
            }
        }
    }

    /// Persist the accumulated transcript under the current session's
    /// updatedAt (the freshness check on the next open).
    fn save_cache(&self) {
        let Some(session_id) = self.active_session_id() else { return };
        // Only persist rows this session actually owns. A cold start paints the
        // last chat's cache and THEN connects, and the connect creates a
        // throwaway session before the real resume — without this the painted
        // rows were filed under the throwaway's id, one junk cache file per
        // launch, and opening that empty chat replayed another conversation
        // into it.
        if self.inner.state.lock().store_session_id.as_deref() != Some(session_id.as_str()) {
            return;
        }
        let items = self.inner.store.rich_transcript();
        let updated_at = self
            .inner
            .state
            .lock()
            .session_updated_at
            .get(&session_id)
            .cloned()
            .unwrap_or_default();
        self.inner.cache.save_transcript(&session_id, &items, &updated_at);
    }

    fn on_sessions_reply(&self, reply: Value) {
        let (sessions, cwds, updated) = crate::spine::parse_sessions(&reply);
        {
            let mut state = self.inner.state.lock();
            state.sessions = sessions.clone();
            for (sid, cwd) in cwds.clone() {
                state.session_cwds.insert(sid, cwd);
            }
            for (sid, updated_at) in updated {
                state.session_updated_at.insert(sid, updated_at);
            }
        }
        // The drawer's names + cwds persist so a cold start renders them
        // before the first session/list round trip.
        let cwds_map: std::collections::BTreeMap<String, String> =
            cwds.iter().cloned().collect();
        self.inner.cache.save_directory(&sessions, &cwds_map);
        self.inner.listener.on_sessions(sessions);
    }

    /// Merge a config reply into the cached option list (session/new|load
    /// replies, set_config_option replies, and `config_option_update`
    /// notifications all funnel here).
    ///
    /// Two shapes matter on the wire: goose can answer `set_config_option`
    /// with a bare `null` (no `configOptions` key at all), and the typed
    /// `config_option_update` notification carries no `choices` lists. A
    /// wholesale replace would wipe both, which is why the merge keeps the
    /// prior entry's choices when the incoming option has none.
    fn on_config_reply(&self, options: Vec<ConfigOption>) {
        if options.is_empty() {
            return; // null reply / empty update: keep the last full list
        }
        {
            let mut state = self.inner.state.lock();
            for option in options {
                match state.config.iter_mut().find(|o| o.id == option.id) {
                    Some(existing) => {
                        existing.value = option.value.clone();
                        if !option.name.is_empty() {
                            existing.name = option.name;
                        }
                        if !option.choices.is_empty() {
                            existing.choices = option.choices;
                        }
                    }
                    None => state.config.push(option),
                }
            }
        }
        let snapshot = self.inner.state.lock().config.clone();
        self.inner.listener.on_config(snapshot);
    }

    fn on_active_run(&self, session_id: String, run_id: String) {
        self.inner.listener.on_active_run(session_id, run_id);
    }

    fn on_commands(&self, commands: Vec<String>) {
        self.inner.listener.on_commands(commands);
    }

    fn mutate_session(&self, method: &str, params: Value) {
        let session_id = params.get("sessionId").and_then(Value::as_str).unwrap_or("");
        // Route session-bound mutations to the owning roam peer
        // (`roam:<label>:<id>` sessions live on the peer's connection, not the
        // main one). Before this, roam mutations went to the main connection,
        // which doesn't own the session — the remote never saw them, so the
        // change reverted on the next session/list (the app only showed it
        // optimistically).
        if session_id.starts_with("roam:") {
            let Some(peer) = self.peer_for_session(session_id) else { return };
            let method = method.to_string();
            crate::roam::runtime().spawn_blocking(move || {
                if peer.rpc(&method, params).is_ok() {
                    // Re-list the peer's sessions so its drawer reflects the
                    // mutation (the peer owns its own list).
                    peer.relist();
                }
            });
            return;
        }
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

    /// Resolve the roam peer owning a `roam:<label>:<id>` session, if any.
    fn peer_for_session(&self, session_id: &str) -> Option<Arc<crate::roam::RoamPeer>> {
        let rest = session_id.strip_prefix("roam:")?;
        let label = rest.split(':').next()?;
        self.inner
            .peers
            .lock()
            .iter()
            .find(|peer| peer.label() == label)
            .cloned()
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

    fn active_run_hook(&self) -> Arc<dyn Fn(String, String) + Send + Sync> {
        let core = self.clone();
        Arc::new(move |session_id, run_id| core.on_active_run(session_id, run_id))
    }

    fn commands_hook(&self) -> Arc<dyn Fn(Vec<String>) + Send + Sync> {
        let core = self.clone();
        Arc::new(move |commands| core.on_commands(commands))
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
        let dir = self.inner.cache.dir();
        let path = dir.join("roam_identity");
        if let Ok(existing) = std::fs::read_to_string(&path) {
            let existing = existing.trim();
            if !existing.is_empty() {
                // A pre-existing (pre-fix) 0644 identity is still world-readable;
                // tighten it even on the read path.
                #[cfg(unix)]
                crate::cache::make_private(&path);
                return existing.to_string();
            }
        }
        let secret = grouse_roam_core::identity_generate();
        // Atomic write (S-RC-5): a crash mid-write must not leave a truncated
        // identity that would re-roll the secret on next launch.
        let _ = crate::cache::atomic_write(&path, secret.as_bytes());
        // The identity is an iroh secret — never world-readable.
        #[cfg(unix)]
        crate::cache::make_private(&path);
        secret
    }

    /// Override the persisted roam identity so the wire dials with the SAME key
    /// the UI advertises. The platform (desktop QSettings) holds the identity the
    /// user shows a host as a card; without this sync the core would generate and
    /// dial with its own separate secret, which the host never accepted
    /// (not_allowlisted). Blank/unset leaves the core's own identity in place.
    pub fn set_roam_identity(&self, secret: String) {
        let s = secret.trim();
        if s.is_empty() {
            return;
        }
        let dir = self.inner.cache.dir();
        let path = dir.join("roam_identity");
        let _ = crate::cache::atomic_write(&path, s.as_bytes());
        #[cfg(unix)]
        crate::cache::make_private(&path);
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
/// it). The store speaks for the MAIN connection's session, and the app
/// paints every transcript/stream event into its single on-screen transcript
/// — so while a roam peer owns the display (`active_peer` is Some), the
/// main connection's CHAT PAINTING must not emit, or a reply streaming in a
/// backgrounded Main chat would render inside the peer's window. This mirrors
/// the peer's own `is_active` gate at its seam (CONTRACT §6): the peer already
/// suppresses backgrounded emissions, and the main side not doing the same was
/// the asymmetry behind the "wrong conversation streams in" report.
///
/// Turn CONTROL is not painting: `RunEnded` always passes, so a backgrounded
/// turn still drains its own per-chat queue wherever the user is looking (the
/// same reasoning roam.rs applies at its run-end emit — gating that stranded
/// a queue with busy=true). Directory/config/permission/status events are not
/// session-painting either and pass through.
struct CoreListenerForwarder {
    inner: Arc<dyn CoreListener>,
    active_peer: Arc<RwLock<Option<String>>>,
}

impl CoreListenerForwarder {
    /// Is the MAIN connection's session what the UI is showing?
    fn main_displayed(&self) -> bool {
        self.active_peer.read().is_none()
    }
}

impl CoreListener for CoreListenerForwarder {
    fn on_status(&self, status: ConnectionStatus) {
        self.inner.on_status(status);
    }
    fn on_sessions(&self, sessions: Vec<SessionSummary>) {
        self.inner.on_sessions(sessions);
    }
    fn on_item(&self, op: TranscriptOp) {
        if self.main_displayed() {
            self.inner.on_item(op);
        }
    }
    fn on_usage(&self, used: i64, size: i64, cost: f64, currency: String) {
        if self.main_displayed() {
            self.inner.on_usage(used, size, cost, currency);
        }
    }
    fn on_run_ended(&self, stop_reason: String) {
        // Turn CONTROL is not painting: always passes, so a backgrounded turn
        // still drains its own per-chat queue wherever the user is looking.
        self.inner.on_run_ended(stop_reason);
    }
    fn on_config(&self, options: Vec<ConfigOption>) {
        self.inner.on_config(options);
    }
    fn on_permission_request(&self, request: PermissionRequest) {
        self.inner.on_permission_request(request);
    }
    fn on_session_touched(&self, session_id: String, title: String, updated_at: String) {
        self.inner.on_session_touched(session_id, title, updated_at);
    }
    fn on_projects(&self, projects: Vec<ProjectSummary>) {
        self.inner.on_projects(projects);
    }
    fn on_roam_peer_status(&self, label: String, status: String) {
        self.inner.on_roam_peer_status(label, status);
    }
    fn on_roam_sessions(&self, label: String, sessions: Vec<SessionSummary>) {
        self.inner.on_roam_sessions(label, sessions);
    }
    fn on_peer_new_session(&self, label: String, session_id: String) {
        self.inner.on_peer_new_session(label, session_id);
    }
    fn on_active_run(&self, session_id: String, run_id: String) {
        self.inner.on_active_run(session_id, run_id);
    }
    fn on_commands(&self, commands: Vec<String>) {
        self.inner.on_commands(commands);
    }
}

#[cfg(test)]
mod forwarder_tests {
    use super::*;
    use std::sync::Mutex;

    /// Records the painting channels (the gate's subject); the other
    /// CoreListener methods are irrelevant to forwarding and stub out empty.
    struct Recorder {
        items: Mutex<usize>,
        usages: Mutex<usize>,
        run_ended: Mutex<usize>,
    }
    impl Recorder {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                items: Mutex::new(0),
                usages: Mutex::new(0),
                run_ended: Mutex::new(0),
            })
        }
    }
    impl CoreListener for Recorder {
        fn on_status(&self, _: ConnectionStatus) {}
        fn on_sessions(&self, _: Vec<SessionSummary>) {}
        fn on_item(&self, _: TranscriptOp) {
            *self.items.lock().unwrap() += 1;
        }
        fn on_usage(&self, _: i64, _: i64, _: f64, _: String) {
            *self.usages.lock().unwrap() += 1;
        }
        fn on_run_ended(&self, _: String) {
            *self.run_ended.lock().unwrap() += 1;
        }
        fn on_config(&self, _: Vec<ConfigOption>) {}
        fn on_permission_request(&self, _: PermissionRequest) {}
        fn on_session_touched(&self, _: String, _: String, _: String) {}
        fn on_projects(&self, _: Vec<ProjectSummary>) {}
        fn on_roam_peer_status(&self, _: String, _: String) {}
        fn on_roam_sessions(&self, _: String, _: Vec<SessionSummary>) {}
        fn on_peer_new_session(&self, _: String, _: String) {}
        fn on_active_run(&self, _: String, _: String) {}
        fn on_commands(&self, _: Vec<String>) {}
    }

    fn fwd(active: Arc<RwLock<Option<String>>>) -> (CoreListenerForwarder, Arc<Recorder>) {
        let rec = Recorder::new();
        (
            CoreListenerForwarder { inner: rec.clone(), active_peer: active },
            rec,
        )
    }

    #[test]
    fn painting_flows_when_main_owns_the_display() {
        let active = Arc::new(RwLock::new(None));
        let (f, rec) = fwd(active);
        f.on_item(TranscriptOp::Reset { session_id: "s".into() });
        f.on_usage(1, 2, 0.0, "x".into());
        f.on_run_ended("end_turn".into());
        assert_eq!(*rec.items.lock().unwrap(), 1);
        assert_eq!(*rec.usages.lock().unwrap(), 1);
        assert_eq!(*rec.run_ended.lock().unwrap(), 1);
    }

    #[test]
    fn painting_suppressed_while_a_peer_owns_the_display() {
        // The reported bug: a backgrounded Main chat's reply streamed into the
        // roam window. With a peer displayed, item + usage painting must not
        // reach the (single, on-screen) transcript.
        let active = Arc::new(RwLock::new(Some("laptop".to_string())));
        let (f, rec) = fwd(active);
        f.on_item(TranscriptOp::Reset { session_id: "s".into() });
        f.on_usage(1, 2, 0.0, "x".into());
        assert_eq!(*rec.items.lock().unwrap(), 0, "no painting while a peer owns the screen");
        assert_eq!(*rec.usages.lock().unwrap(), 0);
        // Turn CONTROL still passes (below).
    }

    #[test]
    fn run_ended_always_flows_so_a_background_turn_drains_its_queue() {
        // Turn CONTROL is not painting: a Main turn ending while a peer is shown
        // must still emit run-ended, or its per-chat queue strands with busy=true.
        let active = Arc::new(RwLock::new(Some("laptop".to_string())));
        let (f, rec) = fwd(active);
        f.on_run_ended("end_turn".into());
        assert_eq!(*rec.run_ended.lock().unwrap(), 1);
    }
}
