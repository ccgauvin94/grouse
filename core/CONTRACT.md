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
  **Known exception — the blocking-intent reality:** for operations where the
  UI cannot proceed without the reply, the core deliberately makes synchronous
  in-intent network RPCs on the calling thread. This covers `connect` (blocks,
  bounded, until ready) and the roam peer / `grouse-unstable` shim
  request/reply calls (cwd resolution, `session_info`/probe, `export`,
  tools/extension/config lists). These are bounded by the shared control-call
  timeout (`session::rpc_timeout`, 30s per non-prompt method) so a hung remote
  goose never pins the caller forever (S-RC-6); outcomes still also arrive as
  events where applicable. `session/prompt` is the exception with no timeout:
  its reply IS the turn's end.
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
    pub accept_invalid_certs: bool,  // true -> historical trust-all; false (default) -> WebPKI + hostname verify
    pub ca_cert_pem: Option<String>, // PEM CA(s) added to the verifier trust store (ignored when trust-all)
    pub cwd: String,           // absolute, must exist in the goose container
    pub auto_connect: bool,
    pub client_id: String,     // _meta.client, e.g. "grouse-desktop" | "grouse" | "grouse-cli"
    pub initial_recipe_id: Option<String>, // session/new recipeId for the fresh session
    pub initial_project_id: Option<String>, // session/new _meta.projectId; stock goose persists it to the project_id column
}
```

The transport is INTERNAL to the core (WebSocket with `X-Secret-Key` header +
verifying TLS; the self-signed-tailnet downgrade is `accept_invalid_certs`,
defaulting to real verification; roam byte stream). The UI supplies only
`ServerConfig`/roam intents, never a socket.

---

## 3. Stable interface (`grouse-core`)

### 3.1 `Core` — intents (UI → core)

| Method | Params | Notes |
|---|---|---|
| `connect(config: ServerConfig)` | — | opens the WebSocket; `initialize`; then new-or-resume per §4 |
| `disconnect()` | — | explicit close, no reconnect |
| `new_session(recipe_id: Option<String>, project_id: Option<String>)` | — | `session/new` with `_meta.client` + cwd; `recipeId`/`projectId` when given. `projectId` is persisted by stock goose at creation, so a project chat needs no post-creation `session/project/update` |
| `open_session(session_id: String)` | — | `session/load` with the real cwd (core resolves it); a settled, matching cache paints authoritatively and suppresses the replay, a settled-but-stale one merges a bounded tail, an incomplete one full-replays |
| `refresh_session(session_id: String, mode: RefreshMode)` | — | routes by id: a `roam:<label>:<raw>` id reloads on its peer (`RoamPeer::open_session_with`, `force` honoured), anything else on the main wire. `RefreshMode::Quick` lets freshness decide (a settled, current cache suppresses the replay; a settled-but-stale one merges a bounded tail; an incomplete store still replays in full); `RefreshMode::Full` forces the full `session/load` replay regardless. Either way the replay is ingested silently and ends in one windowed paint (grouse#18) |
| `list_sessions()` | — | refreshes `session/list` |
| `send_prompt(prompt: Prompt, expect: Option<SendExpect>)` | — | text/image/resource blocks; ONE path for both transports. Routes by `expect.session_id` (fallback: the displayed id) through `Core::owner_for`; an id the core has never opened is rejected instead of mis-routed. Queues per SESSION when that chat's owner is not ready or already has a turn in flight — a turn in another chat never blocks this one (turns are concurrent per session). Echoes the prompt once (`TranscriptStore::echo_prompt`) and finalizes in `Core::on_turn_ended` (run end, cache save with the incomplete-stamp rule, stamp re-baseline, per-session queue flush) |
| `cancel()` | — | `session/cancel` (notification) |
| `set_config_option(config_id: String, value: String)` | — | provider/model/mode/thinking_effort |
| `rename_session(session_id, title)` / `archive_session` / `unarchive_session` / `delete_session` | — | re-list after |
| `roam_connect(card, label)` | — | parallel peer (see §6) |
| `roam_disconnect(label)` | | |
| `roam_open_session(label, session_id)` | | |
| `tasks_refresh()` | — | My Day (§3.6): both lists arrive in ONE `on_tasks` |
| `task_close(id, state, resolution)` | — | `state` is `done` \| `dropped` \| `expired`; `resolution` is the one line that keeps the next briefing from re-raising it |
| `task_snooze(id, until, reason)` | — | `until` is an ISO instant |
| `task_react(id, reaction)` | — | `acted` \| `dismissed` \| `wrong` \| `muted` — assistant-state's only learning signal |

### 3.2 `CoreListener` — events (core → UI), one method per family

```rust
#[uniffi::export(callback_interface)]
pub trait CoreListener {
    fn on_status(&self, status: ConnectionStatus);
    fn on_sessions(&self, sessions: Vec<SessionSummary>);
    fn on_item(&self, op: TranscriptOp);                   // the rich item stream (§3.5)
    fn on_usage(&self, used: i64, size: i64, cost: f64, currency: String); // context window + cost
    fn on_run_ended(&self, session_id: String, stop_reason: String); // a turn finished
    // A sanitized, sentence-sized chunk of the assistant's visible text, ready
    // for a TTS engine (see docs/TRANSCRIPT_MODEL.md "Speaking"). Emitted as
    // the reply streams and on the finalizing flush, so a conversation loop can
    // speak before the turn ends; a client not narrating a live turn ignores it.
    fn on_speak(&self, text: String);
    fn on_config(&self, options: Vec<ConfigOption>);
    fn on_permission_request(&self, request: PermissionRequest);
    fn on_session_touched(&self, session_id: String, title: String, updated_at: String);
    fn on_projects(&self, projects: Vec<ProjectSummary>);
    // The live turn's run id for a session (session_info_update
    // _meta.goose.activeRunId); empty run_id = the run ended. The steer key:
    // a UI holds it to inject mid-turn input via GrouseUnstable::steer.
    fn on_active_run(&self, session_id: String, run_id: String);
    // Slash commands the server can execute right now (available_commands_update).
    fn on_commands(&self, commands: Vec<String>);
    // An open/resume FAILED and nothing was substituted: `session/load` was
    // rejected for `session_id` (stale/archived), or no connection config
    // existed to attempt it with. The core binds NO session for it and repaints
    // nothing — the client decides to retry the open or to start a fresh chat
    // explicitly with `new_session`. An answer to an intent, so it is not gated
    // by which chat is on screen (grouse#19).
    fn on_session_open_failed(&self, session_id: String, message: String);
    // My Day (§3.6): assistant-state's task lists as one JSON document,
    // `{"open": [...], "done": [...], "errors": {section: message}}`. Emitted
    // for every refresh and after every task action — never mid-turn, because
    // none of these calls runs a model.
    fn on_tasks(&self, tasks: String);
}
```

**A failed open never substitutes a session** (grouse#19). `open_session`/
`refresh_session` either load the requested session or report
`on_session_open_failed`; they never leave a different (fresh, empty) session
bound in its place, and a failed load leaves its owner with **no** session
bound (`active_session_id()` → `None`; the displayed key in `SessionHub` is
still the requested chat, so its painted rows stay on screen). The store keeps
whatever the client's cache painted for the requested session — it stays
provisional, so it can never suppress the replay of a later successful load —
and no turn can end for a session nothing is bound to, so `save_cache_for`
cannot re-stamp a cache the open never confirmed.

**An open/reload paints ONCE, at the end** (grouse#18). While a `session/load`
replay is in flight the store ingests every row silently; on completion the client
receives exactly one paint — `Reset` + the newest `window()` items as `Upsert`s +
`Window`. A client therefore lands on the most recent messages and reaches the
rest through `load_older`; it never watches the history re-render from the first
prompt. A failed load releases the hold without painting, leaving the provisional
cache paint in place.

### 3.3 State getters (snapshots the UI may read)

- `status(): ConnectionStatus` — `Disconnected | Connecting | Ready | Syncing | Error(String)`. `Alert(String)` is the advisory variant: a one-off control-path failure (e.g. a `session/set_config_option` the peer refused, emitted when a provider/model pick never lands) delivered with the connection otherwise untouched — clients surface it as text only and must NOT treat it as the wire being down.
- `ready(): bool`
- `active_session_id(): Option<String>`
- `sessions(): Vec<SessionSummary>` — carries the full session metadata the
  UIs render: id, title, updated_at, last_message_snippet, project_id,
  message_count, model, has_recipe, plus `archived` (set from the server's
  `_meta.archivedAt`; `session/list` has no archived filter, so the flag —
  not a drop — is how a UI distinguishes archived chats and restores them via
  `unarchive_session`).
- `transcript(): Vec<Message>` — the accumulated active-session transcript
- `rich_transcript(): Vec<Item>` — the same transcript as rich items (§3.5)
- `item(id): Option<Item>` — one item by id
- `window(): TranscriptWindow` — the pagination cursor
- `config(): Vec<ConfigOption>` — carries id, value, name, and the choices
  list for dropdowns (empty on the config_option_update path; the schema
  has no choices there).
- `load_older(count: u32)` — an **intent**: extend the window backward by
  `count` items, cache-first; outcomes arrive as `on_item` ops (§3.5).

### 3.4 (removed) the flat stream enum

The old `on_stream` / `StreamEvent` channel is gone (docs/TRANSCRIPT_MODEL.md
phase 4). Every transcript row — text, tool call, tool update, chart, MCP app —
arrives on `on_item` (§3.5); the two things that are not transcript rows have
their own methods: `on_usage` and `on_run_ended`.

`ToolCallKind` survives as the wire-derived kind (`Plain | Chart(spec) |
McpApp { app_key, uri, extension, input }`) that the core maps into an `Item`.

**Late MCP-App hydration.** goose attaches `_meta.goose.mcpApp` to the COMPLETING
`tool_call_update`, not the `tool_call` frame (the tool's `ui://` resource only
resolves after the call ran). The core then emits an `Upsert` carrying
`kind = McpApp` for the SAME `tool_call_id`, which clients apply as an in-place
update (match on `tool_call_id`) — appending blindly duplicates the row. A
second promotion of the same id is a core-side no-op.

