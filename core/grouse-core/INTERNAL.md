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
               reconnect orchestration, and the resync cycle.
transport.rs   WsTransport (DONE) — ConnectTo<Client>, X-Secret-Key + WebPKI-verifying TLS
               (accept_invalid_certs opt-out).
spine.rs       the live connection: owns the SDK Client + WsTransport, the
               initialize → new/resume handshake, the notification dispatch
               (SessionNotification → seams below), and server-request answers.
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
               (issue #13): prompt shaping is pure; the hidden summarizer
               session + reply routing live on Core/Conn.
capi.rs        the `grouse_*` C ABI the desktop dlopens: forwarder from the
               typed listener traits to the GrouseCoreListener C table, plus a
               JSON C-string for every record/enum. Mirrors the uniffi surface —
               a new intent/event is added here in the same change.
roam.rs        the peer registry: RoamPeer { label, client, sessions },
               browse mode, roam_connect/disconnect/open_session. Parallel
               connections; Core's roam intents are thin wrappers here. It holds
               a rich `PeerStore` (items) — transcript, staging and cache — and
               emits `on_item` directly (docs/TRANSCRIPT_MODEL.md; no bridge),
               with its own window + `load_older`. `transcript()` flat-projects
               the store for the legacy getter.
```

## Pinned seams (the contract between slices)

1. **Conn** (spine.rs) exposes:
   - `Conn::rpc(&self, method: &str, params: Value) -> Result<Value, AcpError>`
     (synchronous, block_task) — the ONLY way slices talk to the server.
   - `Conn::status() -> ConnectionStatus` and a status-change hook.
   - `Conn::on_notification(fn(SessionNotification))` registration (spine calls
     the stores itself; slices register only for tags they own).
2. **TranscriptStore** (transcript.rs):
   - `TranscriptStore::new(listener: Box<dyn CoreListener>)`
   - `append_chunk(&self, role: &str, text: &str, message_id: Option<&str>, thought: bool)`
     (role ∈ user|agent|thought; a new message_id starts a new bubble)
   - `tool_call(&self, title, detail, tool_call_id, kind: ToolCallKind)` /
     `tool_update(&self, id, status, output, live)` — bubbles + on_stream
   - `usage(&self, used, size, cost, currency)`, `run_ended(&self, stop_reason)`
   - `clear()`, `transcript() -> Vec<Message>`, `replace(&self, Vec<Message>)`
     (used by replay/load: clear + rebuild, emitting TranscriptEvent::Clear once)
   - Rich seam (docs/TRANSCRIPT_MODEL.md): `rich_transcript() -> Vec<Item>`,
     `item(id)`, `replace_rich(items, provisional)`, `load_older(count)`,
     `set_session(id)`, `window() -> TranscriptWindow`. The store keeps the whole
     transcript but emits only `bubbles[emitted_from..]` (the `CLIENT_WINDOW`
     newest on a paint); `load_older` walks `emitted_from` back. A streamed text
     row is finalized with an authoritative Upsert when it ends (new bubble,
     tool call, or `run_ended`). A provisional drop emits Reset before the
     replay's Upserts.
   - owns the toolgroup collapse (consecutive tool calls) and the live-output
     append/replace rule.
3. **CacheStore** (cache.rs):
   - `CacheStore::new(cache_dir: PathBuf)`
   - `load_transcript(session_id) -> Option<(Vec<Item>, String /*updatedAt*/)>`
   - `save_transcript(session_id, Vec<Item>, updated_at)`
   - `load_tools(session_id) -> Option<...>` / `save_tools(session_id, ...)`
   - freshness is the CALLER's job (compare updatedAt); the store is dumb I/O.
4. **Roam** (roam.rs):
   - `RoamPeer::connect(secret, card, label, listener) -> Arc<RoamPeer>` —
     browse mode: initialize → session/list, no auto-open.
   - `RoamPeer::sessions() -> Vec<SessionSummary>`, `open_session(session_id, cwd)`
   - `Core` owns `Vec<Arc<RoamPeer>>` + the active-peer label; routing follows
     CONTRACT §6 (chat routes to the last-opened session's owner).

## Roam transcript synchronisation (the rules, so they stop being rediscovered)

A peer session has FOUR sources that all describe the same rows: the live
stream, a local echo of the prompt the client sent, a `session/load` replay
(full, or a *tail* when the cache is provably current), and the cache itself.
Only one thing keeps them from fighting: **the server's message id is the row.**

1. **Identity.** goose stamps every chunk it sends with the stored message's id
   (`content_chunk_for_message`, and `ensure_message_event_id` guarantees every
   live message has one). The same message therefore carries the same id live
   and in every replay. `PeerStore::msg_idx` maps id → row; a chunk either finds
   its row or opens one. No text comparison, ever.
2. **A replay rewrites; it does not append.** The first replayed chunk of a
   message starts that row over (`rewritten` tracks which ids this replay has
   touched, cleared when a replay begins); the rest append. So a tail replay
   that re-delivers rows the cache already holds is idempotent, a replay can
   complete a row whose live stream was cut short, and re-delivered text cannot
   double. An item the replay does not mention keeps its content — that is what
   makes a *tail* replay safe.
3. **The prompt echo is a row, and a boundary.** goose sends no
   `user_message_chunk` for a prompt a client sent (`spawn_prompt` echoes it on
   the spine for exactly this reason), so `RoamPeer::echo_prompt` writes the
   question into the peer's store at send time. It always opens a NEW row — a
   prompt is a turn boundary, which is also what stops the next reply from
   continuing the previous turn's paragraph. When the server later names that
   prompt, the user chunk adopts the echo row (`pending_echo`, oldest first,
   exact text) instead of appending a second copy of the question.
4. **No id, no guessing.** A chunk without an id falls back to one open bubble
   per role and appends plainly. There is no honest way to tell a repeated word
   from a re-delivery without identity, and the old `contains`/`ends_with`
   guards silently swallowed legitimate repeats. A modern server never reaches
   this branch.

Anything that compares transcript TEXT to decide what to draw is a bug in this
model: it is what produced merged replies, questions duplicated at the end of
the transcript, and text lost to "dedupe".

## Event fan-out rules (who emits what)

- Transcript content → TranscriptStore emits `CoreListener::on_item(...)`
  (`Upsert` / `AppendText` / `AppendOutput` / `Remove` / `Reset` / `Window`).
  Usage and turn end are the two non-item families: `on_usage` /
  `on_run_ended`.
- `session/list` replies → `on_sessions(Vec<SessionSummary>)` (spine).
- `session_info_update` → `on_session_touched` (spine) AND the resync cycle
  (debounce 1.5s → probe `_goose/unstable/session/info` → in-place
  `session/load` replay → re-probe at 8s ×3) — this lives in lib.rs.
- Permission/recipe-params/elicitation server requests → `CoreListener`
  events; the UI answers via `respond_*` intents (see CONTRACT §5).
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
