# Comprehensive quality audit — 2026-11-22

**Baseline:** `dbf1eae4` (after the bug sweep and the Bundle-saver crash fix).
**Method:** eighteen parallel **read-only** agents with disjoint areas, each told to read
`docs/FULL_AUDIT_2026-11-22.md` first and NOT re-report its (already fixed) findings. This audit
was about **structure**, not defects: DRY, security posture, responsibilities that belong in the
Rust core vs the three clients, sloppy practices, maintainability, god files, layering, docs and
tests as maintenance systems. No agent edited, built or committed anything.

Legend: **✓** = coordinator re-read the code and confirms; **~** = agent finding with a concrete
anchor, not independently re-verified. Severity here means *leverage*, not "the app is broken".

---

## 1. Shortlist — highest leverage

| Sev | Finding | Anchor |
|---|---|---|
| **P0 security** ✓ | **Server-controlled `sha256` escapes the cache directory.** `local_file_path` uses `sha256[..16]` verbatim as a path component; a malicious `attachments__attachment_share` result can send `../../../../tmp/AAAAAAAAA` and have the downloaded bytes written outside the cache. A short multibyte digest can also panic on `sha256[..16]`. Digests are never verified against the downloaded bytes. | `lib.rs:3288-3302`, `3270-3279` |
| **P0 security** ✓ | **File uploads always use the MAIN wire.** `send_prompt_with_files` has no owner parameter; `upload_one` → `attachment_call` → `tasks_wire` is hardcoded to `self.inner.conn`. A file attached in a roam chat is uploaded to the main goose server and the main `file://` path is sent to the peer — wrong server, failed delivery. | `lib.rs:1909-1935`, `3164-3167`, `3309+` |
| **P0 security** ✓ | **The Android biometric lock is cosmetic.** `unlocked` gates only Compose rendering; `connectHome()` opens the socket with the stored key and notification `Reply` can enqueue a prompt while the lock screen is up. SecureStore's KDoc promises it gates key reuse. | `MainActivity.kt:125-165`, `ReplyReceiver.kt:23`, `SecureStore.kt:118-126` |
| **P1** ✓ | **The desktop still defaults to trusting any certificate.** The toggle exists but `DEFAULT_ACCEPT_INVALID_CERTS = true`, so the X-Secret-Key can be handed to an on-path attacker. Android defaults to verify. *(This was one of the two decisions left with you.)* | `manager.cpp:334`, `SettingsDialog.qml:24` |
| **P1** ✓ | **Every client re-projects the core's item stream and re-implements turn/queue state.** Android, desktop and macOS each map ~9 item kinds, track in-flight turns and queues, and infer "did my open succeed?" — the recurring class of bug the last audit kept fixing. Core already owns the data but exposes no typed snapshot/op. | cross-client audit §1,2,5; `lib.rs:430-449`, `895-974` |
| **P1** ✓ | **Four god files** hold unrelated subsystems: `lib.rs` 6,618 lines / 264 fns, `manager.cpp` 5,023 lines / 274 methods, `ConnectionManager.kt` 4,384 lines / 276 fns, `ChatScreen.kt` one 1,280-line composable. `roam.rs` 3,262 (half tests) and `session.rs` 3,783 are the next tier. | per-area audits |
| **P1** ✓ | **Docs contradict the code at the front door.** README teaches the stock HTTP transport (AGENTS/transport.rs say it is custom); CHANGELOG has 17 shipped versions buried in Unreleased; CONTRACT's listener snippet lists 17 of 23 callbacks; TRANSCRIPT_MODEL still argues settled questions. | docs audit §1,3,4,5 |

---

## 2. Security posture — beyond the fixed bugs