### 3.5 Rich items (the transcript model, phase 1)

`docs/TRANSCRIPT_MODEL.md` is the design. `on_item` is the ONLY transcript
channel; the legacy `on_transcript` / `on_stream` surface is deleted.

```rust
pub enum ItemKind { User, Agent, Thought, Tool, ToolGroup, Chart, McpApp, Error }

pub struct SubagentStep { name, detail }

pub struct ToolCall { id, title, detail, output, status, thought, seq, steps }

pub struct Item {
    id: String,          // message_id (text) / tool_call_id, or a core "@n"
    kind: ItemKind,
    text: String,        // body / tool-app-chart TITLE
    detail: String,      // tool input / chart spec / app input
    output: String,      // tool result
    status: String,      // tool lifecycle
    app_key: String,     // McpApp: "<extension>|<uri>"
    calls: Vec<ToolCall>,// ToolGroup children (each with its own `seq`)
    thought: String,     // Tool/ToolGroup: the run's thinking, joined
    summary: String,     // Tool/ToolGroup: the run's one-line label
    steps: Vec<SubagentStep>, // Tool: a delegate's child calls (bounded)
    attachments: u32,    // User: how many images/resources rode the prompt
    images: Vec<String>, // image pixels as data URIs ("data:<mime>;base64,...")
    seq: u32,            // transcript position; clients insert by this
}

pub enum TranscriptOp {
    Reset  { session_id: String },      // the window was replaced wholesale
    Upsert { item: Item },              // insert or replace by id (authoritative)
    AppendText   { id: String, chunk: String },
    AppendOutput { id: String, chunk: String },
    Remove { id: String },
    Window { oldest_id: String, has_older: bool },
}

pub struct TranscriptWindow { oldest_id: String, newest_id: String, has_older: bool }
```

