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

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::cache::CacheStore;
use crate::roam::{active_peer, RoamPeer};
use crate::spine::ConnectSpec;
use crate::transcript::TranscriptStore;

uniffi::setup_scaffolding!();

pub mod cache;
pub mod capi;
pub mod notify;
pub mod roam;
mod session;
pub mod speak;
pub mod spine;
mod summarize;
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
    /// Start the fresh session filed under this project (`session/new`
    /// `_meta.projectId`), for a project chat on a cold start (mirrors
    /// `initial_recipe_id`). Stock goose persists it to the `project_id`
    /// column, so no post-creation `session/project/update` is needed.
    #[serde(default)]
    pub initial_project_id: Option<String>,
}

/// Connection lifecycle (CONTRACT §3.3).
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub enum ConnectionStatus {
    #[default]
    Disconnected,
    Connecting,
    Ready,
    Syncing,
    Error {
        message: String,
    },
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
    /// True while the session has backgrounded content the UI has not shown
    /// yet — the green-dot indicator (hub staging, for any transport).
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
    /// The thinking that immediately preceded THIS call, absorbed from the
    /// `Thought` rows above it (issue #13). Per call, not per run, so an expanded
    /// run shows the loop in order: think, call, think, call.
    #[serde(default)]
    pub thought: String,
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
    /// `ItemKind::ToolGroup` / `Tool` only: the model's own framing of the step —
    /// the thinking that preceded the run, absorbed so a 20-call loop reads as a
    /// few meaningful rows instead of a wall of chips. Shown when the chip opens.
    #[serde(default)]
    pub thought: String,
    /// `ItemKind::ToolGroup` / `Tool` only: a one-line label for the step. The
    /// core fills it from the run's thinking; empty means the client falls back
    /// to the call titles.
    #[serde(default)]
    pub summary: String,
    /// `ItemKind::Tool` only: the child tool calls a SUBAGENT made under this
    /// delegate row, oldest first (bounded). Empty for ordinary calls.
    #[serde(default)]
    pub steps: Vec<SubagentStep>,
}

/// One tool call a SUBAGENT made on behalf of a delegate call.
///
/// goose does not expose subagent sessions over ACP (`ACP_VISIBLE_SESSION_TYPES`
/// excludes them), but it forwards each child tool request into the parent's
/// delegate row as `_meta.toolNotification` with a `subagent:<id>` logger. That
/// stream is the record of what the delegate actually did, so the row carries it.
#[derive(uniffi::Record, Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SubagentStep {
    /// The tool the subagent called (`developer__shell`, `read_file`, …).
    pub name: String,
    /// One line of its arguments (command, path, query), already trimmed.
    pub detail: String,
    /// `in_progress` while running; `completed` / `failed` once known.
    pub status: String,
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
#[derive(
    uniffi::Record, Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct TranscriptWindow {
    pub oldest_id: String,
    pub newest_id: String,
    pub has_older: bool,
}

/// How hard a session reload works (grouse#18).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RefreshMode {
    /// Catch up with what is already known: a settled-and-current cache
    /// suppresses the replay, a settled-but-stale one merges a bounded tail, and
    /// an incomplete store (a turn in flight, or a wire that dropped mid-turn)
    /// still takes the full replay — silently, ending in one windowed paint.
    /// This is the everyday "this chat looks stale" refresh.
    Quick,
    /// Ignore freshness and replay the whole session: the escape hatch for a
    /// view known to have drifted.
    Full,
}

/// Collapses the desktop's toolgroup/chart/mcpapp split (CONTRACT §3.4).
#[derive(uniffi::Enum, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ToolCallKind {
    Plain,
    Chart {
        spec: String,
    },
    McpApp {
        app_key: String,
        uri: String,
        extension: String,
        input: String,
    },
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
    Text {
        text: String,
    },
    /// `data` is base64; `mime_type` e.g. `image/png`.
    Image {
        mime_type: String,
        data: String,
    },
    /// `text` and `blob` are mutually exclusive; `blob` is base64.
    Resource {
        uri: String,
        mime_type: String,
        text: Option<String>,
        blob: Option<String>,
    },
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
    fn on_run_ended(&self, session_id: String, stop_reason: String);
    /// A sanitized, sentence-sized chunk of the assistant's visible text, ready
    /// for a client's TTS engine (see `docs/TRANSCRIPT_MODEL.md`). Emitted as
    /// the reply streams (and on the finalizing flush), so a conversation loop
    /// can speak before the turn ends. Clients that are not narrating a live
    /// turn simply ignore it.
    fn on_speak(&self, text: String);
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
    /// An open/resume FAILED and nothing was substituted: the server rejected
    /// `session/load` for `session_id` (a stale/archived id), or the core had
    /// no connection configuration to attempt it with. The core binds NO new
    /// session and repaints nothing — whatever the client is showing stands,
    /// and the client decides: retry the open, or start a fresh chat
    /// explicitly (`new_session`). An answer to an intent the client issued,
    /// so it is never gated by which chat happens to be on screen
    /// (grouse#19: a silent `session/new` here was the "tapped a thread, got a
    /// blank one" defect).
    fn on_session_open_failed(&self, session_id: String, message: String);
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
    /// Dictation provider status (`_goose/unstable/dictation/config`): which
    /// speech-to-text providers the server has configured, as a raw JSON map of
    /// provider name → status entry (the shim's list-reply convention).
    fn on_dictation_config(&self, providers: String);
    /// A dictation transcript (`_goose/unstable/dictation/transcribe`): the text
    /// decoded from the audio clip the UI sent; empty when the clip was silent.
    fn on_transcript(&self, text: String);
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
    /// The store may NOT be a complete copy of the server's transcript: a turn
    /// was in flight (or ended in error) and the last rows are partial. Drives
    /// two things: `save_cache_for` writes an EMPTY `updatedAt` stamp for an
    /// incomplete store (so it can never read back as "fresh" and suppress the
    /// replay that would complete it), and a non-fresh reconnect asks for a FULL
    /// replay instead of a bounded tail-merge, since a merge anchors onto rows
    /// that may be truncated. Keyed by display id: a turn on one chat must not
    /// mark another chat's cache partial (parallel peers make that real).
    incomplete: HashSet<String>,
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
    /// Follow-up probes left in the current resync cycle (desktop m_resyncTicks).
    resync_ticks: i32,
    /// Last probed (updatedAt, messageCount) — the "did it move?" comparison.
    sync_stamp: Option<(String, i64)>,
    /// Ask the model to rewrite each tool run's label at the end of a turn
    /// (issue #13). On by default; costs one short inference per turn, spent on
    /// a hidden session so it never appears in a chat.
    run_summaries: bool,
    /// Sessions whose `session/load` replay is in progress. Replayed thought
    /// chunks are framing for the following tool run, not rows of their own.
    /// Shared by every transport, so the rule is one rule.
    replaying: HashSet<String>,
    /// The live turn's run id per displayed session (main or peer). A run ends
    /// only when one was actually started, so a bare title update cannot clear
    /// the app's busy/steer state.
    active_runs: HashMap<String, String>,
    /// Each session's plain tool runs for the turn in flight, oldest first.
    /// The end-of-turn summarizer drains this; shared by every transport.
    turn_runs: HashMap<String, Vec<String>>,
    /// Hidden run-label summarizer sessions currently in flight (display ids).
    summarizer_ids: HashSet<String>,
    /// Text streamed by those summarizer sessions.
    summarizer_replies: HashMap<String, String>,
    /// One reusable hidden summarizer per endpoint (`""` = main, else the peer
    /// label): `(display id, wire id)`. Truncated after every use.
    summarizer_sessions: HashMap<String, (String, String)>,
}