| Sev | Finding | Evidence / fix |
|---|---|---|
| **P0** ✓ | Digest-as-path traversal and missing digest verification (`lib.rs:3288-3302`). | Require canonical 64-hex digests, verify bytes before rename, create with `create_new`/`O_NOFOLLOW`, assert containment under `cache/files`. |
| **P0** ✓ | Uploads pinned to the main wire (`lib.rs:1909-1935`, `3309+`). | Derive owner from `expect.session_id`/`active_session_id`; use `file_wire(&owner)` per endpoint as downloads already do. |
| **P0** ✓ | Biometric lock does not gate intents (`MainActivity.kt:125-165`). | One choke point in the manager: reject/queue all core intents and notification replies until unlocked; bind the Keystore key to auth. |
| **P1** ✓ | Desktop TLS trust-all default (`manager.cpp:334`). | Flip the constant (pending your decision). |
| **P1** ✓ | Nested roam labels can misroute a chat: `home` + `home:nas` with raw id `nas:s1` forms `roam:home:nas:s1`, resolved to the longer label. A main-list id starting `roam:` can also hijack routing. | `roam.rs:1742-1756`, `lib.rs:5269-5285`. Reject ':' in labels, length-prefix them in ids, validate main ids, and bind owner at open. |
| **P1** ✓ | `PARKED` recipe/elicitation responders are one global slot per family across all wires; a peer can supersede a main form and receive its submitted values. | `spine.rs:138-168`, `unstable.rs:729-735`. Key by request id + owner and carry them in respond intents. |
| **P1** ✓ | `PermissionRequest` carries no session/peer identity; duplicate `tool_call_id`s across wires overwrite the answer closure. | `lib.rs:515-522`, `spine.rs:185-215`. Add owner to the record and reject duplicate ids. |
| **P1** ✓ | Push destination spoofing: non-turn pushes always show and carry the envelope's `session` into a replyable notification; anyone who can POST to the UnifiedPush endpoint can choose the chat a reply lands in. | `notify.rs:236-248`, `Push.kt:94`, `ReplyReceiver.kt:24-30`. Strip/validate `session` for non-turn kinds. |
| **P1** ~ | Roam iroh secret is an unzeroized `String`, cloned per retry and cached in a process-global `OnceLock`; identity writes use predictable temp names and chmod after rename. | `grouse-roam-core/lib.rs:65-71,104-129,269-277`, `lib.rs:5573-5593`, `cache.rs:408-423`. Zeroizing type, random 0600 temps, hash instead of retaining base64. |
| **P1** ~ | A roam card is never pinned to the label that becomes the trust/routing namespace; `card_fingerprint` is displayed but not enforced on reconnect. | `grouse-roam-core/lib.rs:81-87`, `roam.rs:1503,1530`. Store `{label→fingerprint}` on enrollment and verify before dial. |
| **P1** ~ | `download_one` bypasses `session::request`, so a peer that stalls after one chunk parks the task forever with no `Failed` event. | `lib.rs:3226-3236`. Wrap in the shared 30 s policy. |
| **P1** ~ | Desktop writes server-supplied HTML to a predictable `file://` temp file in the no-WebEngine fallback; markdown links reach `Qt.openUrlExternally` with no scheme allowlist; AppBridge pins are keyed by app only, so two chats can cross-wire messages. | `manager.cpp:4306-4339,4353-4367`, `ChatPage.qml:196,1454,1523`, `appbridge.cpp:180-185`. |
| **P2** ~ | Desktop stores the RCE-capable secret key and iroh identity in plain QSettings; it self-installs a host KRunner plugin and global `QT_PLUGIN_PATH`; DBus intents are unauthenticated to session peers; WebEngine guard accepts any 127.0.0.1 port. | desktop audit §2,3, extras. |
| **P2** ~ | Android's biometric verifier authenticates once, not subsequent core operations; the UnifiedPush endpoint is a bearer capability kept in plain prefs and included in backups; a transient Keystore error deletes the secret key and roam identity. | Android-non-UI audit §1,6,8. |

**Verified strengths:** WebPKI + hostname verification is the core default with trust-all strictly
opt-in; `X-Secret-Key` rides only the upgrade header and never appears in logs; no shell execution
in the Rust core; bounded frames/chunks and a 100 MB download ceiling; NUL sanitization and
`catch_unwind` on C ABI entries; sandboxed MCP-App WebViews and escaped desktop markdown.

---

## 3. Core-boundary debt — what belongs in core but isn't

The same five decisions are rebuilt in every client. Ranked by cost of duplication:

| Finding | Core has | Clients reimplement | Core API needed | Effort |
|---|---|---|---|---|
| ✓ Transcript op → UI row projection, seq ordering, ToolGroup fan-out, file/image decode | `Item`, `seq`, one op stream | Android `ConnectionManager.kt:3331-3395`; desktop `manager.cpp:3862-4018`; macOS `ChatModel.swift:76-90` ignores `seq` entirely | one ordered splice op or canonical row projection in the core | M–L |
| ✓ Prompt queues + turn ownership | per-session `pending`/`in_flight`/`active_runs` | Android `PendingSend`/`inFlight`; desktop `m_pendingQueue`/`m_inFlight`; macOS timer-inferred `prompting` | `queued_prompts`, `cancel_queued`, `steer_queued`, typed turn snapshot event | M |
| ✓ Main vs peer ownership and session list | `owner_for`/`peer_for_session`, but `sessions()` returns only the main vector | prefix parsing in all three clients; separate roam models | `session_owner(id)` + unified `all_sessions()`/`on_sessions` | M |
| ✓ My Day board rules (05:00, snooze, due/wake labels) | pure projections in `tasks.rs` emit **raw JSON** | Android re-parses and re-derives; desktop duplicates in `myday.cpp`; macOS no-ops | typed `TaskBoard` records + pure formatter helpers | M |
| ✓ "Did my open succeed?" | knows load success, emits only `on_session_open_failed` | three different Window/Ready/watchdog inferences | `on_session_opened(session_id, mode)` | M |
| ✓ Status/alert/turn-loss derivation | typed `ConnectionStatus`, `connected()` | Android switches, desktop string-matches, macOS string-compares | typed status record with phase/owner/turns-lost | M |
| ~ Elicitation/recipe form decoding | raw schemas, single responder slot | Android and desktop each build a form model | typed `FormRequest{id,owner,fields}` + `respond_form(id,…)` | M |
| ~ File event ownership | `FileEvent` keyed only by server path | Android path→owner FIFO; desktop path-only and can cross-wire | add `session_id`/owner/request id to `FileEvent` | S |
| ~ Backfill/page state | `spawn_fill`, `page`, Window | three clients hold `hasOlder`/loading/watchdogs | `on_backfill_state` or `fill_history(session,n)` with completion | M |
| ~ Sidebar grouping | stamps `mode`, `has_new`, title | two clients re-group; macOS flat list | optional `session_groups()` (view policy stays local) | M, low value |

**Opposite direction:** `on_speak` is consumed only by Android (desktop/macOS discard it) and
should be opt-in at initialize; My Day, thread timeline, file events, sensitivity, roam callbacks
and active-run lanes are no-ops or absent on macOS. Those exclusions are recorded in the ledger.

---

## 4. God files and layering

| Sev | File | Evidence | Remediation |
|---|---|---|---|
| **H** ✓ | `core/grouse-core/src/lib.rs` (6,618 lines, 264 fns, 40 types) | two `impl Core` blocks spanning 3,500 lines; backfill, file downloads, My Day, advisor/labels, reconnect and roam identity all share one `Mutex<CoreState>` | keep facade + records, move `impl Core` blocks to `connection.rs`, `paging.rs`, `hidden.rs`, `my_day.rs`, `roam_intents.rs` |
| **H** ✓ | `clients/desktop/src/manager.cpp/h` (5,023 lines, 274 methods, 63 properties, 114 invokables, 102 members) | one object is QML facade, settings, connection, turn state, queue, attachment pipeline, catalog admin, My Day, MCP dock, projector, notifier, logger | extract `settingsstore`, `coreapi`, `transcriptprojection`, `catalogcontroller`, `chatcontroller`; keep `coreOn*` as dispatch |
| **H** ✓ | `clients/android/.../ConnectionManager.kt` (4,384 lines, 276 fns, 45 state holders) | ~12 jobs; two embedded listener objects with business logic on the core thread; `CoreEventRouter`, `WireController`, `TurnController`, `TranscriptController`, `FeatureController` split proposed | agent's six-file split, facade delegates existing `val`s so UI call sites do not change |
| **H** ~ | `ChatScreen.kt` (one 1,280-line composable) and `MainApp` | 17 local states, app bar, overlays, transcript, composer in one function; body subscribes to `cm.messages`, so every token recomposes the whole screen | `ChatTopBar`/`ChatTranscript`/`ChatComposer`/`ChatOverlays` + `ChatUiState`; scope transcript subscription to a child |
| **H** ~ | `spine.rs::Conn` (~780-line impl, 21 `ConnInner` fields, 7 process-global registries); `roam.rs::RoamPeer` (~34 methods, half the file is tests) | split into Wire/Handshake/SessionBinder/ResponderRegistry/StatusMachine and roam supervisor/transport/session/permissions/state; delete write-only mirrors (`PeerInner::active_run`) |
| **M** ✓ | `session.rs` is 3,783 lines with its test module in the middle and tests for six `ChatEndpoint` doubles; `DispatchHost` is 32 methods wide and `PrefixHost` is 168 lines of mechanical forwarding | split into `session/{params,translate,replay,dispatch,hub}.rs`; split the trait into events/state/transcript groups with a generating macro that still fails compilation on additions |
| **M** ~ | Mirror state with no single owner: `session_cwds` in both Core and RoamPeer, permissions in three registries, active run in both hub and peer, current session in every client | typed `DisplayId`/`SessionKey` instead of `roam:` string parsing; one owner per state; core-owned snapshot for clients |

