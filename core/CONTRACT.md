# Grouse Core Contract

The API boundary between the Rust core and every thin native UI. This is the
single source of truth; the uniffi interface (and the desktop's mirror C ABI)
are generated from it. Read the monorepo AGENTS.md first.

**Model: stateful core.** The core owns the connection, the session list, the
active session's transcript, caches, reconnect/backoff, and remote-change
resync. UIs render state and send intents — they never reimplement client logic.

**Threading.** The core owns a tokio runtime on a worker thread. UI → core
intents are fire-and-forget (or short-blocking); core → UI events arrive on the
`CoreListener` callback. A UI must marshal events onto its own main thread, as
it does today.

**Two crates, one contract.**
- `grouse-core` — stable ACP (the durable surface).
- `grouse-unstable` — the goose-fork `_goose/unstable/*` shim, retired as GDK
  absorbs each feature. Exposed as a SEPARATE interface so it can be dropped
  without touching the stable contract.

---

## 1. Runtime model

```
UI (native)                    core (Rust, tokio)
  intent ───────────────────▶  enqueue / dispatch
  ◀────────────────── event    CoreListener callback
  getter ───────────────────▶  snapshot record (owned by core)
```

- **Intents** never return the result of network work; they change a state
  machine or queue a request. Outcomes arrive as events.
- **Events** arrive as typed methods on `CoreListener` (one per event family).
- **Getters** return immutable snapshots (`Record`s). They are cheap; the core
  is authoritative, the UI mirrors what events tell it and may re-read a
  getter to resync.

---

## 2. Connection & configuration

```rust
#[uniffi::record]
pub struct ServerConfig {
    pub host: String,          // hostname or IP, no scheme
    pub port: u16,
    pub secret_key: String,    // X-Secret-Key
    pub use_tls: bool,
    pub cwd: String,           // absolute, must exist in the goose container
    pub auto_connect: bool,
    pub client_id: String,     // _meta.client, e.g. "grouse-desktop" | "grouse" | "grouse-cli"
}
```

The transport is INTERNAL to the core (WebSocket with `X-Secret-Key` header +
trust-all TLS; roam byte stream). The UI supplies only `ServerConfig`/roam
intents, never a socket.

---

## 3. Stable interface (`grouse-core`)

### 3.1 `Core` — intents (UI → core)

| Method | Params | Notes |
|---|---|---|
| `connect(config: ServerConfig)` | — | opens the WebSocket; `initialize`; then new-or-resume per §4 |
| `disconnect()` | — | explicit close, no reconnect |
| `new_session(recipe_id: Option<String>)` | — | `session/new` with `_meta.client` + cwd |
| `open_session(session_id: String)` | — | `session/load` with the real cwd (core resolves it) |
| `list_sessions()` | — | refreshes `session/list` |
| `send_prompt(prompt: Prompt, expect: Option<SendExpect>)` | — | text/image/resource blocks |
| `cancel()` | — | `session/cancel` (notification) |
| `set_config_option(config_id: String, value: String)` | — | provider/model/mode/thinking_effort |
| `rename_session(session_id, title)` / `archive_session` / `unarchive_session` / `delete_session` | — | re-list after |
| `roam_connect(card, label)` | — | parallel peer (see §6) |
| `roam_disconnect(label)` | | |
| `roam_open_session(label, session_id)` | | |

### 3.2 `CoreListener` — events (core → UI), one method per family

```rust
#[uniffi::export(callback_interface)]
pub trait CoreListener {
    fn on_status(&self, status: ConnectionStatus);
    fn on_sessions(&self, sessions: Vec<SessionSummary>);
    fn on_transcript(&self, event: TranscriptEvent);       // append / update / clear
    fn on_stream(&self, event: StreamEvent);               // chunk, tool_call, tool_update, usage
    fn on_config(&self, options: Vec<ConfigOption>);
    fn on_permission_request(&self, request: PermissionRequest);
    fn on_session_touched(&self, session_id: String, title: String, updated_at: String);
    fn on_projects(&self, projects: Vec<ProjectSummary>);
}
```

### 3.3 State getters (snapshots the UI may read)

- `status(): ConnectionStatus` — `Disconnected | Connecting | Ready | Syncing | Error(String)`
- `ready(): bool`
- `active_session_id(): Option<String>`
- `sessions(): Vec<SessionSummary>`
- `transcript(): Vec<Message>` — the accumulated active-session transcript
- `config(): Vec<ConfigOption>`

### 3.4 Stream event enum (what `on_stream` carries)

`AgentChunk(text, message_id) · UserChunk(text, message_id) · ThoughtChunk(text) ·
ToolCall { title, detail, tool_call_id, kind } · ToolCallUpdate { id, status, output, live } ·
Usage { used, size, cost, currency } · RunEnded(stop_reason)`

`ToolCall.kind` collapses the desktop's toolgroup/chart/mcpapp split into:
`Plain | Chart(spec) | McpApp { app_key, uri, extension, input }`.

---

## 4. Session lifecycle (owned by the core)

- `ready` ⇔ an open session id exists. `send_prompt`/`set_config_option`/tool
  queries queue until `ready`, then flush in order.
- Resume (`open_session`) resolves the session's real cwd: per-session cache →
  `_goose/unstable/session/info` probe (unstable) → the cwd carried in
  `session/list`. Never guess — this is a protocol footgun.
- Reconnect: exponential backoff (500ms·2^n, cap 15s, 6 attempts) on unexpected
  drop, reset on `Ready`; no reconnect on explicit `disconnect()`. Owned here,
  surfaced only via `on_status`.
- Remote-change resync: `session_info_update` debounced → probe → in-place
  `session/load` replay, re-probed a few times at 8s for a still-streaming turn.
  This replaces BOTH the desktop's `sessionTouched` resync and Android's
  `turnResyncTick` — one implementation, no drift.
- Transcript accumulation: chunks append into a bubble keyed by (role,
  `message_id`); live shell output appends, the completion update replaces;
  replay chunks are gated by message id. Caches (per-session transcript + tool
  catalog) are the core's, keyed by session id, with the freshness check
  (`updatedAt == cachedUpdatedAt`).

---

## 5. Unstable interface (`grouse-unstable`)

Separate `GrouseUnstable` interface, clearly marked for retirement. Methods
(the fork shim, from the inventory):

- `steer(text, expected_run_id)` — inject into the running turn
- `export_session(session_id)` → `on_export(data)` event
- `session_info(session_id)` → probe (used by resync + cwd resolution)
- `session_project(session_id, project_id?)` — move between projects
- `list_tools(session_id)`, `session_extensions_list/add/remove(session_id, …)`
- `list_global_extensions()`, `set_extension_enabled(name, enabled)`, `add_extension(…)`
- `sources_list/create/delete/update` (projects + skills)
- `config_read(key)`, `config_upsert(key, value)`, `supported_models(provider)`
- `resources_read(session_id, uri, extension)` → app html
- `recipes_list/schedule/save/delete`, `schedules_list/pause/unpause/run_now/delete/update`
- `working_dir_update(session_id, dir)`, `tools_call(session_id, name, args)`

Server→client requests (both stable and unstable), answered by the CORE, with
the UI prompted only where a human decision is needed:

- `session/request_permission` → `on_permission_request` → UI answers
  `respond_permission(tool_call_id, outcome)` where `outcome = Selected(option_id) | Cancelled`.
- `_goose/unstable/session/recipe/request-params` → core auto-answers with
  defaults; if the UI opts into forms, `on_recipe_params` + `respond_recipe_params`.
- `elicitation/create` (form mode) → `on_elicitation` + `respond_elicitation`
  (this is Android-only today; carried so the contract is complete).

Custom notifications (gated on `customNotifications`): `status_message` →
`on_compaction_status`; `message_usage` → `on_message_usage`.

---

## 6. Roam (parallel peers)

The core owns the peer registry (the desktop's `m_roamPeers`). Peers are
parallel connections in browse mode; chat routes to whichever session was last
opened. Surfaced as:

- intents: `roam_connect(card, label)`, `roam_disconnect(label)`,
  `roam_open_session(label, session_id)`
- events: `on_roam_peer_status(label, status)`, `on_roam_sessions(label, Vec<SessionSummary>)`
- the peer's session id namespace uses the `roam:<peer>:<id>` prefix for
  sidebar grouping, exactly as today.

The roam transport stays the shared `grouse-roam-core` library (iroh), now a
dependency of `grouse-core`'s transport layer.

---

## 7. Resolved decisions (locked)

1. **One `CoreListener`** with many typed methods (one per event family) — a
   single callback interface; the C ABI mirrors it as one function-pointer table.
2. **Sync intents + callback events.** Every `Core` method is a synchronous
   fire-and-forget into the core's tokio runtime; results arrive on
   `CoreListener`. No async uniffi methods.
3. **Full-vector `transcript()` getter.** The UI diffs against its last render.
4. **UI supplies `CacheDir` at `Core` construction**; the core owns the cache
   files under it.
