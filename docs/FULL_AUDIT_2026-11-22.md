# Full grouse audit — 2026-11-22

**Baseline:** `58aaef0a` (the bindless-connect fix), plus the uncommitted-free tree at audit time.
**Method:** twelve parallel **read-only** agents, each assigned a disjoint area of the tree; no
agent edited or built anything. Every high-severity claim below was then re-read against the code
by the coordinator. Legend: **✓** = coordinator read the cited code and confirms the finding;
**~** = agent finding, reported with a concrete anchor but not independently re-verified.

The audit was deliberately scoped to *current* code, not to the known backlog in `AGENTS.md`.
The two fixes shipped on 2026-11-22 (`PaintPlan::bind_only`, `Core::connect_attach`) were excluded
as findings; every agent was instead asked to check their call sites. Several agents found paths
those two fixes do **not** cover — those are the important results.

---

## 1. Shortlist — fix these first

| # | Severity | Finding | Anchor |
|---|---|---|---|
| 1 | **P0** | The **core's own reconnect** still resumes and rebinds a main session, so the bind/ = display bug we fixed on the client is still reachable from the transport. Worse, `connect_attach` wires now have **no core reconnect at all** because the ended hook only schedules one when `active_session_id()` is Some. | `lib.rs:3902-3996` ✓ |
| 2 | **P0** | A "fresh cache" suppression that fails verification is downgraded to a **merge after the paint**, so `end_merge()` returns false and the replay tail is **appended** — the newest turn appears twice. Cold `connect_resume` never verifies at all, so messages another client wrote while the app was closed are dropped. | `session.rs:2200-2213,2245-2250,2100-2104`; `spine.rs:715` ✓ |
| 3 | **P0** | A failed merge load returns before `end_merge()`, leaving the store armed so a later chunk can truncate it at a stale anchor; a cancelled merge-gap rebuild leaves an **empty** transcript (only the `Err` arm restores the paint, not `Drop`). | `session.rs:2386-2413,2329-2337` ✓ |
| 4 | **P0** | Android's inline notification **"Reply" sends to whatever chat is currently on screen**. The session id is only on the content `PendingIntent`, not the reply one. Wrong-conversation message delivery. | `ReplyReceiver.kt:19`; `Notifier.kt:70-72` ✓ |
| 5 | **P0** | Desktop opens a **new roam peer session with the raw remote id**; the core only knows `roam:<label>:<id>`, so prompts are rejected and the chat is unusable. | `manager.cpp:2933`; `roam.rs:1176`; Android does it correctly at `ConnectionManager.kt:3560` ✓ |
| 6 | **P0 (security)** | The chart WebView renders an exception string from a **server-controlled spec** into `innerHTML` with JavaScript enabled on a `file://` origin and no CSP. Any compromised goose can inject script into the app context. | `Bubbles.kt:373-406` ✓ |
| 7 | **P1** | A superseded connection's in-flight handshake keeps running: `shutdown()` sets `closing`, but the handshake only checks it *after* all awaits, so an old wire can still bind a session and emit `Ready`/config, flushing the new wire's queues and resetting reconnect state. | `spine.rs:442,679,777-781`; `lib.rs:3725-3728,5309` ✓ |
| 8 | **P1** | A deferred main-chat open parked while connecting survives a switch to a **roam** chat and is consumed in `onCoreReady` *before* the ownership guard — the same repaint-over-peer bug we just fixed, via a different path. | `ConnectionManager.kt:2819-2827,1654-1664` ✓ |

### Why #1 matters most

`on_connection_ended` calculates `resume` from the main connection's `active_session_id()` and
calls `schedule_reconnect` **only if some**. Two consequences:

- A wire established by `connect_attach` binds nothing, so an unexpected drop leaves the app
  permanently offline until a client-side retry happens. The core's documented 6-attempt budget
  is unreachable on exactly the new path we shipped.
- If a main chat was bound when the reader switched to a roam chat, a main-wire drop makes
  `on_reconnect_timer` build `ConnectSpec::Resume { session_id }` unconditionally; the handshake
  `bind_session`s it, and binding is displaying. This is `58aaef0a`'s bug on the internal path.