**Process globals:** `Core::new` installs a panic hook and three registries; two Core instances in
one process silently clobber each other (`lib.rs:1194-1268`, `spine.rs:80-183`). Pass a weak
resolver bundle into `Conn` and leave statics as a C-ABI shim.

---

## 5. DRY and engineering practices (Rust)

- ✓ **Hidden-session factories copied 3×**, with divergent failure policies (`lib.rs:3013-3414`,
  `4384-4735`): one `hidden_host(endpoint, purpose, cwd, failure_policy)` plus one
  `deterministic_call` helper. The same string-key grammar (`file:{key}`, `labels:{slot}`) lives
  only in comments, and one comment is already stale (`""` vs `"tasks"`).
- ✓ **Two `session/list` pagers** (main in `lib.rs:1144-1184`, roam in `roam.rs:1768-1840`) with
  different timeouts, dedupe and partial-error semantics: one walker over `ChatEndpoint`.
- ~ **Two C ABI helper sets** (`core/capi.rs` vs `grouse-roam-core/capi.rs`); the roam one lacks the
  NUL fix and is shipped into every `.so` despite having no caller. Delete `grc_*` or feature-gate it.
- ✓ **`Result<_, String>` everywhere**: a wording change in an error can change reconnect and
  terminal-status behaviour because `spine.rs:1327-1331` sniffs `"timed out"`/`"gave no answer"`.
  Introduce an internal `WireError`/`CoreError` enum; convert only at FFI edges.
- ~ **Test doubles rewritten 16+ times**: every `CoreListener` has 23 required methods and every
  `DispatchHost` 32; a blanket `impl CoreListener for Arc<T>`, a shared `examples/common`, and one
  composable recorder remove most copies.
- ~ **Unstable shim**: `grouse-unstable` is one `pub use` plus a hand-maintained intent list that is
  already stale (5 missing intents), while stable core paths issue 21 `_goose/unstable/*` RPCs
  directly. Either move the shim behind a typed adapter or stop calling the crate a boundary.
- ~ **Roam policy constants local and duplicated** (reconnect cap 25 vs main 6; page timeouts
  15 s vs 60 s; RPC fallback 30 s): one `roam::policy` module with all budgets.
- ✓ **Surface ratchet has holes**: `surface.rs` guards 11 records but not `ServerConfig`,
  `TimelineRow`, `ConnectionStatus`, or any **intent**. Seven user-facing intents exist only on
  the uniffi face; `#[uniffi::export]` on the whole `impl` block also leaks private helpers into
  Kotlin. Add an `INTENTS` inventory and move internal helpers out of the exported block.
- ✓ **ABI listener table** is two hand-written 47-field structs with two independent version
  constants; the gate cannot see layout/arity. Generate the header with cbindgen and cross-check
  both constants and all fields in CI.
- ✓ **Two of the four ABI-17 symbols are gated but unused** by clients (`push_decide_run`,
  `tools_call_named`), so those fixes are unreachable.

---

## 6. Client quality themes

- **Android controller**: six-file split (§4); duplicate reconnect scheduler
  (`RetryState`/`beginRetry` vs core backoff); `noteRoute` flags as untyped UI intents; pure
  decisions trapped in handlers; server text into Toast/HTML/file export with no single
  validation seam. Wire/Dtos/SecureStore audit adds: parsers documented as fail-soft throw on
  wrong JSON types; JSON mappers duplicated across three owners; `GoosePushService` is a
  mini-controller with no test seam.
- **Android UI**: god composable and whole-transcript recomposition (§4); 16 `TopAppBar`, 16
  `AlertDialog`, 6 `ModalBottomSheet` call sites with no shared primitives; stringly-typed routes
  (34 `navigate` calls); `design/tokens.json` only partially mapped (51 literal
  `RoundedCornerShape` calls); accessibility gaps and uncapped panels; server-supplied MIME used
  verbatim for `ACTION_VIEW` (`ChatScreen.kt:2800`); one server image decoded synchronously on the
  main thread.
