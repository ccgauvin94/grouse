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
  tools/extension/config lists). These are bounded by
  `RoamPeer::RPC_TIMEOUT` (30s) so a hung remote goose never pins the caller
  forever (S-RC-6); outcomes still also arrive as events where applicable.
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
    // The live turn's run id for a session (session_info_update
    // _meta.goose.activeRunId); empty run_id = the run ended. The steer key:
    // a UI holds it to inject mid-turn input via GrouseUnstable::steer.
    fn on_active_run(&self, session_id: String, run_id: String);
    // Slash commands the server can execute right now (available_commands_update).
    fn on_commands(&self, commands: Vec<String>);
}
```

### 3.3 State getters (snapshots the UI may read)

- `status(): ConnectionStatus` — `Disconnected | Connecting | Ready | Syncing | Error(String)`
- `ready(): bool`
- `active_session_id(): Option<String>`
- `sessions(): Vec<SessionSummary>` — carries the full session metadata the
  UIs render: id, title, updated_at, last_message_snippet, project_id,
  message_count, model, has_recipe, plus `archived` (set from the server's
  `_meta.archivedAt`; `session/list` has no archived filter, so the flag —
  not a drop — is how a UI distinguishes archived chats and restores them via
  `unarchive_session`).
- `transcript(): Vec<Message>` — the accumulated active-session transcript
- `config(): Vec<ConfigOption>` — carries id, value, name, and the choices
  list for dropdowns (empty on the config_option_update path; the schema
  has no choices there).

### 3.4 Stream event enum (what `on_stream` carries)

`AgentChunk(text, message_id) · UserChunk(text, message_id) · ThoughtChunk(text) ·
ToolCall { title, detail, tool_call_id, kind } · ToolCallUpdate { id, status, output, live } ·
Usage { used, size, cost, currency } · RunEnded(stop_reason)`

`ToolCall.kind` collapses the desktop's toolgroup/chart/mcpapp split into:
`Plain | Chart(spec) | McpApp { app_key, uri, extension, input }`.

**Late MCP-App hydration.** goose attaches `_meta.goose.mcpApp` to the COMPLETING
`tool_call_update`, not the `tool_call` frame (the tool's `ui://` resource only
resolves after the call ran). When that happens the core promotes the transcript
bubble and RE-ISSUES `ToolCall{ tool_call_id, kind = McpApp }` for the same id,
followed by the matching `on_transcript` Update (or Update+Append when the row
was inside a collapsed toolgroup). Clients must treat a re-issued ToolCall as a
CONVERT-IN-PLACE instruction (match on `tool_call_id`; desktop rewrites the chip
row, Android rebuilds the bubble from the re-stashed kind) — appending blindly
duplicates the row. A second promotion of the same id is a core-side no-op.

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
- `list_tools(session_id)`, `session_extensions_list/add/remove(session_id, …)` — `remove` takes the extension's KEY (session list `extensionKey` = global list `configKey`), not the display name: goose renamed the wire param and rejects `name` now. UIs must match rows by key, since `extension.name` can differ from the key ("Extension Manager" vs `extensionmanager`)
- `list_global_extensions()`, `set_extension_enabled(config_key, enabled)`, `add_extension(…)` — `set_extension_enabled` takes the global list's `configKey` (goose renamed the wire param from `name` and rejects it now)
- `sources_list/create/delete/update` (projects + skills) — `create` is `sourcesCreate(type, name, description, content, projectId?)` where `projectId` scopes a skill to a project (`global` if null; projects themselves are always global)
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

> **Raw-JSON listeners (deliberate, documented here).** The unstable shim's
> list families deliver their payloads to the UI **verbatim as JSON strings**
> on `GrouseUnstableListener`: `on_recipes`, `on_projects`, `on_skills`,
> `on_tools`, `on_extensions`, `on_session_extensions`, `on_supported_models`,
> `on_providers`, `on_config_value`, `on_export`, `on_session_probe`,
> `on_app_resource`, and `on_tool_result` (+ the error/compaction/message
> families). The fork wire shapes are intentionally NOT mirrored into typed
> uniffi records here — the shim is slated for retirement (GDK absorbs each
> feature), so the UI parses these JSON payloads directly and the shim is
> dropped without touching the stable contract. This is the documented
> exception to the "typed event per family" rule in §1.

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
parallel connections in browse mode; chat routes to whichever session was last
opened. A peer's connection is supervised: an unexpected drop re-dials with the
core's backoff curve and resumes the session that was open (a peer that was
never live gives up after the main connection's 6-try budget; one that dropped
after reaching ready retries far longer — see `RoamPeer::RECONNECT_MAX`).
Intents (`open_session`, `new_session`) also queue during the reconnect window
the same way they queue on the main connection. Surfaced as:

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