Fix shape: the ended hook should advance the backoff budget regardless, carry an explicit
`Attach` spec when no session is bound, and consult the displayed key's owner before resuming —
the same rule the client now applies.

---

## 2. Core state machine (`core/grouse-core/src/lib.rs`)

| Sev | Finding | Evidence / fix |
|---|---|---|
| **P1** ✓ | A remote edit arriving during our own turn is cached and stamped current, so its rows are never fetched. `on_touch_debounced` returns early while the chat is `in_flight` (`:5162`) but consumes that generation; `finish_turn` then `refresh_stamp(key,true)` (`:3895`) overwrites `session_updated_at`/`sync_stamp`, and the later move test sees no move. | Track "remote touch during this turn" per session; skip stamp persist/re-baseline and re-arm resync. |
| **P1** ✓ | A dropped MAIN wire removes main keys from `in_flight` and marks them `incomplete`, but never emits `run_ended` / empty active run or calls `finish_turn`; clients stay "working" forever. The roam path does all of this via `host.turn_lost`. | Route main-wire drops through the same `turn_lost` helper (`roam.rs:1334-1345`). |
| **P2** ~ | Resync's 8s ×3 follow-ups never run if the first probe finds no move: `resync_ticks = 4` is armed at `:5168`, but the follow-up timer is only spawned in the `moved` branch (`:5232`). | Schedule next probe whenever ticks remain. |
| **P2** ~ | `sync_stamp` is one global tuple; a probe for chat B clobbers chat A's resync baseline (`:3892`, `:5225`). | Key it by display id. |
| **P2** ✓ | Only the `ended_hook` is generation-bound; `status_hook`, `open_failed_hook`, `config_hook` are plain closures, so a stale wire can mutate current state (`:3725-3735`, `:5309`). | Capture `conn_gen` in every hook. |
| **P2** ~ | `roam_disconnect` strands that peer's turns: `in_flight`, `active_runs`, `incomplete` keep `roam:<label>:` keys, so paging stays blocked, prompts queue forever and reopens take bind-only. | Call `turn_lost` for those keys. |
| **P2** ~ | `peers` mutex is held across `peer.close()` and `RoamPeer::connect` (`:2037-2073`), which synchronously invoke listener callbacks / a multi-second dial; a re-entrant listener deadlocks and all routing blocks. | Clone under lock, drop, then act. |

**Verified sound:** the shipped `bind_only` call sites in this file (`open_session_inner` repaints
live rows and passes `paint.keep`; `refresh_session(Full)` skips cache invalidation for a live
turn); `connect_attach` binds nothing; per-session `in_flight`/`incomplete`/queues are not cleared
by other chats; cache saves stamp incomplete stores empty; no state guard is held across `.await`.

---

## 3. Session pipeline (`session.rs`) — paint, replay, cache policy

| Sev | Finding | Evidence / fix |
|---|---|---|
| **P0** ✓ | Suppression downgraded by `verified_ask` after `replace_rich` painted ⇒ no `merging` flag ⇒ `end_merge()` false ⇒ tail appends (duplicates). Cold resume bypasses verification entirely (`replay_session`). See shortlist #2. | Verify before paint, or re-arm `replace_rich_for_merge(painted)` on downgrade; add `verified_ask` to the Resume handshake. |
| **P0** ✓ | Failed merge leaves `merging` armed (`:2392` returns before `:2404`, and `ReplayGuard::drop` only ends replay/hold). Cancelled gap rebuild leaves an empty store (`:2405` clears, restore only inside `Err`). See shortlist #3. | Disarm on every exit; move `painted` into the guard and restore in `Drop`. |
| **P2** ~ | Roam `dev` sessions never reach the advisor: `DispatchHost::session_mode` has a no-op default and `PrefixHost` doesn't forward it (`:2540`), while roam summaries are emitted on the peer host. | Forward it like `session_stamp`. |
| **P2** ~ | `session_info_update` with a live run id during a suppressed replay calls `end_replay` mid-load (`:2933-2937`), clearing `suppressing` too, so the rest of the replay paints early/duplicated. | Only end on run start when `!replaying(key)`. |
| **P2** ~ | Roam opens use the MAIN cwd and refreshes use an empty one (`:lib.rs:2114-2122`, `roam.rs:1094-1100`); goose rewrites `working_dir` from it and can drop the provider. | Use `owner.session_cwd(key)`. |
| **P2** ~ | Shared-file tool calls skip the background green dot via the early return at `:2879-2881`; `SessionHub::forget` doesn't clear `page_targets` (`:3151-3156`). | Small, separate fixes. |