Rules:

- **`Upsert` is idempotent and authoritative**; the `Append*` ops are O(chunk)
  streaming shortcuts, so a dropped delta still converges on the next `Upsert`.
- **A fresh text row emits `Upsert` first**, then `AppendText` deltas. The
  stream's end (a new bubble, a tool call, or the turn ending) emits one
  authoritative `Upsert` with the final text — so a client renders markdown a
  single time, never per chunk. A live tool output emits `AppendOutput`; the
  completion update emits `Upsert`.
- **`Window` is the pagination cursor**: `oldest_id` is the oldest item the core
  has emitted and `has_older` says whether `load_older` can still produce more.
- Rich items make the cache and any rebuild lossless: a chart keeps its spec, an
  MCP app keeps `app_key`, a toolgroup keeps its children. `TRANSCRIPT_CACHE_VERSION`
  is bumped so a pre-rich cache reads as absent and is rewritten by the replay.
- A **cache paint** rebuilds the store from rich items and emits `Reset` + the
  newest `WINDOW` (60) items + `Window`. The **store keeps the whole transcript**
  (the cache and the legacy flat projection are unaffected); only the item
  stream is windowed, so a long chat does not realize every delegate on open.
  A `session/load` replay then continues the same id-keyed stream.
- `load_older(count)` walks the window back over the store's in-memory rows —
  no cache read and no wire call. A stock server has no history cursor, so once
  the store's start is reached `has_older` is `false`.