struct CoreInner {
    listener: Arc<dyn CoreListener>,
    /// Every session's transcript store, keyed by display id, with the one
    /// display gate the UI follows. Both transports feed it.
    hub: Arc<crate::session::SessionHub>,
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
    /// The `Arc` this core is owned by, for handing itself to connections as
    /// the shared dispatch host (set once, right after construction).
    self_ref: std::sync::OnceLock<std::sync::Weak<Core>>,
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
        // One hub for every session, main or roam. The display gate lives in
        // the hub, so a background chat can accumulate without painting.
        let active_peer_label = Arc::new(RwLock::new(None));
        let hub = crate::session::SessionHub::new(listener.clone());
        let cache_dir = if cache_dir.is_empty() {
            default_cache_dir()
        } else {
            PathBuf::from(cache_dir)
        };
        let core = Arc::new(Self {
            inner: Arc::new(CoreInner {
                listener,
                hub,
                cache: Arc::new(CacheStore::new(cache_dir)),
                state: Mutex::new(CoreState {
                    run_summaries: true,
                    ..Default::default()
                }),
                conn: Mutex::new(None),
                conn_task: Mutex::new(None),
                peers: Mutex::new(Vec::new()),
                active_peer_label,
                self_ref: std::sync::OnceLock::new(),
            }),
        });
        let _ = core.inner.self_ref.set(Arc::downgrade(&core));
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
        // The unstable shim resolves the active chat (main or peer) through
        // this, so intents like steer never address the main wire by default.
        let weak = Arc::downgrade(&core);
        crate::spine::register_active_session_resolver(Arc::new(move || {
            weak.upgrade().and_then(|core| core.active_session_id())
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
                project_id: config.initial_project_id.clone(),
                // Cold start: the cached chat already painted stays on screen.
                display: false,
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
        let paint = self.plan_cache_paint(&session_id, false);
        let (suppress, merge) = (paint.suppress, paint.merge);
        // The cache is the rich item list (`docs/TRANSCRIPT_MODEL.md`), so a
        // paint keeps charts/apps/toolgroups intact. A fresh cache suppresses the
        // replay outright; a settled-but-stale one asks for a bounded tail that
        // merges into the paint; an incomplete one paints provisionally and
        // takes the full replay (see `plan_cache_paint`).
        let store = self.inner.hub.display(&session_id);
        match paint.items {
            Some(items) if suppress => store.replace_rich(items, false),
            Some(items) if merge => store.replace_rich_for_merge(items),
            Some(items) => store.replace_rich(items, true),
            None => store.clear(),
        };
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
        if let Some(store) = self.store() {
            store.clear();
        }
        self.inner.hub.clear_display();
    }

    /// `session/new` with `_meta.client` + cwd; replaces the current wire.
    /// `recipe_id` starts a recipe session; `project_id` files the chat under a
    /// project at creation (`_meta.projectId`, which stock goose persists to the
    /// `project_id` column — the create-time bridge off `session/project/update`,
    /// see grouse#5). When a Ready wire with the same host/port/key already
    /// exists, reuse it live — no `old.shutdown()` race. Only falls back to a
    /// full reconnect when the wire is down or the server identity changed.
    pub fn new_session(&self, recipe_id: Option<String>, project_id: Option<String>) {
        *self.inner.active_peer_label.write() = None;
        self.reset_chat_state();
        if let Some(store) = self.store() {
            store.clear();
        }
        self.inner.hub.clear_display();
        let config = self.inner.state.lock().last_config.clone();
        let Some(config) = config else { return };
        // Live reuse: same host/port/key, already Ready — session/new on the
        // existing wire. This is the project-creation path
        // (createProject → newChatInProject) and every "new chat".
        if let Some(conn) = self.inner.conn.lock().clone() {
            if conn.is_ready() && conn.config_matches(&config) {
                let this = self.clone();
                let cfg = config.clone();
                let rid = recipe_id.clone();
                let pid = project_id.clone();
                crate::roam::runtime().spawn(async move {
                    match conn.live_new_session_async(rid.clone(), pid.clone()).await {
                        Ok(_session_id) => {
                            this.save_cache();
                            this.probe_stamp();
                            // Live reuse doesn't go through the handshake's
                            // on_ready → on_conn_status(Ready) path, so the UI
                            // would stay in `connecting`/`Loading 0` forever.
                            // Emit Ready to drive `live=true`/`connecting=false`.
                            this.on_conn_status(ConnectionStatus::Ready);
                        }
                        Err(_) => {
                            let _ = this.connect_impl(
                                cfg,
                                ConnectSpec::New {
                                    recipe_id: rid,
                                    project_id: pid,
                                    display: true,
                                },
                                false,
                                false,
                            );
                        }
                    }
                });
                return;
            }
        }
        let (_, _rx) = self.connect_impl(
            config,
            ConnectSpec::New {
                recipe_id,
                project_id,
                display: true,
            },
            false,
            false,
        );
    }

    /// `session/load` with the session's real cwd (resolved: cache → probe →
    /// session/list, never guessed); a fresh cached transcript renders
    /// instantly, otherwise the load replays it.
    pub fn open_session(&self, session_id: String) {
        self.open_session_inner(session_id, false);
    }

    /// Reload `session_id`.
    ///
    /// [`RefreshMode::Full`] forces a full `session/load` replay, ignoring cache
    /// freshness, and rebuilds the transcript from the server — the escape hatch
    /// for a local view that has drifted (a turn that dropped mid-stream left a
    /// truncated store, and the cache on disk may match it). It never suppresses
    /// and never bounded-tail-merges: the cached window is painted provisionally
    /// and the full replay replaces it.
    ///
    /// [`RefreshMode::Quick`] is the same open without the force: a settled,
    /// current cache suppresses the replay, a settled-but-stale one merges a
    /// bounded tail, and an INCOMPLETE store (a turn in flight, or a wire that
    /// dropped mid-turn — whose cache carries an empty stamp by construction)
    /// still takes the full replay. Either way the replay is ingested silently
    /// and ends in one windowed paint (grouse#18).
    pub fn refresh_session(&self, session_id: String, mode: RefreshMode) {
        let force = matches!(mode, RefreshMode::Full);
        // A peer owns its own wire and cache; the same intent must reach it
        // instead of clearing the display owner and loading on the main wire
        // (which does not own `roam:<label>:<id>`).
        if let Some(peer) = self.peer_for_session(&session_id) {
            *self.inner.active_peer_label.write() = Some(peer.label().to_string());
            self.inner.hub.display(&session_id);
            peer.open_session_with(session_id, String::new(), force);
            return;
        }
        self.open_session_inner(session_id, force);
    }

    fn open_session_inner(&self, session_id: String, force_replay: bool) {
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
        // ALWAYS paint the cached transcript instantly — never clear it and wait
        // on the wire. Three cases:
        //  - a SETTLED, fresh cache is authoritative: suppress the replay;
        //  - a SETTLED, stale cache paints and merges a bounded tail;
        //  - an INCOMPLETE cache (or a forced refresh) paints PROVISIONAL, so the
        //    first real row of the full replay drops it wholesale and rebuilds.
        // The incomplete case is the one that used to corrupt: a mid-stream drop
        // left a truncated store, the bounded-tail merge anchored onto it, and
        // each reopen re-saved a different wrong subset.
        let paint = self.plan_cache_paint(&session_id, force_replay);
        let (suppress, merge) = (paint.suppress, paint.merge);
        let store = self.inner.hub.display(&session_id);
        match paint.items {
            Some(items) if suppress => store.replace_rich(items, false),
            Some(items) if merge => store.replace_rich_for_merge(items),
            Some(items) => store.replace_rich(items, true),
            None => store.clear(),
        };
        let config = self.inner.state.lock().last_config.clone();
        let Some(config) = config else {
            // No connection configuration has ever been installed in this
            // process, so there is nothing to load the session over. The client
            // has ALREADY moved its screen to this session (and cleared its
            // rows) by the time it calls this, so returning silently leaves it
            // parked on a blank chat forever with no error to show (grouse#19).
            self.on_session_open_failed(
                session_id,
                "not connected: no server configuration yet".to_string(),
            );
            return;
        };
        // Live reuse: same host/port/key, already Ready — session/load on the
        // existing wire. Avoids the `old.shutdown()` churn that made every
        // session switch a full reconnect.
        if let Some(conn) = self.inner.conn.lock().clone() {
            if conn.is_ready() && conn.config_matches(&config) {
                let this = self.clone();
                let sid = session_id.clone();
                let c = cwd.clone();
                crate::roam::runtime().spawn(async move {
                    if let Err(error) = conn
                        .live_load_session_async(sid.clone(), c.clone(), suppress, merge)
                        .await
                    {
                        // The live wire refused the load. Do NOT bind a
                        // substitute session: the client asked for THIS one, and
                        // a fresh empty session silently becoming the active
                        // chat is the "tapped a thread, got a blank one" defect
                        // (grouse#19). Report it and leave the wire as it is —
                        // the client retries with `open_session`, or starts a
                        // fresh chat explicitly with `new_session`.
                        this.on_session_open_failed(sid, format!("session/load failed: {error}"));
                        return;
                    }
                    // Live reuse doesn't go through the handshake's
                    // on_ready → on_conn_status(Ready) path, so without this
                    // the UI stayed in `Loading…` (replayActive never
                    // finalized) and queued prompts never flushed. This
                    // Ready used to arrive indirectly from a spurious
                    // resync replay of the session we just loaded.
                    this.on_conn_status(ConnectionStatus::Ready);
                });
                return;
            }
        }
        let (_, _rx) = self.connect_impl(
            config,
            ConnectSpec::Resume { session_id, cwd },
            suppress,
            merge,
        );
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
            let store = self.inner.hub.display(&session_id);
            store.replace_rich(items, false);
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
        let Some(conn) = self.inner.conn.lock().clone() else {
            return;
        };
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

    /// Send a prompt (text/image/resource blocks). Queues until the owning
    /// connection can accept it; rejected (not queued) when `expect` mismatches
    /// the displayed session. One path for the main wire and every peer.
    pub fn send_prompt(&self, prompt: Prompt, expect: Option<SendExpect>) {
        let Some(display) = self.active_session_id() else {
            self.queue_prompt(prompt, expect);
            return;
        };
        if let Some(exp) = &expect {
            if exp.session_id != display {
                eprintln!(
                    "grouse-core: send_prompt rejected — UI expects a different session than the one displayed"
                );
                return;
            }
        }
        let Some(owner) = self.owner_for(&display) else {
            self.queue_prompt(prompt, expect);
            return;
        };
        if self.inner.state.lock().prompting || !owner.accepts_prompt() {
            self.queue_prompt(prompt, expect);
            return;
        }
        self.spawn_prompt(owner, display, prompt);
    }

    /// `session/cancel` (a notification; never waits for a reply).
    pub fn cancel(&self) {
        let Some(display) = self.active_session_id() else {
            return;
        };
        let Some(owner) = self.owner_for(&display) else {
            return;
        };
        let params = json!({ "sessionId": owner.wire_id(&display) });
        crate::roam::runtime().spawn(async move {
            let _ = owner.notify("session/cancel", params);
        });
    }

    /// `session/set_config_option`; queues until ready (CONTRACT §4).
    pub fn set_config_option(&self, config_id: String, value: String) {
        match self.try_set_config(config_id, value) {
            Ok(()) => {}
            Err((config_id, value)) => {
                self.inner
                    .state
                    .lock()
                    .pending
                    .entry(String::new())
                    .or_default()
                    .push_back(PendingIntent::SetConfig(config_id, value));
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
        if self.inner.hub.is_displayed(&session_id) {
            if let Some(store) = self.store() {
                store.clear();
            }
        }
        self.inner.hub.forget(&session_id);
        self.inner.state.lock().incomplete.remove(&session_id);
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
        let host: Arc<dyn crate::session::DispatchHost> = Arc::new(
            crate::session::PrefixHost::new(label.clone(), self.host(), self.inner.hub.clone()),
        );
        let peer = RoamPeer::connect(
            secret,
            card,
            label,
            self.inner.listener.clone(),
            host,
            self.inner.hub.clone(),
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
            self.inner.hub.clear_display();
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
        // The store is the single display source; the peer paints into it.
        self.inner.hub.display(&session_id);
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
            let _ = peer.respond_permission(&tool_call_id, outcome);
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
        self.inner
            .conn
            .lock()
            .as_ref()
            .and_then(|conn| conn.active_session_id())
    }

    pub fn sessions(&self) -> Vec<SessionSummary> {
        self.inner.state.lock().sessions.clone()
    }

    /// The accumulated active-session transcript.
    pub fn transcript(&self) -> Vec<Message> {
        self.store()
            .map(|store| store.transcript())
            .unwrap_or_default()
    }

    /// The rich item snapshot of the active session (docs/TRANSCRIPT_MODEL.md).
    pub fn rich_transcript(&self) -> Vec<Item> {
        self.store()
            .map(|store| store.rich_transcript())
            .unwrap_or_default()
    }

    /// One rich item by id from the displayed session.
    pub fn item(&self, id: String) -> Option<Item> {
        self.store().and_then(|store| store.item(&id))
    }

    /// The pagination cursor for the displayed session.
    pub fn window(&self) -> TranscriptWindow {
        self.store()
            .map(|store| store.window())
            .unwrap_or(TranscriptWindow {
                oldest_id: String::new(),
                newest_id: String::new(),
                has_older: false,
            })
    }

    /// Extend the client's window backward by `count` items
    /// (docs/TRANSCRIPT_MODEL.md). The store keeps the whole transcript, so the
    /// items come from memory — no cache read and no wire call. Emits `Upsert`
    /// per older item (oldest-first) + a refreshed `Window`; `has_older: false`
    /// means there is nothing further back.
    pub fn load_older(&self, count: u32) {
        if let Some(store) = self.store() {
            store.load_older(count as usize);
        }
    }

    pub fn config(&self) -> Vec<ConfigOption> {
        self.inner.state.lock().config.clone()
    }

    /// Rewrite each tool run's label with a short model-written line at the end
    /// of a turn (issue #13). On by default; the request runs on a hidden
    /// session after the turn, so it never delays a reply or appears in a chat.
    pub fn set_run_summaries(&self, on: bool) {
        self.inner.state.lock().run_summaries = on;
    }

    /// Whether the model rewrites run labels (issue #13).
    pub fn run_summaries(&self) -> bool {
        self.inner.state.lock().run_summaries
    }
}

// ---------------------------------------------------------------------------
// Core internals: connection lifecycle, status machine, reconnect, resync
// ---------------------------------------------------------------------------

impl Core {
    /// The store of the chat the UI is showing, if any.
    fn store(&self) -> Option<Arc<TranscriptStore>> {
        self.inner.hub.displayed_store()
    }

    /// The store for a session, created on first use.
    fn store_for(&self, key: &str) -> Arc<TranscriptStore> {
        self.inner.hub.store(key)
    }

    /// The shared dispatch host (this core), for a connection.
    fn host(&self) -> Arc<dyn crate::session::DispatchHost> {
        self.inner
            .self_ref
            .get()
            .and_then(|weak| weak.upgrade())
            .expect("core self reference is set at construction")
    }

    /// Decide how to paint a session's cached transcript and which wire load it
    /// owes. `force_replay` (see [`Core::refresh_session`]) ignores the cache as
    /// a source of truth. A cache is usable as a merge BASE only when it is
    /// settled (non-empty stamp: written from a complete store) and the store is
    /// not currently incomplete — a truncated base would anchor a bounded-tail
    /// merge in the wrong place and drift on every reopen.
    fn plan_cache_paint(&self, session_id: &str, force_replay: bool) -> crate::session::PaintPlan {
        let (listed, incomplete) = {
            let state = self.inner.state.lock();
            (
                state
                    .session_updated_at
                    .get(session_id)
                    .cloned()
                    .unwrap_or_default(),
                state.incomplete.contains(session_id),
            )
        };
        crate::session::plan_paint(
            &self.inner.cache,
            session_id,
            &listed,
            force_replay,
            incomplete,
        )
    }

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
    ) -> (
        Arc<crate::spine::Conn>,
        oneshot::Receiver<Result<(), String>>,
    ) {
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

        let host = self.host();
        let (conn, ready_rx) = crate::spine::Conn::new(
            self.inner.listener.clone(),
            self.inner.hub.clone(),
            host,
            config.clone(),
            spec,
        );
        conn.set_suppress_replay(suppress_replay);
        conn.set_merge_replay(merge_replay);
        conn.set_on_status(self.status_hook());
        conn.set_on_open_failed(self.open_failed_hook());
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
            // The session THIS wire bound (the display may belong to a peer).
            let bound = self
                .inner
                .conn
                .lock()
                .as_ref()
                .and_then(|conn| conn.active_session_id());
            if let Some(key) = bound {
                {
                    let mut state = self.inner.state.lock();
                    // The replay/new just completed, so the store is a faithful
                    // copy of the server's transcript — unless a turn is still
                    // in flight.
                    if state.prompting {
                        state.incomplete.insert(key.clone());
                    } else {
                        state.incomplete.remove(&key);
                    }
                }
                self.save_cache_for(&key);
                // Record the server's current stamp from a session/info probe
                // (no cache write: a load's store is only current up to the
                // stamp its replay carried). This makes a later open compare
                // the cache against the server's real position.
                self.refresh_stamp(&key, false);
            }
            self.flush_pending();
        }
        self.inner.listener.on_status(status);
    }

    /// `session/info` probe for the active session: record its updatedAt (the
    /// cache-freshness comparison) and refresh the resync baseline
    /// (`sync_stamp`) so our own just-completed change does not read as a
    /// remote one. Does not write the cache — see [`Self::refresh_stamp`].
    fn probe_stamp(&self) {
        if let Some(session_id) = self.active_session_id() {
            self.refresh_stamp(&session_id, false);
        }
    }

    /// Re-read a session's stamp from its OWNING connection. `persist` writes
    /// the store with the probed stamp — only safe when the store holds the
    /// turn that moved it (after our own prompt). Works for the main wire and
    /// a peer alike.
    fn refresh_stamp(&self, session_id: &str, persist: bool) {
        let Some(owner) = self.owner_for(session_id) else {
            return;
        };
        let params = json!({ "sessionId": owner.wire_id(session_id) });
        let core = self.clone();
        let key = session_id.to_string();
        crate::roam::runtime().spawn(async move {
            let probed =
                crate::session::request(owner.as_ref(), "_goose/unstable/session/info", params)
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
                        .insert(key.clone(), updated_at.clone());
                }
                // Baseline for the move test in `on_session_probe`. Without
                // this, the first `session_info_update` after a prompt (the
                // server's own run-end touch) probes "moved" and replays the
                // session we just finished streaming.
                if !updated_at.is_empty() && message_count >= 0 {
                    state.sync_stamp = Some((updated_at, message_count));
                }
            }
            if persist {
                core.save_cache_for(&key);
            }
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
        // drop, so a fresh suppress keeps the UI exactly as it was; a stale one
        // replays (see below: the live rows are marked provisional and the
        // replay replaces them).
        let paint = self.plan_cache_paint(&resume, false);
        let suppress = paint.suppress;
        // The store holds the LIVE transcript across the drop, not a painted
        // cache, so there is no merge base: `replace_rich_for_merge` was never
        // called and the store's `merging` flag is off. A "merge" here would
        // therefore just APPEND the replay onto the live rows — and goose
        // ignores `_meta.replayTail` and replays the WHOLE session with
        // `tool_call_id`s re-assigned from `call_0`, so appending duplicates
        // every replayed row and collides the ids (the repeated/duplicated-bubble
        // bug). Arm the provisional drop instead: the first replayed row
        // supersedes the live rows and the full replay rebuilds cleanly. Never
        // arm a merge on reconnect — the paint it would need does not exist.
        if !suppress {
            self.store_for(&resume).mark_provisional();
        }
        let (_, _rx) = self.connect_impl(
            config,
            ConnectSpec::Resume {
                session_id: resume,
                cwd,
            },
            suppress,
            false,
        );
    }

    /// Resolve a session's real cwd: last-known (session/list / previous
    /// open) → `_goose/unstable/session/info` probe → the user's working dir.
    /// Never guess (CONTRACT §4 footgun note).
    fn resolve_cwd(&self, session_id: &str) -> String {
        if let Some(cwd) = self
            .inner
            .state
            .lock()
            .session_cwds
            .get(session_id)
            .cloned()
        {
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
        let Some(display) = self.active_session_id() else {
            return Err((prompt, expect));
        };
        let Some(owner) = self.owner_for(&display) else {
            return Err((prompt, expect));
        };
        if !owner.accepts_prompt() {
            return Err((prompt, expect));
        }
        if let Some(exp) = &expect {
            if exp.session_id != display {
                // The UI is showing a different chat than the one displayed;
                // reject instead of mis-routing (CONTRACT §3.1). Neither id is
                // printed: a session id is a bearer-like identifier.
                eprintln!(
                    "grouse-core: send_prompt rejected — UI expects a different session than the one displayed"
                );
                return Ok(());
            }
        }
        self.spawn_prompt(owner, display, prompt);
        Ok(())
    }

    /// Park a prompt until the displayed chat's owner can accept it.
    fn queue_prompt(&self, prompt: Prompt, expect: Option<SendExpect>) {
        let mut state = self.inner.state.lock();
        let id = state.next_pending_id.to_string();
        state.next_pending_id += 1;
        let key = expect
            .as_ref()
            .map(|e| e.session_id.clone())
            .or_else(|| self.active_session_id())
            .unwrap_or_default();
        state
            .pending
            .entry(key)
            .or_default()
            .push_back(PendingIntent::SendPrompt(prompt, expect, id));
    }

    fn try_set_config(&self, config_id: String, value: String) -> Result<(), (String, String)> {
        let Some(display) = self.active_session_id() else {
            return Err((config_id, value));
        };
        let Some(owner) = self.owner_for(&display) else {
            return Err((config_id, value));
        };
        if !owner.accepts_prompt() {
            return Err((config_id, value));
        }
        self.spawn_set_config(owner, display, config_id, value);
        Ok(())
    }

    fn spawn_prompt(
        &self,
        owner: Arc<dyn crate::session::ChatEndpoint>,
        session_id: String,
        prompt: Prompt,
    ) {
        let params = crate::session::prompt_params(&prompt, &owner.wire_id(&session_id));
        let core = self.clone();
        let store = self.store_for(&session_id);
        let key = session_id.clone();
        let summary_key = session_id.clone();
        // Local echo of the prompt. goose does NOT send a `user_message_chunk`
        // for a prompt WE sent: the prompt next crosses the wire in a later
        // `session/load` replay. The echo lands here, at the send, so exactly
        // one row appears per prompt that reaches the wire; attachments stay
        // the client's own local echo (`Item` has no image field).
        if let Some(text) = pure_text(&prompt) {
            store.echo_prompt(&text);
        }
        // A new turn's runs are a fresh worklist for the summarizer.
        self.inner.state.lock().turn_runs.remove(&session_id);
        {
            let mut state = self.inner.state.lock();
            state.prompting = true;
            // The turn's rows are not final until it ends: mark THIS session
            // incomplete so a save or reconnect in the meantime cannot treat it
            // as a settled copy.
            state.incomplete.insert(session_id.clone());
        }
        let summary_owner = owner.clone();
        crate::roam::runtime().spawn(async move {
            let result = owner.request("session/prompt", params).await;
            core.on_turn_ended(&key, result);
            // The turn is over: rewrite its tool runs' labels (issue #13),
            // after the reply and on a hidden session on the SAME connection,
            // so it costs the user no latency.
            core.spawn_run_summaries(summary_owner, summary_key);
        });
    }

    /// Ask the model for a one-line label per tool run of the turn that just
    /// ended. The provisional label (the run's own thinking, condensed by the
    /// store) stands wherever the model does not answer, so this is pure
    /// improvement — and it runs after the turn, so it never delays a reply.
    fn spawn_run_summaries(
        &self,
        owner: Arc<dyn crate::session::ChatEndpoint>,
        key: String,
    ) {
        let (enabled, run_ids) = {
            let mut state = self.inner.state.lock();
            let ids = state.turn_runs.remove(&key).unwrap_or_default();
            (state.run_summaries, ids)
        };
        if !enabled {
            return;
        }
        let store = self.store_for(&key);
        let runs: Vec<(String, String, Vec<String>)> = run_ids
            .into_iter()
            .filter_map(|id| {
                store
                    .run_framing(&id)
                    .map(|(thought, tools)| (id, thought, tools))
            })
            .collect();
        if runs.is_empty() {
            return;
        }
        let core = self.clone();
        crate::roam::runtime().spawn(async move {
            core.rewrite_run_labels(owner, key, runs).await;
        });
    }

    /// One hidden session on the CHAT'S OWN connection (main or peer). Its
    /// `session/update` chunks are intercepted centrally by `session::dispatch`
    /// via the summarizer ids, so they never touch a client store.
    async fn rewrite_run_labels(
        &self,
        owner: Arc<dyn crate::session::ChatEndpoint>,
        key: String,
        runs: Vec<(String, String, Vec<String>)>,
    ) {
        let slot = owner.label().unwrap_or_default();
        let existing = self
            .inner
            .state
            .lock()
            .summarizer_sessions
            .get(&slot)
            .cloned();
        let (hidden, wire) = if let Some(pair) = existing {
            pair
        } else {
            let cwd = owner
                .session_cwd(&key)
                .filter(|cwd| !cwd.is_empty())
                .unwrap_or_else(|| {
                    if owner.label().is_none() {
                        self.resolve_cwd(&key)
                    } else {
                        String::new()
                    }
                });
            // `_meta.hidden` keeps it out of every session list.
            let params = json!({
                "cwd": cwd,
                "mcpServers": [],
                "_meta": { "hidden": true },
            });
            let Ok(reply) =
                crate::session::request(owner.as_ref(), "session/new", params).await
            else {
                return;
            };
            let Some(wire) = reply
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                return;
            };
            let hidden = crate::session::display_id(owner.label().as_deref(), &wire);
            self.inner
                .state
                .lock()
                .summarizer_sessions
                .insert(slot.clone(), (hidden.clone(), wire.clone()));
            (hidden, wire)
        };
        self.inner
            .state
            .lock()
            .summarizer_ids
            .insert(hidden.clone());

        let body = crate::summarize::prompt(
            &runs
                .iter()
                .map(|(_, t, tools)| (t.clone(), tools.clone()))
                .collect::<Vec<_>>(),
        );
        let params = crate::session::prompt_params(
            &Prompt {
                blocks: vec![PromptBlock::Text { text: body }],
            },
            &wire,
        );
        let sent = crate::session::request(owner.as_ref(), "session/prompt", params)
            .await
            .is_ok();
        let text = crate::session::DispatchHost::take_summarizer_reply(self, &hidden);
        // Empty the hidden session: its context must not grow turn on turn.
        let truncated = crate::session::request(
            owner.as_ref(),
            "_goose/unstable/session/conversation/truncate",
            json!({ "sessionId": &wire, "truncateFrom": 0 }),
        )
        .await
        .is_ok();
        {
            let mut state = self.inner.state.lock();
            state.summarizer_ids.remove(&hidden);
            if !sent || !truncated {
                // The hidden session is unusable (the link replaced it, or its
                // context could not be emptied): recreate it next turn.
                state.summarizer_sessions.remove(&slot);
                state.summarizer_replies.remove(&hidden);
            }
        }
        if !sent || !truncated {
            return;
        }
        let store = self.store_for(&key);
        for (n, label) in crate::summarize::parse_labels(&text) {
            if let Some((id, _, _)) = runs.get(n - 1) {
                store.set_run_summary(id, &label);
            }
        }
    }

    fn spawn_set_config(
        &self,
        owner: Arc<dyn crate::session::ChatEndpoint>,
        session_id: String,
        config_id: String,
        value: String,
    ) {
        let core = self.clone();
        let key = session_id.clone();
        crate::roam::runtime().spawn(async move {
            let params = json!({
                "sessionId": owner.wire_id(&session_id),
                "configId": config_id,
                "value": value,
            });
            if let Ok(reply) =
                crate::session::request(owner.as_ref(), "session/set_config_option", params).await
            {
                let options = crate::session::parse_config_options(&reply);
                core.on_config_reply(options);
                core.refresh_stamp(&key, false);
            }
        });
    }

    /// A turn finished on `key`'s connection. One path for every transport:
    /// finalize the transcript, persist with the incomplete rule, re-baseline
    /// the resync stamp, then drain the queue.
    ///
    /// `clean` is false when the prompt failed (including a dropped wire): the
    /// store's tail is then partial, so the cache is written with an empty
    /// stamp and the next open full-replays instead of trusting it.
    fn on_turn_ended(&self, key: &str, result: Result<Value, crate::spine::AcpError>) {
        let (stop, clean) = match &result {
            Ok(reply) => (
                reply
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                true,
            ),
            Err(error) => {
                // No stable error event exists; surface the failure as the
                // turn's end so the UI clears in-flight state and the queue
                // drains (the desktop wedged here before).
                eprintln!("grouse-core: session/prompt failed: {error}");
                ("error".to_string(), false)
            }
        };
        self.store_for(key).run_ended(&stop);
        {
            let mut state = self.inner.state.lock();
            state.prompting = false;
            if clean {
                state.incomplete.remove(key);
            } else {
                state.incomplete.insert(key.to_string());
            }
        }
        self.save_cache_for(key);
        // Re-baseline the resync stamp from the server after OUR OWN turn. The
        // server also emits a run-end `session_info_update`, and its debounced
        // probe otherwise sees a moved (updatedAt, messageCount) against the
        // stamp captured at open time and replays the whole session in place —
        // a full-history reload of the turn we just streamed (painful on a
        // long chat). Recording the post-turn stamp here makes that probe a
        // no-op while a genuine remote change still differs and resyncs.
        self.refresh_stamp(key, true);
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
                        if q.is_empty() {
                            state.pending.remove("");
                        }
                        (String::new(), intent)
                    } else {
                        // No global pending, try current session
                        if let Some(q) = state.pending.get_mut(&sid) {
                            if let Some(intent) = q.pop_front() {
                                if q.is_empty() {
                                    state.pending.remove(&sid);
                                }
                                (sid.clone(), intent)
                            } else {
                                return;
                            }
                        } else {
                            return;
                        }
                    }
                } else if let Some(q) = state.pending.get_mut(&sid) {
                    if let Some(intent) = q.pop_front() {
                        if q.is_empty() {
                            state.pending.remove(&sid);
                        }
                        (sid.clone(), intent)
                    } else {
                        return;
                    }
                } else {
                    return;
                }
            };
            let requeue = match intent {
                PendingIntent::SendPrompt(prompt, expect, id) => {
                    match self.try_send_prompt(prompt, expect) {
                        Ok(()) => None,
                        Err((prompt, expect)) => {
                            let k = expect
                                .as_ref()
                                .map(|e| e.session_id.clone())
                                .unwrap_or_default();
                            Some((k, PendingIntent::SendPrompt(prompt, expect, id)))
                        }
                    }
                }
                PendingIntent::SetConfig(config_id, value) => {
                    match self.try_set_config(config_id, value) {
                        Ok(()) => None,
                        Err((config_id, value)) => {
                            Some((String::new(), PendingIntent::SetConfig(config_id, value)))
                        }
                    }
                }
            };
            if let Some((k, intent)) = requeue {
                self.inner
                    .state
                    .lock()
                    .pending
                    .entry(k)
                    .or_default()
                    .push_front(intent);
                return;
            }
        }
    }