**Verified sound:** `plan_paint`'s settled/stale/incomplete/forced branches and `first_tail`;
`ask_with_stamp` probe-failure conservatism; bind-only's early return and 1-message ask;
`replay_skipped` backlog arithmetic; `SessionHub`/`SessionGate` display gating and `has_new`;
`ChatEndpoint` main/peer id stripping and timeouts; ACP param spellings; `connect_client`'s shared
handler table; duplicate message/tool ids keyed by id in the store.

---

## 4. Main wire (`spine.rs`, `transport.rs`)

| Sev | Finding | Evidence / fix |
|---|---|---|
| **P0** ✓ | Ended hook gates reconnection on a bound session — attach wires and failed handshakes never retry (shortlist #1). | `lib.rs:3902-3996`. |
| **P1** ✓ | Superseded handshake can bind/repaint/Ready after shutdown (shortlist #7). | Check `closing` after every await and race awaits against shutdown. |
| **P1** ~ | `Conn::rpc`/`rpc_async` (`:544,567`) call `block_task().await` **with no timeout**; the shared `session::request` policy is bypassed by unstable calls and `resolve_cwd`. A hung server parks the calling thread indefinitely. | Route all RPC through `session::request`. |
| **P2** ~ | Handshake RPCs have no timeout; `wait_ready` discards both timeout and error (`lib.rs:3759-3766`), so a server that answers pings but not `initialize` leaves "Connecting" forever. | Timeout handshake RPCs; emit `Error` + `signal_ready(Err)`. |
| **P2** ~ | A second recipe/elicitation request overwrites the single global `PARKED` responder (`:1049`): the first is never answered and the server waits forever. | Error the previous responder or key by request id. |
| **P2** ~ | Shutdown during a handshake leaves `connect()` blocked the full 15 s, or later told success for a dead wire (`:612`, `lib.rs:3765`). | Signal a cancelled ready from `shutdown()`. |
| **P2** ~ | Permission prompts are parked per-connection, but `Core::respond_permission` routes only by `active_peer()` and discards the result (`:967`, `lib.rs:2214`); with a roam chat displayed, the main wire's prompt cannot be answered. | One responder registry keyed by `tool_call_id` with owner. |

**Verified sound:** `ConnectSpec` New/Resume/Attach bind semantics; `X-Secret-Key` and
authority-only `Host`; WebPKI/CA/trust-all TLS selection; keepalive ping/pong, serialized writes,
bounded inbound queue; 500ms·2ⁿ cap-15s math; config-option parsing.

---

## 5. Roam (`roam.rs`, `grouse-roam-core`)

| Sev | Finding | Evidence / fix |
|---|---|---|
| **P1** ✓ | A peer's relist **replaces** `session_cwds` with only server-listed rows (`:1290`); goose omits empty sessions, so the cwd of a just-created chat is wiped. Later loads get `""`, which goose rewrites `working_dir` from; hidden summarizer/file sessions lose it too. | Merge new entries instead of replacing. |
| **P1** ✓ | Emitted roam sessions always carry `mode: None` (`:621`) even though the stored rows have the effective mode (`:1235-1243`) — dev chats lose their badge on the desktop. | `mode: s.mode.clone()`. |
| **P1** ~ | A transient `session/new`/open error calls `fail()` (`:1348`), which only sets `Error` (`:1355`); the command loop keeps running, the link stays up, the supervisor closes normally and **never redials**. One failure permanently kills a live peer. | Treat command failures as advisory or break the loop. |
| **P2** ~ | Deleting/archiving the open roam chat resurrects it as a ghost row (`:1254-1277`) and a later drop queues it as `resume_pending`. | Clear `open_session_id` on delete/archive. |
| **P2** ~ | A peer label containing `:` breaks routing: prefixes use the full label but `peer_for_session` takes `split(':').next()`, so the id can fall through to the **main wire** with a `roam:` prefix and silently no-op. | Longest-prefix match over known peer labels, or reject `:` in labels. |
| **P2** ~ | Only the first `session/list` has a 15 s timeout (`:918-928`); the mode-walk pages (`:1610`) and Relist walk have none, so a stalled peer hangs the handshake/command loop. | Bound every page. |
| **P2** ~ | `connection_ended` never clears `pending_permission` (`:1442-1445`); stale responders accumulate across reconnects. | Clear on link loss. |

**Verified sound:** shipped bind-only call site (`turn_in_flight` → `repaint_window` →
`load_with_plan` with tail 1, no clear); one endpoint per identity; blocking I/O on
`spawn_blocking`; cancel+shutdown on close; ready-reset retry budget; session-bound RPCs routed
to the peer; custom notifications prefixed before the display gate. The known 1 MiB `RoamCodec`
cap remains an open item, with no new impact found (downloads chunk at ~683 KiB base64).

---

## 6. Transcript & cache (`transcript.rs`, `cache.rs`)

| Sev | Finding | Evidence / fix |
|---|---|---|
| **P2** ✓ | `call_to_row` rebuilds a ToolGroup's children with `steps: Vec::new()` and `seq: 0` (`:858-859`), so grouped delegate subagent steps vanish on every cache/paint round-trip. The existing test only covers the live path. | Copy `steps` and `seq`. |
| **P2** ✓ | `bubble_from_item` hardcodes `created: 0` (`:896`) although `Item.created` is serialized and `timeline_chat` uses it as the only clock; cache-painted rows sort last in a merged timeline. | `created: it.created`. |
| **P2** ~ | Message-id gating fails when the first chunk has no id: `fresh` only compares ids when `stream_msg_id` is already set, and the accumulation branch never adopts an arriving id (`:1187-1191,1245-1286`) — distinct messages merge into one bubble. | Adopt the first non-empty id. |
| **P2** ~ | Identical repaint drops the backlog cursor: `st.has_older = st.emitted_from > 0` (`:2447`) ignores `server_more` used everywhere else. | `|| st.server_more`, emit Window on change. |
| **P2** ~ | Lenient cache load drops malformed rows but keeps the fresh stamp (`:253-260`), so suppression prevents repair. | Empty stamp when any row skipped. |
| **P2** ~ | Empty save leaves a stale transcript file (`:178-180`), contradicting its doc. | Remove the file. |

**Verified sound:** atomic writes (temp + fsync + rename + dir fsync); version gates; empty stamp
= unsettled; `CLIENT_WINDOW`/SEQ stamping; `has_older` on page splice; generation-based page
discard; file rows are references only; Reset/Upsert/Append*/Window order; POSIX path escaping.

---

## 7. Unstable shim & shared modules

| Sev | Finding | Evidence / fix |
|---|---|---|
| **P1** ✓ | `config_read` reads the response's `key` (`:413-416`), but goose's `ConfigReadResponse` has only `value`; `on_config_value("")` never matches a client key, and numeric/bool values are dropped by `as_str().unwrap_or_default()`. | Keep the requested key; stringify non-null values. |
| **P1** ✓ | Direct `tools_call` omits the **required** `extensionName` (`:613-614`); every call is rejected as invalid params. Also reports `isError` results as success (`:624`). This is the already-known "dead tools_call" path, now with a cause. | Derive/send extension name; propagate `isError`. |
| **P2** ~ | Malformed extension JSON bare-returns from `session_extensions_add`/`add_extension` (`:233,300`), leaving the UI waiting for the re-list that never comes. | Emit `on_error`. |
| **P2** ~ | `speak.rs`: a hidden chat's `SpeakStream` retains pending prose and `on_speak` has no session id, so switching back mid-turn can speak stale sentences. | Tag speak chunks with a session key. |
| **P2** ~ | `notify.rs:122-128`: session-only 120 s dedupe suppresses a genuine second turn in the same session. | Dedupe on run id, not only session. |
| **P2** ~ | Recipe/elicitation server requests carry no session/peer key and can surface over a different chat. | Key by owner. |

**Verified sound:** `surface.rs` compile guards match all guarded records/6 ops/9 item kinds and
all 23 listener callbacks; param spellings checked against goose SDK types; session-bound RPCs
route via `peer_for`; roam custom notifications are prefixed before the display gate; push policy
and summarizer are pure with edge-case tests; with no bound session, custom notifications are
dropped and steer reports "not ready" instead of painting another chat.

---

## 8. C ABI & desktop bridge (`capi.rs`, `corebridge.cpp`)

| Sev | Finding | Evidence / fix |
|---|---|---|
| **P1** ✓ | `corebridge.cpp:464-467` frees `err` and then dereferences it in `qWarning` — use-after-free on the core-create failure path. | Copy to `QString` first. (Found independently by two agents.) |
| **P2** ~ | `marshal` reads raw `m_target` on the core thread with no clearing hook; a callback arriving during shutdown can queue against a dead `Manager`. | `clearTarget()` from `~Manager`, QPointer-based read. |
| **P2** ~ | `grouse_push_parse`/`grouse_push_decide` (`:110-137`) are the only non-trivial exports without `catch_unwind`, and receive untrusted input. | Wrap. |
| **P2** ~ | Embedded NUL in server text makes `CString::new` return NULL, and `cb_str*` then **skips the callback entirely** (`:66-70,286-347`). | Sanitize or pass length+bytes. |
| **P2** ~ | Gate checks only 19 of 98 symbols; a stale `.so` reporting ABI 16 can crash on first use of an unchecked symbol. | Validate all, bump ABI on every export addition. |
| **P2** ~ | `grouse_tasks_paint` is called on the Qt main thread (`manager.cpp:2995`) though it does a disk read + callback. | Dispatch via `runOnCore`. |
| **P2** ~ | No `grouse_connect_attach`/`grouse_connected` in the C ABI, so desktop can't use the bindless connect. **Important nuance:** the desktop's `grouse_connect` uses `ConnectSpec::New{display:false}`, which does *not* repaint a displayed chat, but it mints a throwaway session and steals `active_session_id()`. Two agents disagreed on the repaint claim; the repaint part is **not** confirmed. | Export attach + connected (ABI 17) and use for wire-only connects. |

**Verified sound:** ABI 16 matches both sides; all 98 exports resolve; Rust/C++ listener tables have
the same 47 fields in order; callback strings are copied before free; queued `invokeMethod` keeps UI
off the core thread; all intents use `catch_unwind` except the push pair; uniffi bindings contain
`connectAttach`/`connected`.

---

## 9. Android controller (`ConnectionManager.kt` and friends)

| Sev | Finding | Evidence / fix |
|---|---|---|
| **P0** ✓ | Notification inline Reply has no session id in its `PendingIntent`; `sendWhenReady` targets the current chat or an orphan rebound to whatever is active. | Add `EXTRA_SESSION_ID`, route through that chat, bind it first if needed. |
| **P1** ✓ | Deferred main open survives a switch to roam and is consumed before the ownership guard (shortlist #8). | Clear on roam `openSession`; gate on `resumeTargetFor(current) == deferred`. |
| **P1** ✓ | `releaseTurnForLostWire` clears busy but leaves the id in `inFlight`; switching away and back re-derives `turnInFlight` and sticks forever (`:2717-2724`, `:249-253`). | Remove from `inFlight`/`activeRunIds`. |
| **P1** ✓ | Main-wire disconnect releases only `currentSession`, not the chat that actually owns the turn; roam error path has the same shape (`:2765-2780`, `:3530`). | Use `turnInFlightSession ?: currentSession`. |
| **P1** ✓ | `resourcesRead` argument order reversed at `:1228` (`extension, uri` instead of `uri, extension`); the callback key never matches, so pinned MCP Apps wedge on "Loading template…". The other call at `:3305` is correct. | Swap to match `:3305`. |
| **P2** ✓ | A parked Assistant open is dropped only when the route is neither `chat` nor `landing`; roam chats use the same `chat` route, so it can hijack a peer screen (`:1915`). | Also drop when `roamPeer(currentSession.value) != null`. |
| **P2** ~ | `onCoreItem` applies every op with no owner check while the core's display gate still points at the old chat; a streaming reply from the previous chat can paint under the new chat's id (`:3040-3137`). | Drop/stage ops from non-displayed owners. |
| **P2** ~ | `fileDownloads` keyed by server path only; two peers sharing a path show one another's download state (`:2990-2998`). | Key by owner + path. |

**Verified sound:** attach-only bindless launch; `resumeTargetFor` rejects roam ids; reconnect uses
attach when no main chat is shown; Ready's no-session/launchQuiet/ownership guards; per-chat queue
keying and normal-path turn mirrors; roam disconnect clears only the owning peer; TLS config and
reference-only file fetching.

---

## 10. Android UI

| Sev | Finding | Evidence / fix |
|---|---|---|
| **P0 (security)** ✓ | Chart spec error is interpolated into `document.body.innerHTML` with JS on, no CSP, `file://` origin (shortlist #6). | `textContent` + `about:blank` under CSP, block navigation. |
| **P1** ✓ | `onCreate` calls `handleEntry(intent)` on **every** recreation, so rotation replays ACTION_NEW_CHAT / OPEN_SESSION / SEND (`:64-67,82-98`). | Only when `savedInstanceState == null`, or consume in the manager. |
| **P1** ~ | Permission and elicitation sheets are hosted only in `ChatScreen`; a request arriving on landing/My Day/Settings parks with no UI, and the notification has no route to it. | Host sheets in `AppRoot` above the NavHost. |
| **P2** ~ | LazyList state isn't saved and the open effect always `scrollToItem(0)`, so rotation snaps a reader out of history (`:452,688-690`). | Save first-visible index + offset per chat. |
| **P2** ~ | Elicitation form state uses plain `remember`; required numbers aren't validated, so "abc" is submitted with its key absent (`:1962-2019`). | `rememberSaveable` + `toDoubleOrNull`. |
| **P2** ~ | Move-to-project dialog puts every project in one unbounded Column in the dialog text slot; with many projects, Cancel is clipped/unreachable (`:532-551`). | Weighted LazyColumn + real dialog button. |
| **P2** ~ | Streaming fence scan toggles only on `` ``` `` while `splitFencedCode` also handles `~~~` at column 0; blank lines inside tilde fences settle early (`:429` vs `CodeBlock.kt:325`). | Share one fence matcher. |
| **P2** ~ | Host/key/workingDir and skill-edit forms use plain `remember`; a rotation wipes unsaved input (`:109-133`, `CatalogScreens.kt:260`). | `rememberSaveable`. |

**Verified sound:** MCP App iframe is sandboxed `allow-scripts` srcdoc under a host CSP; permission
dismiss maps to Cancelled; reverse-layout index 0 is the bottom; finger-settle follow and held-stream
gating are coherent; roam id→peer drives the pill vs main status mark; drawer search scoping; fresh
chat counter effect ordering.

---

## 11. Desktop client (C++/QML)

| Sev | Finding | Evidence / fix |
|---|---|---|
| **P0** ✓ | Raw peer id on new roam session (shortlist #5). | Prefix with `roam:<label>:` via `openSession`. |
| **P1** ✓ | Main-wire `Ready` unconditionally clears `m_inFlight`, `m_activeRunIds`, `m_prompting` (`:2599-2604`), while Disconnected/Error already guard peer-owned turns — the exact bug fixed on Android today. | Guard with `m_activePeerLabel.isEmpty()`. |
| **P1** ✓ | `openSession` calls `grouse_load_cached_transcript` immediately after `grouse_open_session` (`:1021-1027`). Since the latter has already painted the live store (including bind-only), the cached `replace_rich(items,false)` overwrites it with stale disk rows. | Drop the second call, or make the ABI respect in-flight/bind-only. |
| **P1** ~ | AppBridge tokens are sequential `t1,t2,…`; token alone authorises, no Host/Origin check, JSON parsed regardless of Content-Type (`:4217`, `appbridge.cpp:204-241`) — any local page can inject a prompt. | Random 128-bit token + loopback Host/Origin + content type. |
| **P2** ~ | `serverConfigJson` hardcodes `accept_invalid_certs = true` (`:533`); no user-facing verify toggle. | Expose it, default false. |
| **P2** ~ | Wire-only attach unavailable to the desktop (see §8). | Add ABI symbol. |
| **P2** ✓ | core-create use-after-free (see §8). | Copy before free. |

**Verified sound:** ABI gate; listener table; `runOnCore` ordering; Reset→buffer→Window flush and
3 s safety valve; `MessageListModel` incremental upsert/prepend/deferred `dataChanged`; distance-
from-end scroll memory and re-asserted pin; `SessionListModel` no-op reset guard; markdown/code HTML
escaping and tests; native Canvas charts; sandboxed WebEngine profile; streamed HTML is plain text
until finalize.

---

## 12. macOS & parity ratchet

Agent 15 found macOS is **not** a placeholder: six Swift sources implement connect, sidebar,
session open, incremental item transcript, composer, permission sheet and advisor (13 of 23
listener callbacks). Compilability is unverified — `.github/workflows/macos.yml` only generates
bindings and checks defaults, never runs `swiftc`.

| Sev | Finding | Evidence / fix |
|---|---|---|
| **P1** ✓ | A benign advisory `Alert` is mapped to `"alert: …"` then `handleStatus` sets `online = false` (`:181-184`), and `App.swift:33-36` swaps the whole chat for `ConnectView`. Core sends Alert for routine control-path failures and documents "text only". | Keep online/connected unchanged for `alert:`. |
| **P1** ✓ | A failed chat open destroys the current transcript: `chat.reset()` + id switch happen before the load; core emits `on_session_open_failed` and binds nothing, then macOS clears the id → empty screen. | Defer reset/switch until load succeeds. |
| **P2** ~ | Local turn flag can wedge: `prompting=true` cleared only by `on_run_ended` matching `currentSessionId`; background run ends and dropped ends leave Stop enabled and queues stuck. | Reconcile with `core.turnInFlight`. |
| **P2** ~ | `hasOlder` stored but no view calls `loadOlder`; older history is unreachable. | Add a Load older action. |
| **P2** ✓ | The real macOS client is **outside** the parity ratchet: `check-parity.py:56-60` has no `"macos"` suffix mapping, so the 6 Swift files have no ledger column despite `PARITY.md:12,34-36` claiming every source-bearing client is checked. | Add `"macos": (".swift",)` plus a column, or document an explicit exemption. |
| **P2** ~ | Listener guard compares **names**, not signatures (`check-parity.py:68-79,94-97`); a signature change passes CI. | Compare normalized signatures. |
| **P2** ~ | `surface.rs` compile-guards only 5 records; `SessionSummary`, `ConfigOption`, `Permission*` etc. can gain fields with no disposition. | Add more `record_surface!` groups. |
| **P2** ~ | Evidence anchors are forgeable (`../` allowed, substring match anywhere, comments count). | Containment check + skip comments. |

---

## 13. Tests, docs & CI

| Sev | Finding | Evidence / fix |
|---|---|---|
| **P1** ✓ | None of today's fixes has a client-side regression test: `attachOnly`/`clearTurnState` and the streaming-hold latch are untested (Android tests are framework-free parsers); the core's `bind_only` test calls the helper with hardcoded flags and does not exercise production call-site selection or the main-wire handshake. | Extract pure predicates; add a spine test with `hold_prompts` + reconnect asserting `replayTail: 1`. |
| **P1** ✓ | Android JUnit and desktop ctest are dev-only: `android.yml:87` runs only `assembleDebug`; flatpak builds with `-DBUILD_TESTING=OFF`. None of the above tests gate a push. | Add both jobs. |
| **P2** ✓ | `PARITY.md:162` claims a desktop `ui.chat-stream-hold` anchored to `pinToEnd`, which just calls `positionViewAtEnd()` — no frozen row exists. | Change to `excluded:` or implement. |
| **P2** ~ | `AGENTS.md:651` calls `tst_manager` an intent→ABI proof; it is a no-crash smoke test that passes with no core loaded, and the `setInstanceForTesting` seam is unused. | Install a stub and assert captured calls, or correct the doc. |
| **P2** ~ | Roam reconnect/resume has no end-to-end test; only constants and state recording. The only real-peer test is `#[ignore]`'d. | Scripted peer test dropping a link. |
| **P2** ✓ | `docs/TRANSCRIPT_MODEL.md:550-551` names `tst_itemmodel` and `ItemStoreTest`, neither of which exists. | Cite real coverage or delete. |
| **P2** ~ | Doc drift: `CACHE_OPEN_AUDIT.md:24`/`INTERNAL.md:119` still describe three paint modes and "300 unit + 23 spine"; `AGENTS.md:448,597` says "no `rust-toolchain.toml`", but `core/rust-toolchain.toml` exists. | One doc sweep. |
| **P2** ~ | `task_action` (mutation → refresh / Alert on failure) has no test. | Add a task-state transition test. |

**Well covered:** spine fake-server wire shape (`X-Secret-Key`, messageId chunks, bounded tail +
`replaySkipped`, no live prompt echo, paged lists, failure/reconnect replays); cache stamp/merge/
provisional tests; rich item op sequences; My Day parsers; roam codec/list union/gating tests;
desktop QML suites and `itemStreamDrivesTheModel`; core CI test + clippy.

---

## 14. Cross-cutting themes

1. **Bind == display is still not universal.** We fixed the client launch/reconnect and the peer
   load, but the core's own reconnect (§2/#1) and the desktop's cached-transcript call (§11)
   still bind/paint a session the reader isn't looking at. This is the single highest-value
   invariant to enforce centrally.
2. **Paint-then-verify ordering is unsafe.** Three of the worst core bugs (§3) come from
   painting a cache before verification decides whether that paint was correct. Verification
   belongs before the paint, or the paint must be re-armable as a merge.
3. **Termination paths are where state rots.** Failed/cancelled loads, dropped wires, peer
   errors and explicit disconnects repeatedly leave `merging`, `in_flight`, responders, turn
   flags and cwd maps in a state the happy path never produces. `Drop`-based cleanup and
   owner-scoped registries would remove a whole class.
4. **Trust boundary is only enforced on some renderers.** Desktop escapes markdown and sandboxes
   WebEngine; Android's chart WebView writes server-controlled text into `innerHTML`.
5. **The ratchet doesn't cover the clients that are actually growing.** macOS is real code with
   no ledger column, no compile job, and no signature check; new UI behaviour is recorded only
   as free-form `ui.*` rows that CI cannot verify.
6. **Today's fixes shipped with no regression tests**, and neither client's test suite runs in CI.

---

## 15. Suggested sequencing

1. **Core P0 cluster** (§2 #1, §3 #1-#3, §4 #1/#2): reconnect spec + generation guards, and
   paint/verify/merge ordering. These interact — fix them as one PR with spine tests.
2. **Client misdelivery cluster** (Android Reply routing, desktop raw roam id, desktop Ready
   guard, Android deferred open, macOS Alert): all small, all user-visible, all independent.
3. **Security** (chart WebView, AppBridge token).
4. **Roam lifecycle** (cwd merge, mode, terminal error, ghost rows, label with `:`).
5. **Stimulus/response plumbing** (config_read, tools_call, pinned apps, permission routing).
6. **Ratchet + CI** (macOS client column, signature comparison, Android/desktop test jobs) —
   do this *before* the next large feature so the fixes stay fixed.
7. Doc sweep last, once the behaviour is final.

No code was changed as part of this audit.