- **Desktop**: split-brain authority (C++ mirrors core turn state) and hand-built serde DTOs in
  the UI, including a session id interpolated into JSON with `QString::arg` (`manager.cpp:1302`) —
  a quote in a roam label silently drops the bound-session guard; 51 `m_store` touches across 7
  regions; ~20 ad-hoc JSON decodes; dead hooks; `rowFromItem` writes role `"agent"` while
  `lastAssistantText` searches `"assistant"`; unbounded UI caches; `tst_manager` still cannot
  prove an intent→ABI mapping; source lists hand-duplicated between app and tests.
- **macOS**: real 6-file client but no parity column, no `swiftc` in CI, and no roam path;
  `connect()` blocks the SwiftUI main thread for up to 15 s and mints a throwaway session;
  open success inferred from `Reset` while `replace_rich` may emit none on an identical paint;
  one `@Published permission?` drops a second prompt; `onActiveRun` ignored; files/charts/apps
  degrade to text; no refresh/config UI.

---

## 7. Docs, CI and tests as maintenance systems

**Docs** ✓ — four files claim authority (AGENTS, CONTRACT, INTERNAL, PARITY), and the paint and
bind==display invariants are restated in five. Concrete contradictions: README names the stock
HTTP transport; AGENTS/README still call macOS "planned" while CHANGELOG records its fixes;
CHANGELOG has 17 shipped versions as bare lines inside Unreleased and is 10 versions behind the
build file; CONTRACT's listener block lists 17/23; TRANSCRIPT_MODEL argues settled questions and
its Item sketch omits 7 shipped fields; three docs give three test counts; AGENTS says Android
tests are not in CI while its own CI table says they are. Fix: one doc-ownership map
(normative / rationale / historical / ledger), generate listener and Item blocks from
`surface.rs`, and promote version markers to headings.

**CI/release** ✓ — a `v*` tag can publish both assets with **no test/clippy/ctest/parity gate** on
the tagged commit; release signing falls back to the public debug key and still publishes a green
job; the committed `.so` + bindings have no freshness proof (desktop has mtime/symbol guards,
Android does not); three different Rust compilers build the shipped `.so` because rustup resolves
`rust-toolchain.toml` from CWD, not `--manifest-path`; Actions and images are floating tags next
to signing secrets and `contents: write`; Flatpak caching can ship stale QML; macOS never compiles
Swift. Fix: reusable gate workflow + protected environment, hard-fail on missing signer and verify
its digest/versionCode, regenerate-and-diff bindings in CI, build with CWD `core/` and assert
`rustc 1.97.1`, SHA-pin actions.

**Tests** ✓ — the spine suite mutates process-global `XDG_DATA_HOME` but only 16/28 tests take
the lock; the fake server returns `{}` for unknown RPCs, so misspelled methods pass; boundary
tests sleep past production timers; the riskiest plan selection is tested by calling helpers with
flags the test chose, not by driving a real mid-turn open; no C ABI listener test fires any of the
47 callbacks; desktop's intent→ABI proof doesn't exist; macOS has zero XCTest and a name-only
binding check. Fix: RAII temp-dir fixture, strict method dispatch, injectable clock, one
table-driven `plan_for_open` test, table-driven C callback test, signature comparison.

---

## 8. Suggested sequencing

1. **Security P0s** — digest path + verification, upload owner routing, Android lock gate,
   desktop TLS default (pending your call). Small, high impact, no architectural commitment.
2. **Core-boundary events** — `on_session_opened`, typed status/turn snapshot, `FileEvent` owner,
   unified session list. These remove the inference that keeps producing client bugs.
3. **Cross-client row/queue/fill** — item projection op, queued-prompt API, backfill state; do
   Android + desktop + macOS together or not at all.
4. **God-file splits** — one at a time, facade-first, no behaviour change: `lib.rs`, Android
   controller, desktop manager, then `session.rs`/`roam.rs`/`spine.rs`.
5. **Ratchet and CI** — intent inventory, ABI table generation, release gates and freshness
   proof *before* the next large feature, so the refactors stay refactors.
6. **DRY/doc cleanup** last, once the seams are stable.

No code was changed as part of this audit.