    /// Persist the accumulated transcript under the current session's
    /// updatedAt (the freshness check on the next open).
    ///
    /// An INCOMPLETE store (a turn in flight, or one that ended in error after a
    /// dropped wire) is written with an EMPTY stamp: the content is still worth
    /// painting instantly, but it must never compare equal to the server's
    /// `updatedAt` and suppress the replay that would complete it.
    fn save_cache(&self) {
        if let Some(session_id) = self.active_session_id() {
            self.save_cache_for(&session_id);
        }
    }

    /// Persist one session's store. The store is keyed by display id, so the
    /// rows can never be filed under another chat.
    fn save_cache_for(&self, session_id: &str) {
        let items = self.store_for(session_id).rich_transcript();
        let (incomplete, updated_at) = {
            let state = self.inner.state.lock();
            (
                state.incomplete.contains(session_id),
                state
                    .session_updated_at
                    .get(session_id)
                    .cloned()
                    .unwrap_or_default(),
            )
        };
        let stamp = if incomplete { "" } else { &updated_at };
        self.inner.cache.save_transcript(session_id, &items, stamp);
    }

    fn on_sessions_reply(&self, reply: Value) {
        let (mut sessions, cwds, updated) = crate::spine::parse_sessions(&reply);
        for session in &mut sessions {
            session.has_new = self.inner.hub.has_new(&session.id);
        }
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
        let cwds_map: std::collections::BTreeMap<String, String> = cwds.iter().cloned().collect();
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

    /// An open/resume failed; nothing was substituted (see the listener's
    /// `on_session_open_failed`).
    pub fn on_session_open_failed(&self, session_id: String, message: String) {
        self.inner
            .listener
            .on_session_open_failed(session_id, message);
    }

    fn mutate_session(&self, method: &str, params: Value) {
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or("");
        // Route session-bound mutations to the owning roam peer
        // (`roam:<label>:<id>` sessions live on the peer's connection, not the
        // main one). Before this, roam mutations went to the main connection,
        // which doesn't own the session — the remote never saw them, so the
        // change reverted on the next session/list (the app only showed it
        // optimistically).
        if session_id.starts_with("roam:") {
            let Some(peer) = self.peer_for_session(session_id) else {
                return;
            };
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
        let Some(conn) = self.inner.conn.lock().clone() else {
            return;
        };
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

    /// The connection that owns a display session id: a peer for a
    /// `roam:<label>:<id>` id, the main wire otherwise.
    fn owner_for(&self, display: &str) -> Option<Arc<dyn crate::session::ChatEndpoint>> {
        if display.starts_with(crate::session::ROAM_PREFIX) {
            return self
                .peer_for_session(display)
                .map(|peer| peer as Arc<dyn crate::session::ChatEndpoint>);
        }
        self.inner
            .conn
            .lock()
            .clone()
            .map(|conn| conn as Arc<dyn crate::session::ChatEndpoint>)
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
                state
                    .session_updated_at
                    .insert(session_id.clone(), updated_at.clone());
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
        let Some(owner) = self.owner_for(&session_id) else {
            return;
        };
        let params = json!({ "sessionId": owner.wire_id(&session_id) });
        let core = self.clone();
        crate::roam::runtime().spawn(async move {
            let probe =
                crate::session::request(owner.as_ref(), "_goose/unstable/session/info", params)
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
        let Some(owner) = self.owner_for(session_id) else {
            return;
        };
        let cwd = owner
            .session_cwd(session_id)
            .unwrap_or_else(|| self.resolve_cwd(session_id));
        let key = session_id.to_string();
        self.store_for(session_id).clear();
        // This is a FULL rebuild: the store was just cleared, so the replay must
        // be APPLIED. A previous live load can leave `suppress_replay` armed
        // (open a fresh cache → suppress), and without this reset the resync's
        // rows would be dropped and the transcript left empty/partial.
        owner.reset_replay_gates();
        self.inner
            .state
            .lock()
            .replaying
            .insert(session_id.to_string());
        self.emit_status(ConnectionStatus::Syncing);
        let core = self.clone();
        crate::roam::runtime().spawn(async move {
            let params = crate::session::session_load_params(&owner.wire_id(&key), &cwd, None);
            match crate::session::request(owner.as_ref(), "session/load", params).await {
                Ok(reply) => {
                    core.inner.state.lock().replaying.remove(&key);
                    let options = crate::session::parse_config_options(&reply);
                    core.on_config_reply(options);
                    core.emit_status(ConnectionStatus::Ready);
                    if let Some(peer) = core.peer_for_session(&key) {
                        peer.relist();
                    } else {
                        core.list_sessions();
                    }
                }
                Err(error) => {
                    core.inner.state.lock().replaying.remove(&key);
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

    fn config_hook(&self) -> Arc<dyn Fn(Vec<ConfigOption>) + Send + Sync> {
        let core = self.clone();
        Arc::new(move |options| core.on_config_reply(options))
    }

    fn open_failed_hook(&self) -> Arc<dyn Fn(String, String) + Send + Sync> {
        let core = self.clone();
        Arc::new(move |session_id, message| core.on_session_open_failed(session_id, message))
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

/// The prompt's text when it is PURE text — the only shape the item model can
/// carry (see the local echo in `spawn_prompt`). `None` when it has an image or
/// resource block: those stay the client's own local echo, because `Item` has no
/// image field to represent them.
fn pure_text(prompt: &Prompt) -> Option<String> {
    let mut text = String::new();
    for block in &prompt.blocks {
        match block {
            PromptBlock::Text { text: t } => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(t);
            }
            _ => return None,
        }
    }
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// The core is the dispatch host for the main connection: display ids are the
/// wire ids, and the hub's display gate decides what paints.
impl crate::session::DispatchHost for Core {
    fn display_id(&self, wire: &str) -> String {
        crate::session::display_id(None, wire)
    }
    fn store(&self, key: &str) -> Arc<TranscriptStore> {
        self.inner.hub.store(key)
    }
    fn touched(&self, key: String, title: String, updated_at: String) {
        self.on_session_touched(key, title, updated_at);
    }
    fn active_run(&self, key: String, run_id: String) {
        if run_id.is_empty() {
            if self.inner.state.lock().active_runs.remove(&key).is_some() {
                self.inner.listener.on_active_run(key, String::new());
            }
        } else {
            self.inner
                .state
                .lock()
                .active_runs
                .insert(key.clone(), run_id.clone());
            self.inner.listener.on_active_run(key, run_id);
        }
    }
    fn config(&self, key: &str, options: Vec<ConfigOption>) {
        if self.inner.hub.is_displayed(key) {
            self.on_config_reply(options);
        }
    }
    fn usage(&self, key: &str, used: i64, size: i64, cost: f64, currency: String) {
        if self.inner.hub.is_displayed(key) {
            self.inner.listener.on_usage(used, size, cost, currency);
        }
    }
    fn commands(&self, _key: &str, names: Vec<String>) {
        // Slash commands are a connection-level capability, not chat content:
        // forward them even while a transient session is binding (the old
        // spine behavior).
        self.inner.listener.on_commands(names);
    }
    fn permission(&self, key: &str, request: PermissionRequest) {
        if self.inner.hub.is_displayed(key) {
            self.inner.listener.on_permission_request(request);
        }
    }
    fn begin_replay(&self, key: &str) {
        self.inner.state.lock().replaying.insert(key.to_string());
    }
    fn end_replay(&self, key: &str) {
        self.inner.state.lock().replaying.remove(key);
    }
    fn replaying(&self, key: &str) -> bool {
        self.inner.state.lock().replaying.contains(key)
    }
    /// May a chunk for `key` land in its store right now?
    ///
    /// The main connection suppresses content ONLY while it is replay-ing that
    /// one session in (the store is held and painted once at the end). Asking
    /// the connection-wide flag without comparing the key dropped a PEER's
    /// chunks whenever the main wire happened to be in that window — a chat on
    /// another transport went silent for a main-connection reason.
    fn accepts_chat(&self, key: &str) -> bool {
        let held = self.inner.conn.lock().clone().and_then(|conn| {
            conn.suppresses_replay().then(|| conn.display_key()).flatten()
        });
        crate::session::chat_accepted(held.as_deref(), key)
    }
    fn background(&self, key: &str) {
        self.inner.hub.mark_new(key);
    }
    fn endpoint_ready(&self, _label: Option<&str>) {
        self.flush_pending();
    }
    fn cache_session(&self, key: &str) {
        self.save_cache_for(key);
    }
    fn session_stamp(&self, key: &str, updated_at: &str) {
        if !updated_at.is_empty() {
            self.inner
                .state
                .lock()
                .session_updated_at
                .insert(key.to_string(), updated_at.to_string());
        }
    }
    fn tool_run(&self, key: &str, run_id: &str) {
        let mut state = self.inner.state.lock();
        let runs = state.turn_runs.entry(key.to_string()).or_default();
        if !runs.iter().any(|id| id == run_id) {
            runs.push(run_id.to_string());
        }
    }
    fn stamp_probe(&self, key: &str, persist: bool) {
        self.refresh_stamp(key, persist);
    }
    fn is_summarizer(&self, key: &str) -> bool {
        self.inner.state.lock().summarizer_ids.contains(key)
    }
    fn summarizer_chunk(&self, key: &str, text: &str) -> bool {
        let mut state = self.inner.state.lock();
        if !state.summarizer_ids.contains(key) {
            return false;
        }
        state
            .summarizer_replies
            .entry(key.to_string())
            .or_default()
            .push_str(text);
        true
    }
    fn take_summarizer_reply(&self, key: &str) -> String {
        // Keep the id registered until the truncate reply: late updates from
        // that session must not create a store.
        self.inner
            .state
            .lock()
            .summarizer_replies
            .remove(key)
            .unwrap_or_default()
    }
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
