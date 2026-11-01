# grouse-core internals (implementation contract)

The OUTER contract is `CONTRACT.md` (the uniffi surface). This file pins the
INTERNAL architecture: module layout, threading model, and the exact seams the
parallel slices build against. A slice must only depend on the seams below —
never reach into another slice's internals.

## Threading model

The core owns one tokio runtime on a worker thread (`lazy_static`/`OnceLock`
runtime + a spawned worker). All `#[uniffi::export]` intents are synchronous:
they enqueue onto the runtime and return. `connect()` is the exception — it
blocks until the connection is initialized or fails (bounded). All network I/O,
reply dispatch, and `CoreListener` callbacks run on the runtime thread.

The SDK's `Connection::send_request(...).block_task().await` gives synchronous
request/reply — there is NO global request-id table (the SDK tracks ids
internally). Notifications arrive via `on_receive_notification` and stream
during a pending `send_request`.

## Module layout

```
lib.rs         the uniffi surface: records/enums (DONE), Core + GrouseUnstable
               objects, listener fan-out. Owns the intents, status machine,
               reconnect orchestration, the resync cycle (both transports), the
               one prompt path (send_prompt → owner_for → per-session queue →
               on_turn_ended; turns are concurrent per session, keyed by display id),
               and Core's DispatchHost impl (main namespace).
transport.rs   WsTransport (DONE) — ConnectTo<Client>, X-Secret-Key + WebPKI-verifying TLS
               (accept_invalid_certs opt-out).
session.rs     THE session layer: transport-independent ACP semantics. Owns the
               ACP parameter builders (initialize_params/initialize_request,
               session_list_params/meta, session_new_params, session_load_params,
               prompt_params), the bounded RPC policy (rpc_timeout/request), the
               wire → record translators (content_block_text/chunk_text,
               tool_call_kind/tool_call_app/tool_live_output/tool_final_output,
               tool_call_status_str, permission_option_kind_str,
               config_option_value, parse_config_options, parse_sessions,
               summary_from_fields/summary_from_info), the id namespace
               (ROAM_PREFIX/display_id), the cache paint policy
               (PaintPlan/plan_paint/apply_paint), the ONE notification pipeline
               (DispatchHost/PrefixHost/dispatch/apply_tool_update), the ONE store
               registry + display gate (SessionHub/SessionGate), the ONE transmit
               surface (ChatEndpoint), and the ONE SDK client builder
               (ClientHandlers/connect_client). Nothing here knows which wire a
               session arrived on.
spine.rs       the main WebSocket connection: dial + framing (WsTransport), the
               initialize → new/resume handshake, reconnect bookkeeping, and the
               server-request answers. Owns the main id namespace (no prefix) and
               implements ChatEndpoint; its notifications go through
               session::dispatch. It does NOT translate wire payloads or keep a
               second copy of the accumulation rules.
transcript.rs  TranscriptStore: chunk accumulation → bubbles; emits on_item
               (the rich item stream, docs/TRANSCRIPT_MODEL.md), on_speak
               (speak.rs), and on_run_ended. Also owns the window cursor
               (oldest_id / has_older), the rich snapshot/rebuild, and the
               replay hold (hold_emissions/release_held_replay: a load's rows
               land silently, then ONE windowed paint). NO network.
cache.rs       CacheStore: per-session RICH item list + tools, freshness check,
               under the UI-supplied CacheDir. TRANSCRIPT_CACHE_VERSION gates
               the format; a pre-rich file reads absent. NO network.
unstable.rs    the GrouseUnstable impl (the `_goose/unstable/*` shim) — each is
               conn.rpc + a reply handler (re-list pattern). Owns its own file;
               if uniffi rejects cross-file impl blocks, export via a single
               #[uniffi::export] re-export and note it.
notify.rs      push envelope + show/don't-show policy, shared by every client
               (CONTRACT §5.5, docs/NOTIFICATIONS.md). Pure: no network, no OS.
speak.rs       SpeakStream: raw assistant markdown → sanitized, sentence-sized
               TTS chunks. Pure text, no uniffi exports, no network.
summarize.rs   model-written one-line labels for a completed run of tool calls
               (issue #13): prompt shaping is pure; Core creates the hidden
               summarizer session on the turn's own endpoint (main or peer) and
               session::dispatch intercepts its reply by display id.
capi.rs        the `grouse_*` C ABI the desktop dlopens: forwarder from the
               typed listener traits to the GrouseCoreListener C table, plus a
               JSON C-string for every record/enum. Mirrors the uniffi surface —
               a new intent/event is added here in the same change.
roam.rs        the peer registry: RoamPeer { label, transport, session list },
               browse mode, roam_connect/disconnect/open_session. Parallel
               iroh connections, each with its own dial/framing (RoamTransport),
               reconnect supervisor, and id namespace (roam:<label>:<raw>).
               Implements ChatEndpoint; notifications go through the SAME
               session::dispatch with a PrefixHost. Peer transcripts live in the
               shared SessionHub, keyed by display id, and are cached by the core
               under that same prefixed key.
```

## Pinned seams (the contract between slices)

1. **Session layer** (session.rs) — the ONE implementation of session semantics,
   shared by the spine and every roam peer. A slice that needs ACP behavior uses
   these; it must not grow a transport-specific copy.
   - Params: `initialize_params` / `initialize_request` (the same capability
     declarations, untyped and typed), `session_list_params` / `session_list_meta`,
     `session_new_params(client_id, cwd, recipe_id, project_id)`,
     `session_load_params(wire_id, cwd, replay_tail)`, `prompt_params(prompt, wire_id)`.
   - RPC policy: `rpc_timeout(method)` (bounded for every control call, `None` for
     `session/prompt` — the reply IS the turn's end) and `request(owner, method, params)`
     which applies it through a `ChatEndpoint`.
   - Translation: `content_block_text` / `chunk_text`, `tool_call_kind` (→
     `(ToolCallKind, detail)`), `tool_call_app`, `tool_live_output`,
     `tool_final_output`, `tool_call_status_str`, `permission_option_kind_str`,
     `config_option_value`, `parse_config_options`, `parse_sessions`,
     `summary_from_fields`, `summary_from_info`, `info_cwd`, `permission_request`,
     `permission_response`.
   - `ChatEndpoint` — the ONE transmit surface: `request`, `notify`,
     `wire_id(display)`, `accepts_prompt`, `label`, `session_cwd`,
     `reset_replay_gates`. Implemented by `spine::Conn` (main namespace) and
     `roam::RoamPeer` (prefix-stripped ids).
   - `DispatchHost` (+ `PrefixHost`) and `dispatch(host, notif)` — the ONE
     `session/update` pipeline; `apply_tool_update` is the one tool lifecycle
     rule. `Core` implements `DispatchHost`; a peer wraps it in `PrefixHost`.
   - `SessionHub` + `SessionGate` — ONE `TranscriptStore` per display session id,
     for every transport, with ONE display gate (see below).
   - `PaintPlan` / `plan_paint` / `apply_paint` — the ONE cache policy (fresh
     suppresses the replay, settled-but-stale merges a bounded `REPLAY_TAIL`,
     incomplete/forced paints provisionally).
   - `ClientHandlers` / `connect_client` — the ONE SDK client builder: typed
     `session/update`, untyped custom notifications, permission requests, and
     server requests. Both transports register the same table, so a custom
     notification or recipe/elicitation request cannot work on one wire and be
     dropped on the other.
   - Id namespace: `ROAM_PREFIX` (`roam:`) and `display_id(label, wire)`. The UI
     only ever sees display ids; wire ids exist only inside an endpoint.
2. **Conn** (spine.rs) exposes:
   - `Conn::rpc(&self, method: &str, params: Value) -> Result<Value, AcpError>`
     (synchronous, block_task) — the ONLY way slices talk to the main server.
   - `Conn::status() -> ConnectionStatus` and a status-change hook.
   - `register_notification_listener` / `register_server_request_listener` for the
     tags a slice owns; `session/update` itself goes to `session::dispatch`.
   - Transport/namespace concerns only: `WsTransport` dial, the handshake,
     `prebind_session`, `suppress_replay`/`merge_replay` gates, `is_ready`,
     `config_matches`, `shutdown`.
3. **TranscriptStore** (transcript.rs):
   - `TranscriptStore::new(listener: Box<dyn CoreListener>)`
   - `append_chunk(&self, role: &str, text: &str, message_id: Option<&str>, thought: bool)`
     (role ∈ user|agent|thought; a new message_id starts a new bubble)
   - `tool_call(&self, title, detail, tool_call_id, kind: ToolCallKind)` /
     `tool_update(&self, id, status, output, live)` / `tool_app(&self, id, uri, ext)`
   - `usage(&self, used, size, cost, currency)`, `run_ended(&self, stop_reason)`,
     `echo_prompt(&self, text)` (the local prompt echo + turn boundary)
   - `clear()`, `transcript() -> Vec<Message>`, `replace(&self, Vec<Message>)`
     (used by replay/load: clear + rebuild, emitting TranscriptEvent::Clear once)
   - Rich seam (docs/TRANSCRIPT_MODEL.md): `rich_transcript() -> Vec<Item>`,
     `item(id)`, `replace_rich(items, provisional)`, `replace_rich_for_merge(items)`,
     `end_merge()`, `load_older(count)`, `set_session(id)`,
     `window() -> TranscriptWindow`, `replay_thought(text)`. The store keeps the
     whole transcript but emits only `bubbles[emitted_from..]` (the
     `CLIENT_WINDOW` newest on a paint); `load_older` walks `emitted_from` back.
   - Reverse-loading seam: `prepend_rich(items) -> Option<usize>` (splice a page
     of OLDER rows in front, stamped below everything held), `server_more()` /
     `set_server_more(bool)` (the server's backlog keeps `has_older` true at the
     origin), `can_load_older()`, `len()`, `generation()` (a wholesale rebuild
     invalidates a page in flight), `publish_window()`, `emissions_held()`.
     A streamed text row is finalized with an authoritative Upsert when it ends
     (new bubble, tool call, or `run_ended`). A provisional drop emits Reset
     before the replay's Upserts; a merge replay truncates at its first
     overlapping id (`anchor_merge`) and repaints once at `end_merge`.
   - owns the toolgroup collapse (consecutive tool calls) and the live-output
     append/replace rule.
4. **CacheStore** (cache.rs):
   - `CacheStore::new(cache_dir: PathBuf)`
   - `load_transcript(session_id) -> Option<(Vec<Item>, String /*updatedAt*/, bool /*complete*/)>`
   - `save_transcript(session_id, Vec<Item>, updated_at, complete)` — `complete`
     is whether the file reaches the session's origin; a fresh-but-partial cache
     still paints and still pages.
   - `load_tools(session_id) -> Option<...>` / `save_tools(session_id, ...)`
   - freshness is the CALLER's job (compare updatedAt); the store is dumb I/O.
     The core's caller is `Core::save_cache_for` (incomplete store ⇒ empty
     stamp) and `session::plan_paint`.
5. **Roam** (roam.rs):
   - `RoamPeer::connect(secret, card, label, listener, host, hub, cache) -> Arc<RoamPeer>`
     — browse mode: initialize → session/list, no auto-open.
   - `RoamPeer::sessions() -> Vec<SessionSummary>`, `open_session(session_id, cwd)`,
     `open_session_with(session_id, cwd, force)`, `new_session(cwd)`, `relist()`,
     `close()`, `save_open_transcript()`.
   - `Core` owns `Vec<Arc<RoamPeer>>` + the active-peer label; routing follows
     CONTRACT §6 (`owner_for` / `peer_for_session` resolve a display id to the
     endpoint that owns it).

## Session hub and display gate (one store policy)

`SessionHub::store(key)` creates exactly ONE `TranscriptStore` per display
session id — the main connection's sessions and every peer's alike — and each
store's listener is a `SessionGate` bound to that key. The gate forwards
everything that is turn CONTROL (status, session list, run end, open failure,
touched, active run) and forwards painting (`on_item`, `on_usage`, `on_speak`)
ONLY while its key is the `displayed` one. `SessionHub::display(key)` moves the
gate and clears that key's green dot; a background store still accumulates and
is cached, and `SessionHub::mark_new(key)` sets the dot via
`on_session_touched` once per new content. `forget(key)` drops a store (deleted
session, closed peer).

`SessionHub::dispatch_store(key)` is the same store EXCEPT while a backfill page
is in flight for that key, when it returns the page's scratch store: the page's
rows arrive on the live notification stream, so turning them into items must not
touch the transcript on screen. Only the dispatch funnel (`DispatchHost::store`)
uses it; every other caller wants `store`.

Every transport supplies two things to the gate: `display_id` (identity for the
main wire, prefixed for a peer) and the wire itself. There is no second store
type and no transport-level "who is active" routing: the hub's `displayed` key
is the only gate.

## How to change a protocol detail

- ACP params, timeouts, payload translation, tool/chunk mapping, session-list
  parsing, or cache paint policy: change `session.rs` ONCE. Both transports pick
  it up; a change that needs a second copy means the seam is in the wrong place.
- A notification or server request family: register it in `session::ClientHandlers`
  / `session::dispatch` (the shared table). `spine.rs` and `roam.rs` must not grow
  their own handler.
- Transport-only work (dial, framing, reconnect budget, browse-mode list, id
  prefix): goes in `spine.rs` / `roam.rs`.

## Transcript synchronisation (both transports, one set of rules)

Every session — the main WebSocket's and every roam peer's — has FOUR sources
that describe the same rows: the live stream, a local echo of the prompt the
client sent, a `session/load` replay (full, or a *tail* when the cache is
provably current), and the cache itself. They all land in the session's one
`TranscriptStore` (via `session::dispatch` / `Core`), so these rules are
implemented once, in `transcript.rs`, and hold identically for both transports.
Only one thing keeps the sources from fighting: **the server's message id is the
row.**

1. **Identity.** goose stamps every chunk it sends with the stored message's id
   (`content_chunk_for_message`, and `ensure_message_event_id` guarantees every
   live message has one). The same message therefore carries the same id live
   and in every replay. `TranscriptStore::append_chunk` keys the bubble on
   `(role, message_id)`; a chunk either accumulates into its row or opens one.
   No text comparison, ever.
2. **A replay rewrites; it does not append.** A replayed chunk whose
   `message_id` differs from the open bubble's opens a new row rather than
   extending the live one, and a provisional cache paint is dropped wholesale
   (`supersede_provisional`, emitting `Reset`) at the first real replayed row.
   A *merge* paint (stale-but-settled cache) instead truncates the cache at the
   first replayed id already held (`anchor_merge`) and lets the replay
   re-supply that row and everything after, so a tail replay is idempotent, can
   complete a row whose live stream was cut short, and cannot double text. An
   item the replay does not mention keeps its content — that is what makes a
   *tail* replay safe (and `end_merge` reports a gap when the tail never
   overlapped).
3. **The prompt echo is a row, and a boundary.** goose sends no
   `user_message_chunk` for a prompt a client sent, so the core's one prompt path
   (`Core::spawn_prompt`) calls `TranscriptStore::echo_prompt` at send time — on
   the main wire and on a peer alike. It always opens a NEW row — a prompt is a
   turn boundary, which is also what stops the next reply from continuing the
   previous turn's paragraph. When the server later names that prompt, the user
   chunk adopts the echo row (`pending_echo`, oldest first, exact text) instead
   of appending a second copy of the question.
4. **No id, no guessing.** A chunk without an id falls back to one open bubble
   per role and appends plainly; the item stream gets a synthetic `@n` id so
   deltas and `Upsert`s still key to the same row. There is no honest way to
   tell a repeated word from a re-delivery without identity, and the old
   `contains`/`ends_with` guards silently swallowed legitimate repeats. A modern
   server never reaches this branch.

Anything that compares transcript TEXT to decide what to draw is a bug in this
model: it is what produced merged replies, questions duplicated at the end of
the transcript, and text lost to "dedupe".

Cache keys follow the display id, never a shared bare id: the main wire caches
under `<sessionId>`, a peer under `roam:<label>:<raw>`. The save policy is the
core's (`Core::save_cache_for`: an incomplete store is written with an EMPTY
`updatedAt` stamp; a peer reaches it through `DispatchHost::cache_session`), and
the paint policy is `session::plan_paint` for both.

## Event fan-out rules (who emits what)

- Transcript content → the session's `TranscriptStore` emits
  `CoreListener::on_item(...)` (`Upsert` / `AppendText` / `AppendOutput` /
  `Remove` / `Reset` / `Window`), forwarded by its `SessionGate` only while the
  session is displayed. Usage and turn end are the two non-item families:
  `on_usage` / `on_run_ended` (run end is turn CONTROL and always passes).
- `session/list` replies → `on_sessions(Vec<SessionSummary>)` (spine); a peer's
  list → `on_roam_sessions` (`RoamPeer::apply_sessions` / `emit_sessions`).
- `session_info_update` → `on_session_touched` (via `session::dispatch` →
  `DispatchHost::touched` → `Core::on_session_touched`) AND the resync cycle
  (debounce 1.5s → probe `_goose/unstable/session/info` on the OWNER of that
  display id → in-place `session/load` replay → re-probe at 8s ×3) — this lives
  in lib.rs and runs for peer sessions too (`Core::probe_and_maybe_resync` /
  `resync_current_session` route through `owner_for`).
- Permission/recipe-params/elicitation server requests → `CoreListener`
  events; the UI answers via `respond_*` intents (see CONTRACT §5). The peer
  registers the same handler table (`session::ClientHandlers`) and parks
  responders through the same `spine::park_server_request` seam.
- A FAILED open/resume → `on_session_open_failed(session_id, message)`
  (`lib.rs` `open_session_inner` for "no config"; `spine.rs`'s Resume arm and
  `live_load_session_async` for a rejected `session/load`, relayed through
  `Conn::set_on_open_failed`; `roam.rs`'s peer `load_session`). Nothing is
  substituted: a failed load leaves NO session bound and the gate naming
  nothing, so the store's paint stays provisional and `save_cache` cannot
  re-stamp it (grouse#19).

## Build & test

- `cargo check`/`cargo test` via `distrobox enter kde-build`.
- Tests: pure unit tests for stores/unstable (no server). For the spine, a
  minimal in-process scripted WS server (tokio-tungstenite). The desktop is no
  longer a reference implementation: it is a thin client over the C ABI
  (`src/capi.rs` + `clients/desktop/src/corebridge.cpp`), so the core is the
  only place wire logic lives.