- **The core echoes the prompt it sends.** goose does NOT send a
  `user_message_chunk` for a prompt WE sent — the prompt next crosses the wire in
  a later `session/load` replay (verified against a live serve). Without a local
  copy the core's transcript, and the cache saved from it, holds no record of
  what was asked, so a reopen that trusts the cache shows the replies with the
  questions missing. `spawn_prompt` therefore appends a pure-text prompt to the
  store as a `User` item. Attachments are the exception: `Item` has no image
  field, so an attachment prompt stays the CLIENT's own local echo.
- **A run IS the loop** (issue #13). Everything the model thought and did between
  two visible messages is ONE `toolgroup` item, with the loop inside it: each
  `ToolCall` carries the `thought` that immediately preceded IT (absorbed from the
  `Thought` row, which is dropped with a `Remove`), so an expanded run reads
  think → act → think → act. A thought therefore does NOT split a run — the model
  thinking between two calls is the same loop, so a 20-call loop is one row, not
  twenty. A **visible message** (the model's narration, or your next prompt) is
  what ends a run. `Item.thought` is the run's thinking joined, and `Item.summary`
  its label, derived from the first thinking it saw; both are empty when a run has
  nothing to draw on and the client then falls back to the call titles. Thinking
  that leads nowhere stays a `Thought` item of its own, and the turn's end seals
  it so a later turn's call can never claim it.
- **The label is rewritten by the model at the end of a turn.** `summary` as the
  store sets it is provisional; after the turn the core asks the model for one
  short line per run and replaces the label with an `Upsert`. The request runs on
  a **hidden session** (`session/new` with `_meta.hidden` — goose's `hidden`
  session type is not listable, so it can never appear as a chat), is emptied
  after every use (`_goose/unstable/session/conversation/truncate`), and is one
  inference per turn that never delays a reply. `set_run_summaries(bool)` /
  `run_summaries()` toggle it (on by default); with it off, or wherever the model
  does not answer, the provisional label stands.
- **Outbound images are rescaled once, in the core** (§8.1). Every
  `PromptBlock::Image` the UI hands in is downscaled to a 1600 px long edge
  before it reaches the wire, the echoed row, or the transcript — one
  `imgrescale::rescale()` hook at `send_prompt` entry covers all three, so a
  send, its echo and every later replay carry the same (small) pixels. JPEG
  re-encodes at q80; PNG stays PNG (and anything with meaningful alpha
  stays PNG with it), so screenshots keep their format. WebP/HEIC or a
  corrupt/unreadable payload passes through **unchanged** with an
  `Alert("sent N image(s) in their original size…")` — compression must
  never become "the message did not send". `set_image_rescale(bool)` /
  `image_rescale()` are a UI preference (default **ON**), persisted
  client-side; the core only applies it.
- **A replayed thought never becomes a row of its own.** goose re-sends the
  reasoning trail on every `session/load`, but such a row would double the trail
  and carries no stable id to reconcile by. The core holds it as the thinking of
  the call that follows it (`replay_thought`), so a run's `summary`/`thought`
  rebuild identically from a replay — the label is derived from the replayed rows,
  never persisted against a `tool_call_id`.
- **There is one store per session, not one per transport.** `SessionHub` owns
  exactly one `TranscriptStore` per display session id — the main connection's
  sessions and every roam peer's alike. A `SessionGate` per store forwards
  `on_item` / `on_usage` / `on_speak` only while that key is the displayed one,
  and always forwards turn control (`on_run_ended`, `on_session_open_failed`,
  touched/status events). A background session therefore keeps accumulating (and
  is cached) without painting, and its content sets the drawer's green dot
  (`SessionHub::mark_new` → `on_session_touched`). A peer session (`roam:<label>:<raw>`)
  gets the same rich items, the same window and the same `load_older` —
  `Core::load_older` walks the displayed store, whichever transport produced it.
  The transport differs only in dial/framing, reconnect budget, browse-mode
  session list and id namespace (`session::display_id` / `ChatEndpoint::wire_id`);
  routing and painting are the core's, keyed by display id.

### 3.6 My Day (assistant-state items)

The clients' "My Day" tab is assistant-state's own working state — open loops,
waiting-ons, obligations — read over a **deterministic `_goose/unstable/tools/call`**,
not a prompt. One call is a few milliseconds of SQLite on the server; a model
turn would be a 60k-token prefill to answer "what is open", which is the whole
reason this path exists.

- **`on_tasks`** carries
  `{"open": [...], "done": [...], "tasks": [...], "errors": {...}}`. All three
  sections are fetched together (`state_items` with `state=open,snoozed`, with
  `state=done,expired`, and `task_list` with `state=running,review`) and emitted
  in ONE callback, so a UI can never paint a half-updated set. `errors` maps a
  section name to why it failed and is omitted when all succeeded — "nothing to
  do" and "could not ask" must not look alike.
- **`tasks` is handed-over work, not items.** An item is a fact that needs
  attention (re-observed, merged, evidenced, deduped); a task is an act —
  something was asked for, something did it, and a RESULT came back. Fields: id,
  title, state (`running` | `review`; the terminal states are not listed here),
  thread, `session_id`, `request` (trimmed to 300 chars), `result`, artifacts
  (`[{path, …}]`, paths in the agent's own workspace), `created_at`,
  `updated_at`. `review` means the work stopped and is waiting for a human, which
  is the only task state a tab has to make loud. A server older than
  assistant-state v4 has no `task_list`, and that shows up as `errors.tasks`
  rather than as an empty section that reads like "nothing was handed over".
- **Rows are slimmed by the core** (`tasks.rs`, `KEPT_FIELDS`): id, kind, state,
  title, detail, owner, `due_at`, salience, confidence, resolution,
  `snooze_until`, and `thread` — the area's **label** as one string
  (`chami-billing`), dropped when blank. assistant-state's rows
  also carry evidence, provenance and dedupe keys; those stay on the server — they
  are audit trail, not row content, and shipping them made one refresh 82 KB.
  The label is normalised in the core because the server's own shape changed under
  the client: assistant-state schema v3 (2026-10-29) replaced the `thread` table
  with a nullable label on the item, so `thread` answers as a string where it used
  to answer `{slug, title}`. `normalise_thread` accepts both and always emits the
  string, so an app built against this contract has one field to render and an
  older server cannot blank it. A thread is a grouping hint, not a container: one
  label may hold many items over its life, and nothing keys on it.
- **`open` holds snoozed rows too, so `snooze_until` is what separates them.** A
  snoozed item is still open server-side, only quiet until its wake time, which is
  why the open query is `state=open,snoozed`. A client MUST group rather than
  render them alike: put rows whose `snooze_until` is still in the future under
  their own heading with the wake time, or a Snooze tap looks like a no-op (it did).
  A snoozed row whose `until` has passed reads as open again — the server's wake
  pass (`rank`/`maintain`) flips it, and until that runs the only truth is the
  timestamp. Waking early is `state_snooze` with an already-past instant, not a
  separate tool.
- **The calls run on a dedicated HIDDEN session in `auto` mode.** goose refuses
  an app tool call in every mode but `auto`
  (`crates/goose/src/acp/server/tools.rs`: *app tool calls require auto mode*).
  Flipping the chat thread to `auto` would disarm its approvals, and patching the
  fork to allow app calls under `smart_approve` would be a security bypass — so
  the core mints one `_meta.hidden: true` session (never listed, never prompted,
  its context stays empty) and sets it to `auto`. It is created on first use and
  **dropped on any failed call**, which is what recovers from a server restart or
  from goose evicting it.
- **The params are a contract, not free-form:** `extensionName` is the
  extension's display name (`Assistant State`; goose keys it through
  `name_to_key`), and the TOOL name must carry the extension key prefix
  (`assistantstate__state_items`) — goose compares tool names verbatim.
- **Freshness is pull-shaped, by design.** Task state changes inside goose's own
  scheduled sessions (the briefings), which `session/update` — session-scoped —
  cannot report. So there is no push: clients refresh on tab open, on an explicit
  refresh, and after every action (the core re-reads and re-emits; a UI never
  patches its own list).

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
- Deliberate wire replacement emits no terminal `Error`: `new_session`/
  `open_session`/`disconnect()` shut the previous connection down, and a
  deliberately closed wire reports nothing — the replacement handshake owns the
  status story (`Connecting` → `Ready`), or `disconnect()`'s own `Disconnected`
  does. Only an UNEXPECTED end surfaces `Error(String)`.
- Keepalive: the WebSocket transport pings an idle connection (ping after 30s
  of silence, checked every 5s; no reply within 15s ⇒ the connection is
  declared dropped and the reconnect above fires). Traffic suppresses pings
  entirely — any inbound byte rebases the idle timer, so a streaming turn
  never pays for one. This is what turns a silently reaped NAT/proxy path into
  a visible reconnect instead of a hung socket; UIs must not add their own
  ping/idle probes.
- One session pipeline for every transport (`core/grouse-core/src/session.rs`):
  the same ACP parameter builders, wire→item translators, `PaintPlan` cache
  policy, `SessionHub` store/display gate, and `session/update` `dispatch`.
  `spine::Conn` (WebSocket) and `roam::RoamPeer` (iroh) own only transport
  concerns — dial/framing, reconnect budget, browse-mode session list, and id
  namespace — and both implement `ChatEndpoint`, the single transmit surface.
  A protocol change lands in `session.rs` once, never once per transport.
- One prompt path: `send_prompt` resolves the owner of the displayed id, queues
  per session when the owner is not ready or a turn is running, echoes the
  prompt once, and `on_turn_ended` finalizes (run end, cache save, stamp
  re-baseline, queue flush).
- Remote-change resync: `session_info_update` debounced → probe on the session's
  owner → in-place `session/load` replay, re-probed a few times at 8s for a
  still-streaming turn. Runs for peer sessions too. This replaces BOTH the
  desktop's `sessionTouched` resync and Android's `turnResyncTick` — one
  implementation, no drift.
- Transcript accumulation: chunks append into a bubble keyed by (role,
  `message_id`); live shell output appends, the completion update replaces;
  replay chunks are gated by message id. Caches (per-session transcript + tool
  catalog) are the core's, keyed by display id, with the freshness check
  (`updatedAt == cachedUpdatedAt`); a peer session's cache key is
  `roam:<label>:<raw>`, and `Core::save_cache_for` writes an empty stamp for an
  incomplete store on every transport.

---

## 5. Unstable interface (`grouse-unstable`)

Separate `GrouseUnstable` interface, clearly marked for retirement. Methods
(the fork shim, from the inventory):

- `steer(text, expected_run_id)` — inject into the running turn
- `export_session(session_id)` → `on_export(data)` event
- `session_info(session_id)` → probe (used by resync + cwd resolution)
- `session_project(session_id, project_id?)` — move between projects
- `list_tools(session_id)`, `session_extensions_list/add/remove(session_id, …)` — `remove` takes the extension's KEY (session list `extensionKey` = global list `configKey`), not the display name: goose renamed the wire param and rejects `name` now. UIs must match rows by key, since `extension.name` can differ from the key ("Extension Manager" vs `extensionmanager`)
- `list_global_extensions()`, `set_extension_enabled(config_key, enabled)`, `add_extension(…)` — `set_extension_enabled` takes the global list's `configKey` (goose renamed the wire param from `name` and rejects it now)
- `sources_list/create/delete/update` (projects + skills) — `sourcesCreate(type, name, description, content)` creates a GLOBAL source; project-scoped skills were removed (goose is migrating sources CRUD out of the server — grouse#5)
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

Custom notifications (gated on `customNotifications`) — each starts with a
session gate: a notification carrying a `sessionId` that is not the displayed
chat is dropped (goose's own scheduled runs status/compact constantly; without
the gate they painted "compacting" and other sessions' numbers onto the idle
client): `status_message` → `on_compaction_status`; `usage_update` →
`on_usage(used, size, cost)`; `message_usage` → `on_message_usage`.
`usage_update` is the context-window opposite number of the standard ACP
`UsageUpdate`, and — because roam only transports custom notifications — the
thing that keeps a roam session's context bar live at all.

> **Raw-JSON listeners (deliberate, documented here).** The unstable shim's
> list families deliver their payloads to the UI **verbatim as JSON strings**
> on `GrouseUnstableListener`: `on_recipes`, `on_projects`, `on_skills`,
> `on_tools`, `on_extensions`, `on_session_extensions`, `on_supported_models`,
> `on_providers`, `on_config_value`, `on_export`, `on_session_probe`,
> `on_app_resource`, `on_tool_result`, and the dictation pair
> `on_dictation_config` / `on_transcript` (+ the error/compaction/message
> families). The fork wire shapes are intentionally NOT mirrored into typed
> uniffi records here — the shim is slated for retirement (GDK absorbs each
> feature), so the UI parses these JSON payloads directly and the shim is
> dropped without touching the stable contract. This is the documented
> exception to the "typed event per family" rule in §1.
> One shim-side enrichment matters: `on_providers` entries carry
> `hasCredentials` (whether any of the provider's own config fields is actually
> set — env vars count for secret keys), because goose's `configured` also marks
> local endpoints and detected CLIs usable with nothing configured. A picker that
> means "the user set this up" must require `configured && hasCredentials`.

### Dictation (speech-to-text)

`GrouseUnstable::dictation_config()` → `_goose/unstable/dictation/config`: the
provider status map as raw JSON on `on_dictation_config` (same list-reply
convention as the other shim families).

`GrouseUnstable::dictation_transcribe(audio, mime_type, provider)` →
`_goose/unstable/dictation/transcribe`: the decoded text on `on_transcript`.

`audio` is base64; `mime_type` ∈ `audio/wav|webm|mp4|m4a|mpeg`; `provider` ∈
`openai|groq|elevenlabs|local|model`. goose's dictation is **one-shot** — the
clip is the whole utterance and there are no partials — and the call is a
blocking round-trip (decode + provider), so a UI must call it off its UI thread.
This is the STT half of the voice loop; the TTS half is client-side and is fed
by the `on_speak` stream (§3.2, `docs/TRANSCRIPT_MODEL.md`).

---

## 5.5 Notification policy (`grouse-core::notify`)

Two namespace-level functions (no handle, no state) exported over uniffi and, as
JSON in/JSON out, over the C ABI (`grouse_push_parse`, `grouse_push_decide`). They
are **the** implementation of "what does this payload mean, and should it interrupt
the user" — clients contribute only transport and rendering, so a sender's payload
behaves identically on the phone and the desktop.

- `parse_push(raw: String) -> PushEnvelope` — `{kind: Turn|Briefing, session_id?,
  text}`. Never fails: a body that is not a JSON object (or does not parse) is a
  briefing carrying the raw text, because a sender with a broken envelope should
  still reach the user.
- `decide_notify(envelope, ctx) -> NotifyDecision` — `{show, summary, body}`.
  `NotifyContext` is what the client knows: `app_visible` (Android: foreground;
  desktop: active window), `armed_session` (the session this device last sent to),
  `session_title` (where known — the desktop's sidebar has it, a push to a sleeping
  phone does not), `announce_any_turn` (true for a single-client desktop, false for
  the phone, which suppresses turns it did not arm), and `announced_session` /
  `announced_secs_ago` — the last turn the client announced itself, so the same turn
  end arriving twice (the live connection sees it; the operator's sender pushes for
  it) is announced once. Senders cannot disambiguate: goose's `Stop` hook payload
  carries no run id, so a bounded recency window is the available identity.

Wording (`"Grouse replied"`, `"Grouse briefing"`, the empty-transcript fallback)
lives here too: the two clients used to say different things for the same event.
An announcer passes `announced_*` as unset: this is the *first* sighting, and the
dedupe exists only to silence the other path, never a genuine second turn.
Clients gate approval requests and "a session changed elsewhere" themselves — those
are client-local events, not push kinds.

---

## 6. Roam (parallel peers)

The core owns the peer registry (the desktop's `m_roamPeers`). Peers are
parallel connections in browse mode. A peer's connection is supervised: an
unexpected drop re-dials with the core's backoff curve and resumes the session
that was open (a peer that was never live gives up after the main connection's
6-try budget; one that dropped after reaching ready retries far longer — see
`RoamPeer::RECONNECT_MAX`). Intents (`open_session`, `new_session`) also queue
during the reconnect window the same way they queue on the main connection.
Surfaced as:

**A peer is a transport, not a second session implementation.** Both wires feed
the one pipeline in `core/grouse-core/src/session.rs`: the same ACP parameter
builders, the same `session/update` `dispatch(DispatchHost, ..)` (a peer passes a
`PrefixHost` so ids land in the `roam:` namespace), the same `SessionHub` store
per display id with the same display gate, the same `PaintPlan` cache policy,
and the same `ClientHandlers`/`connect_client` SDK client — so turn-end custom
notifications (`status_message`, `message_usage`) and recipe/elicitation server
requests work on a peer exactly as on the main connection. `RoamPeer` owns only
the transport: iroh dial + newline framing, its reconnect supervisor and budget,
its browse-mode `session/list`, and its id prefix (`ChatEndpoint::wire_id` strips
it, `display_id` adds it). Chat routes by display id: `Core::owner_for` picks the
peer for a `roam:<label>:<raw>` id and the main `Conn` otherwise, and
`Core::refresh_session` reloads a peer session on its peer
(`RoamPeer::open_session_with`, `force` honoured). Peer transcripts are cached
under `roam:<label>:<raw>`, saved by the core's policy
(`Core::save_cache_for`, incomplete ⇒ empty stamp), and a background peer chat
sets the drawer's green dot via `SessionHub::mark_new` instead of painting.

- intents: `roam_connect(card, label)`, `roam_disconnect(label)`,
  `roam_open_session(label, session_id)`
- events: `on_roam_peer_status(label, status)`, `on_roam_sessions(label, Vec<SessionSummary>)`
- the peer's session id namespace uses the `roam:<peer>:<id>` prefix for
  sidebar grouping, exactly as today.
- session-bound unstable RPCs (tools list/call, session extensions, project,
  working-dir, resources, export) ROUTE to the owning peer's connection when
  the session id carries the `roam:` prefix — the peer answers its own
  sessions' tool/extension queries (the in-chat N-tools indicator works there).
- wire ids vs app ids: the `roam:` prefix is CLIENT-side only. The peer's own
  connection rewrites `sessionId` to its raw id at its single rewrite point
  (`RoamPeer::rpc` → `wire_params`); events echoed back to the app carry the
  app-facing prefixed form.
- the peer's `session/list` only returns sessions that HAVE messages (goose
  filters empty ones), so a just-created chat is invisible to a relist. The
  peer layer unions its open session into every emitted list — the drawer
  always shows the open chat; the next list (first message landed) replaces
  the synthetic row with server truth.

The roam transport stays the shared `grouse-roam-core` library (iroh), now a
dependency of `grouse-core`'s transport layer.

---

## 7. Resolved decisions (locked)

1. **One `CoreListener`** with many typed methods (one per event family) — a
   single callback interface; the C ABI mirrors it as one function-pointer table.
2. **Sync intents + callback events.** Every `Core` method is a synchronous
   fire-and-forget into the core's tokio runtime; results arrive on
   `CoreListener`. No async uniffi methods. See the blocking-intent exception
   in §1 (roam/unstable RPCs that must block on the reply).
3. **Full-vector `transcript()` getter.** The UI diffs against its last render.
4. **UI supplies `CacheDir` at `Core` construction**; the core owns the cache
   files under it.

---

## 8. Trust boundary for server-provided content

The core delivers server-provided content to the UI **verbatim**: `Message`
text (`Message.content`), tool output (`ToolCallUpdate.output`), app resources
(`appHtml`/`resources_read`), and chart specs (`ToolCall.kind == Chart(spec)`).
The core never escapes, sanitizes, or re-renders this content — it is raw text
and raw server-rendered HTML/JS as the goose server produced it.

Consequences for every UI (this is a policy, not implemented in the core):

- The server is **untrusted** at the rendering boundary. All bytes that
  originate from `content`/`output`/`appHtml`/chart specs MUST be treated as
  untrusted by the UI that renders them.
- **Plain-text renderers are safe by construction.** Rendering
  `Message.content` as plain text (no parser that interprets HTML/JS) cannot be
  injected into.
- **Any HTML/JS surface MUST sanitize or sandbox at that surface.** A WebView
  that renders `appHtml`, or a RichText/HTML renderer for `Message.content`,
  must apply a Content-Security-Policy, a sandboxed iframe, and/or HTML
  sanitization before the bytes can reach the document. Escaping in the core
  would corrupt servers that legitimately send HTML (see S-RC-8): the guard
  belongs at render time, owned by the platform UI.
- Chart specs are embedded as JavaScript string literals; a UI embedding them
  into a `<script>` element MUST JSON-escape the spec and neutralize `<` so the
  spec cannot terminate the surrounding script element.

Root policy reference: `AGENTS.md` protocol notes → "Trust boundary".
