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
transcript.rs  TranscriptStore: chunk accumulation → Message bubbles; emits
               on_stream + on_transcript. NO network.
cache.rs       CacheStore: per-session transcript + tools, freshness check,
               under the UI-supplied CacheDir. NO network.
unstable.rs    the GrouseUnstable impl (the 35 shim methods) — each is
               conn.rpc + a reply handler (re-list pattern). Owns its own file;
               if uniffi rejects cross-file impl blocks, export via a single
               #[uniffi::export] re-export and note it.
roam.rs        the peer registry: RoamPeer { label, client, sessions },
               browse mode, roam_connect/disconnect/open_session. Parallel
               connections; Core's roam intents are thin wrappers here.
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
   - owns the toolgroup collapse (consecutive tool calls) and the live-output
     append/replace rule.
3. **CacheStore** (cache.rs):
   - `CacheStore::new(cache_dir: PathBuf)`
   - `load_transcript(session_id) -> Option<(Vec<Message>, String /*updatedAt*/)>`
   - `save_transcript(session_id, Vec<Message>, updated_at)`
   - `load_tools(session_id) -> Option<...>` / `save_tools(session_id, ...)`
   - freshness is the CALLER's job (compare updatedAt); the store is dumb I/O.
4. **Roam** (roam.rs):
   - `RoamPeer::connect(secret, card, label, listener) -> Arc<RoamPeer>` —
     browse mode: initialize → session/list, no auto-open.
   - `RoamPeer::sessions() -> Vec<SessionSummary>`, `open_session(session_id, cwd)`
   - `Core` owns `Vec<Arc<RoamPeer>>` + the active-peer label; routing follows
     CONTRACT §6 (chat routes to the last-opened session's owner).

## Event fan-out rules (who emits what)

- Stream chunks → TranscriptStore emits `CoreListener::on_stream(...)`; bubble
  structure changes also emit `on_transcript(...)`.
- `session/list` replies → `on_sessions(Vec<SessionSummary>)` (spine).
- `session_info_update` → `on_session_touched` (spine) AND the resync cycle
  (debounce 1.5s → probe `_goose/unstable/session/info` → in-place
  `session/load` replay → re-probe at 8s ×3) — this lives in lib.rs.
- Permission/recipe-params/elicitation server requests → `CoreListener`
  events; the UI answers via `respond_*` intents (see CONTRACT §5).

## Build & test

- `cargo check`/`cargo test` via `distrobox enter kde-build`.
- Tests: pure unit tests for stores/unstable (no server). For the spine, a
  minimal in-process scripted WS server (tokio-tungstenite) mirroring the
  desktop's tests/fakeserver.h — initialize/new/load/prompt handlers that
  stream chunks. The desktop reference (same logic, C++): the grouse-desktop
  repo's src/{acpclient,manager}.{h,cpp}.
