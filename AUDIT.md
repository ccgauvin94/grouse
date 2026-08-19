# Audit Findings & Status

Deep read-only audit of the Grouse monorepo, run 2026-08-18 via five parallel
review agents (2 security, 2 quality, 1 cross-cutting). Every finding cites the
file:line verified by the reviewing agent; the top behavioral claims were
re-verified directly. No builds/tests were run during the audit itself (per
instructions); verification of fixes is done via `cargo test` + `cargo clippy
-D warnings` and is recorded on the fixed rows.

Severity legend: CRITICAL / HIGH / MEDIUM / LOW / INFO.
Status legend: FIXED — done and verified against the repo; DECISION — fix
approach pending a design review; DELEGATED — assigned to the medium
subagent; DONE — completed in the medium batch and verified (cargo for Rust,
inspection for Android/docs); DEFERRED — consciously postponed from the medium
batch with a reason recorded below; TRACKED — long-term debt, no immediate
code change; OPEN — not yet actioned; INFO — positive finding, no action.

## High (priority 1)

| ID | Finding | Axis | Location | Status |
|---|---|---|---|---|
| RC-1 | Roam transport emits Rust `Debug`-case status strings (`InProgress`) and permission kinds (`AllowOnce`) where the spine emits snake_case; the Android UI matches lowercase only, so roam-peer tool progress/error states and permission labels silently break | Correctness | `core/grouse-core/src/roam.rs:1568,1421` (was); `spine.rs:1298-1314`; `Screens.kt:2660-2667,1156` | **FIXED** — spine helpers made `pub(crate)` and reused from roam; both paths now share one snake_case translation. Regression test `shared_status_and_kind_strings_are_snake_case_variants` added. `cargo test -p grouse-core` 74 passed; clippy `-D warnings` clean. |
| RC-2 | Failed `session/prompt` on a roam peer returns before emitting `RunEnded`, leaving in-flight UI wedged; the spine deliberately hardened this (`run_ended("error")`) | Correctness | `roam.rs:615-633` (was) | **FIXED** — `RoamPeer::rpc` now emits `RunEnded { stop_reason: "error" }` on the failure arm, mirroring the spine. Verified with the suite above. |
| S-RC-1 | Trust-all TLS with no hostname verification and no pinning; the sole authenticator (`X-Secret-Key`) travels inside the MITM-terminated tunnel — an on-path attacker can read all ACP traffic and steal the key | Security | `core/grouse-core/src/transport.rs:218-274,80` | **DONE** — real WebPKI + hostname verification is now the default; `ServerConfig` gains `accept_invalid_certs` (trust-all opt-out) and `ca_cert_pem` (private-CA trust). Core verified (`cargo test` + clippy, 75 green); bindings regenerated (`just android-libs`); Android Connect screen exposes the toggle + CA field. |
| A-1 | Archiving is irreversible from the Android app: `archiveSession` is wired but `unarchiveSession` is never called and no archived view exists; the dialog/KDoc promise restoration | API coverage | `ConnectionManager.kt:762-765`; `Screens.kt:1323,1396-1399` | **DONE** — `SessionSummary` carries `archived` (parsed from `_meta.archivedAt` instead of dropped); bindings regenerated; Android maps it, filters active lists, adds an ARCHIVED drawer section and an Unarchive action (restores via the existing `unarchiveSession` RPC). |
| X-1 | Desktop C++ reimplements the wire+state the Rust core owns (~4,000 LOC across `acpclient`+`manager`+`*transport.*`); every protocol fix must land in both — documented deviation, largest DRY/migration debt | DRY | `clients/desktop/src/*` vs `core/grouse-core/src/*` | TRACKED — migration project; already recorded in AGENTS.md |

## Medium (priority 2 — delegated)

| ID | Finding | Axis | Location | Status |
|---|---|---|---|---|
| S-RC-2 | `RoamCodec::feed` grows an unbounded buffer on newline-less streams (memory DoS) | Security | `roam.rs:103-116` | **DONE** — partial-frame buffer capped at `MAX_FRAME_BYTES` (1 MiB); oversized frames truncate + re-sync. `cargo test -p grouse-core` + clippy `-D warnings` clean. |
| S-RC-3 | Inbound WebSocket frames pushed to an unbounded channel, no backpressure | Security | `transport.rs:185-191` | **DONE** — bounded 128-frame inbound hand-off; reader awaits capacity so TCP flow control pushes backpressure onto the server. `cargo test -p grouse-core` + clippy `-D warnings` clean. |
| S-RC-4 | Unbounded transcript bubbles + O(n) backward scan per tool update (quadratic over a long chat) | Security | `transcript.rs:40-60,282-300`; `roam.rs:1191-1270,1287-1460` | **DONE** — `TranscriptStore` now indexes tool calls by `tool_call_id` (O(1) `tool_update`) and caps bubbles at `MAX_BUBBLES`; the roam peer caps its message lists. `cargo test -p grouse-core` + clippy `-D warnings` clean. |
| S-RC-5 | Cache writes non-atomic/non-fsync; `roam_identity` written 0644 (world-readable iroh secret); cache.rs does not mirror the desktop's mktemp+swap | Security | `cache.rs:220-230`; `lib.rs:1524-1525` | **DONE** — `cache::atomic_write` (temp+fsync+rename, mirroring the desktop) + `roam_identity` chmod 0600. `cargo test -p grouse-core` + clippy `-D warnings` clean. |
| S-RC-6 | Roam post-handshake RPCs block runtime workers with no timeout | Security | `roam.rs:407-460,505-620` | **DONE** — post-handshake RPCs bounded by `RoamPeer::RPC_TIMEOUT` (30s); runaway replies never pin the caller. `cargo test -p grouse-core` + clippy `-D warnings` clean. |
| S-RC-8 | Core passes server/tool text to UIs unescaped; XSS burden entirely on UI surfaces | Security | `spine.rs:1000-1015`; `roam.rs:1520-1560` | **DONE** — resolved as core-side *policy*, not core-side escaping: `core/CONTRACT.md` §8 documents the trust boundary (core delivers content verbatim; every UI treats it as untrusted at its rendering surface and sanitizes/sandboxes HTML/JS renderers). AGENTS.md protocol notes point at it. |
| S-A-2 | Plaintext `roam_identity` copied to `filesDir` is NOT excluded from Auto Backup / device transfer, defeating the encrypted-store protection | Security | `ConnectionManager.kt:679-691`; `backup_rules.xml:4-9`; `data_extraction_rules.xml:4-14` | **DONE** — `roam_identity` excluded from cloud backup + device transfer in both XMLs. Compile-safe by inspection. |
| S-A-3 | MCP-App template injected via `iframe.srcdoc` with no CSP/sandbox, same-origin with the `GrouseHost` JS bridge | Security | `Screens.kt:2314-2448` | **DONE** — host page gained a strict CSP meta and the guest iframe is now `sandbox="allow-scripts"` (opaque origin: scripts + postMessage only, no parent-DOM/localStorage/cookies). Bridge surface unchanged. `Bubbles.kt` (post-split). |
| S-A-4 | Chart spec interpolated unescaped into a `<script>` block (`const spec = __SPEC__`) | Security | `Screens.kt:2440` | **DONE** — `chartHtml` JSON-encodes the spec (`org.json.JSONObject.quote`) and neutralizes `<` as `\u003c`, so an injected `</script>` cannot terminate the template's script element. Covered by `ChartHtmlTest`. `Bubbles.kt` (post-split). |
| S-A-5 | `SecureStore` deletes the entire store on any exception, wiping key + iroh identity on transient failures | Security | `SecureStore.kt:29-41` | **DONE** — wipe only on `GeneralSecurityException` (key invalidation/restore); transient failures log + propagate, store intact. Compile-safe by inspection. |
| RC-3 | `chunk_text`/`tool_kind` duplicated between spine and roam and drifted (roam renders image/audio as `[unknown]`; McpApp input differs) | DRY / correctness | `spine.rs:1283-1291,810-860` vs `roam.rs:1475-1495,1497-1555` | **DONE** — single shared `spine::content_block_text` + `spine::tool_call_kind`; both spine and roam reuse them (drift eliminated). `cargo test -p grouse-core` + clippy `-D warnings` clean. |
| RC-4 | CONTRACT.md drift: 6 uniffi-exported items + 13 raw-JSON listeners undocumented; `open_session`/shim do synchronous in-intent network work contrary to CONTRACT §1 | Adherence | `lib.rs:245-299,506-517,551,569,757`; `unstable.rs:336` | **DONE** — CONTRACT.md §1/§7 now document the blocking-intent reality (bounded by `RoamPeer::RPC_TIMEOUT`) and the unresolved shim surface; the §5 raw-JSON listeners are enumerated in a documented exception note. |
| RC-5 | `GrouseUnstable::steer` cannot target a roam-owned chat (injects into the wrong conversation while roam is active) | API coverage | `unstable.rs:66-82` | **DONE** — steer resolves the active session's owner via `route()`/`peer_for`; roam-owned chats route to the peer, unresolved sessions refuse cleanly. Regression test `steer_routes_roam_owned_active_session_to_peer`. `cargo test -p grouse-core` + clippy clean. |
| RC-6 | Unused dependencies in `grouse-roam-core`: `rand = "0.8"`, `anyhow` dev-dep (zero references) | Cleanliness | `core/grouse-roam-core/Cargo.toml:23,29` | **DONE** — `rand` + `anyhow` dev-dep removed; lockfile updated. `cargo check -p grouse-core`, `cargo test -p grouse-core`, clippy `-D warnings` all clean. |
| A-2 | Dead/duplicate imports + dead top-level code in the two largest Kotlin files | Cleanliness | `Screens.kt:20-66`; `ConnectionManager.kt:12-22` | **DONE** — removed dead/duplicate imports and dead `CONFIG_IDS`/`mainThread` from Screens.kt; dead imports removed from ConnectionManager.kt. Compile-safe by inspection. |
| A-3 | 13 hardcoded duplicated status hex colors bypass the theme (5 shared colors re-derived inline) | Adherence | `Screens.kt:302,303,537,539,609,610,648,1243,3611,3612,3671,3774,3775` | **DONE** — status colors now come from `MaterialTheme.statusColors.{online,connecting,offline}` (mapped from `design/tokens.json color.semantic.{light,dark}.status`); 0 hardcoded status hexes remain outside `Color.kt` (X-4). `ChatScreen.kt`/`Drawer.kt`/`RoamScreens.kt` (post-split). |
| A-4 | Recipe provider picker hardcodes `["", "openai", "openrouter", "openrouter_custom"]`; a 4th server-configured provider is silently unavailable for recipes | Adherence | `Screens.kt:2984` | **DONE** — picker now sources from `cm.providerChoices(provider)` (the live server inventory) with a leading blank. Compile-safe by inspection. |
| A-5 | God files: `Screens.kt` 3,940 lines, `ConnectionManager.kt` 2,240 (all screens + unstable parsers + connect state machine) | Maintainability | `Screens.kt`; `ConnectionManager.kt` | **DONE** — `Screens.kt` split by surface into 12 per-concern files (`ConnectScreens`, `ChatScreen`, `Drawer`, `ProjectScreens`, `SettingsScreens`, `Dropdowns`, `Bubbles`, `RecipeScreens`, `InstanceScreens`, `CatalogScreens`, `RoamScreens`, `QrScan`); `Screens.kt` deleted. `ConnectionManager`'s companion wire parsers + record translators moved to top-level `Wire.kt` (pure, JVM-testable). |
| A-6 | Composer hardcodes the exact `design/tokens.json component.composer` values; raw dp/radius literals throughout | Adherence | `Screens.kt:803-805` | **DONE** — `GrouseShapes` object (`ui/theme/Shapes.kt`) exposes `composer`/`toolChip`/`userBubble` from tokens; the composer, tool-chip, and user-bubble corner sites now use them (scoped to those three per the audit; not a full dp sweep). `ChatScreen.kt`/`Bubbles.kt` (post-split). |
| A-7 | ~182 hardcoded user-facing strings vs 4 string resources | Maintainability | `Screens.kt`; `Notifier.kt` | **DONE** — 151 `stringResource(R.string.*)` callsites across the split screen files; 124 string resources added to `strings.xml` (header notes "single-language by design; extraction is for hygiene, not i18n"). Dynamic/interpolated and technical/kind-label strings deliberately left inline (e.g. model names, timestamps, slash-command labels). |
| A-8 | Duplicated config-id list (`optionIds` vs dead `CONFIG_IDS`) | DRY | `ConnectionManager.kt:529`; `Screens.kt:136` | **DONE** — one shared `internal val CONFIG_IDS` in Screens.kt, referenced by `optionIds` in ConnectionManager.kt; the duplicate is gone. Compile-safe by inspection. |
| A-9 | `decodeImageBlock` runs Base64+BitmapFactory decode on the UI thread during composition | Best practices | `Screens.kt:2480,2495-2498` | **DONE** — decode now runs off-main via `produceState` + `withContext(Dispatchers.Default)`, with a bounded content-addressed cache (`ConcurrentHashMap`, cap 16) so identical images decode once. `Bubbles.kt` (post-split). |
| A-10 | Stale/orphaned KDoc for removed features; duplicated provider filtering | Cleanliness | `SecureStore.kt:230,246-252` | **DONE** — orphaned KDoc (removed "tool actions" setting) removed from `SecureStore.kt`; the duplicated provider filter gone — `ModelSheet` now consumes `ConnectionManager.providerChoices(current)` instead of re-deriving from `configuredProviders` (which was removed as dead). |
| A-11 | Untested wire parsers the code itself documents as bug-prone (`toExtensionDto`, cron builders, recipe/schedule/extension parsers) | Testing | `Dtos.kt:118-157`; `Screens.kt:2764-2875`; `ConnectionManager.kt:1952-2100` | **DONE** — new `WireParserTest.kt` (JVM) covers `toExtensionDto`, `parseCron`/`buildCron`, and the recipe/schedule/global/session extension parsers; the parsers were moved to internal/pure seams to make them testable. Compile-safe by inspection. |
| A-12 | `mockwebserver` declared `testImplementation` but never imported | Cleanliness | `app/build.gradle.kts:141` | **DONE** — dropped the unused `mockwebserver` dependency (the A-11 parser tests are pure and do not need it). Compile-safe by inspection. |
| A-13 | No Gradle version catalog; release `isMinifyEnabled=false` while shipping `material-icons-extended`; blanket `kotlin-stdlib` force | Best practices | `build.gradle.kts:1-10`; `app/build.gradle.kts:92,110-114` | **DONE** — `gradle/libs.versions.toml` version catalog created; root + app build files rewritten to `libs.*` accessors. The `kotlin-stdlib` force is **kept** (removal re-broke resolution — UnifiedPush pulls stdlib 2.3.0 whose metadata the 2.0.20 compiler can't read; this is the plan's A-13 contingency, documented in the build file). Minification stays OFF: R8 with JNA/uniffi keep rules can't be runtime-verified on this host, so flipping it blind risks a broken APK (conscious deferral). |
| X-2 | `grouse-unstable` (the 35-method `_goose/unstable/*` shim) has zero tests — highest-risk untested core surface | Testing | `core/grouse-unstable/src/lib.rs` | **DONE** — added `#[cfg(test)]` tests locking the silent-param-drop contract (`re_exported_types_are_usable`, `all_intents_are_clean_noops_when_disconnected`); `cargo test -p grouse-unstable` passes. |
| X-3 | Broken reference: `clients/cli/Cargo.toml` does not exist yet is wired into the devcontainer and instructed by CONTRIBUTING | Organization | `.devcontainer/devcontainer.json:11,18`; `CONTRIBUTING.md:44,63` | **DONE** — dangling `clients/cli` refs removed/neutralized in the devcontainer + CONTRIBUTING (noted as not-yet-written). devcontainer.json validated as JSON. |
| X-4 | Design tokens not authoritative: `tokens.json` claims single source but platforms hardcode identical hexes; ChartBubble palette absent from tokens | Organization / DRY | `design/tokens.json:5`; `Color.kt:9-28`; `values(-night)/colors.xml`; `ic_launcher_foreground.xml:8-15`; `main.qml:472`; `ChartBubble.qml:31-32` | **DONE** — tokens authoritative for status + component (composer/tool-chip/user-bubble) on Android: `tokens.json` v1.1.0 adds `color.semantic.{light,dark}.status` + `color.raw.chart.series`; Android status/shape tokens consume them; 0 hardcoded status hexes remain; `ThemeTokensTest` pins the mapping. Desktop adoption (Kirigami `ChartBubble` etc.) awaits the desktop work — desktop is out of scope. Codegen from tokens.json still doesn't exist (hand-synced; recorded gap). |
| X-5 | Stale/contradictory docs: desktop README "Not yet" list + Layout + split-repo naming; desktop AGENTS.md "no test targets" vs CTest wiring | Adherence | `clients/desktop/README.md:6,8,12,13,17,47,50-58,61`; `clients/desktop/AGENTS.md:218` | **DONE** — stale/contradictory desktop README entries and the `AGENTS.md:218` CTest contradiction corrected. |
| X-6 | gitleaks runs with no config/baseline/ignores — no allowlist mechanism | Security hygiene | `.github/workflows/secrets.yml:6-12` | **DONE** — added a `.gitleaks.toml` allowlist config and wired it via `GITLEAKS_CONFIG` (the gitleaks-action interface is env-var based, with no `config-file` input). Config validated first-run: git-mode `gitleaks detect` exit 0, no leaks (all 55 no-git working-tree hits were untracked `core/**/target/` build artifacts — PEM markers compiled into ed25519/pkcs8 deps — not committed). |
| X-7 | Unpinned Actions/container images; CI `stable` rust vs devcontainer 1.97.1; no `rust-toolchain.toml`/MSRV | Best practices | workflows; `.devcontainer/Containerfile:14` | **DONE** — Action/container version pins (earlier); CI rust toolchain now pinned to 1.97.1 (`dtolnay/rust-toolchain` `toolchain:` input), `core/rust-toolchain.toml` added, matching devcontainer `ARG RUST_VERSION=1.97.1`. See the Medium deferred note for the MSRV decision. |
| X-8 | No per-file license headers; stale `ccgauvin94/grouse` repo URL; no AUTHORS/NOTICE | Organization | all sources; `grouse-core/Cargo.toml:7` | **DONE** — repo-root `AUTHORS` added (from `git log`, one author); `// SPDX-License-Identifier: AGPL-3.0-or-later` added to every `.kt` under `clients/android` (39) and every source `.rs` under `core` lacking it (13; the one already-headered kept). `grouse-core/Cargo.toml` repo URL verified non-stale (false positive). No NOTICE (LICENSE suffices for AGPL). |
| X-9 | `env.sh` hardcodes `ANDROID_HOME=$HOME/Android/Sdk` + PATH (documented convenience) | Best practices | `clients/android/env.sh:29-31` | **DONE** — `ANDROID_HOME` is now auto-detected (env `ANDROID_HOME`/`ANDROID_SDK_ROOT` plus common paths, per-OS) with a documented fallback; verified under `set -euo pipefail` (detection, fallback, and invalid-JAVA_HOME all exit 0). |

## Medium — deferred (still DELEGATED, consciously postponed)

The following were consciously **not** completed and remain delegated with
their reasons rather than silently dropped. Everything else the remainder plan
addressed is marked DONE in its row above.

- **X-7 (MSRV decision):** toolchain is pinned to 1.97.1 via
  `rust-toolchain.toml` + CI + devcontainer; declaring a formal MSRV in metadata
  remains a maintenance-policy decision.

## Low / informational

| ID | Finding | Axis | Location | Status |
|---|---|---|---|---|
| S-A-6 | MainActivity exported with NEW_CHAT/OPEN_SESSION handlers, no permission — benign availability noise | Security | `AndroidManifest.xml:25-54` | OPEN — no action required |
| S-RC-7 / RC-7 | Stale `panic=abort` comment in `capi.rs` contradicts workspace `panic=unwind` | Maintainability | `capi.rs:5-7` | **DONE** — comment corrected to describe the `panic="unwind"` release profile (uniffi catch_unwind → foreign exception). |
| S-RC-9 | Verified positive: no secret leakage of X-Secret-Key in logs/Debug/URLs | Security | various | INFO |
| RC-8 | No `tracing`/`log` — two deliberate `eprintln!` sites only | Maintainability | `lib.rs:1136,1178` | INFO — deliberate |
| X-10 | Positive findings: no committed secrets; shell scripts clean (`set -euo pipefail`, mktemp+trap, stage-then-swap); AGPL-3.0 uniform; CI matches documented no-tests-in-CI plan | — | `scripts/*.sh`; `LICENSE`; workflows | INFO — no action |

## Deferred design decisions (resolved this round)

- **S-RC-1 / S-A-1 (TLS trust-all):** RESOLVED — real WebPKI + hostname
  verification is now the default (`ServerConfig.accept_invalid_certs` +
  `ca_cert_pem`; verified at `transport.rs`, regenerated bindings, Android
  Connect-screen toggle). Evidence-driving fact: the live public host serves a
  valid Let's Encrypt chain, so trust-all was a pure downgrade on every public
  path. Desktop's separate C++ `VerifyNone` path is a recorded parity gap.
- **A-1 (archive restore):** RESOLVED — `SessionSummary.archived` (from
  `_meta.archivedAt`) instead of the silent drop; bindings regenerated;
  Android gets an ARCHIVED drawer section and an Unarchive action wired to the
  existing `unarchiveSession` RPC.
- **X-1 desktop migration:** TRACKED — ~4,000 LOC duplicated C++ client; the
  designated migration path is documented in AGENTS.md.

## Fix order (recommended)

1. RC-1, RC-2 — DONE + verified (this round).
2. S-RC-1 TLS + A-1 archive — DONE + verified (this round): core changes
   gated by `cargo test --workspace` + `clippy -D warnings`; bindings
   regenerated via `just android-libs`; `compileDebugKotlin` + all 47 JVM
   unit tests green on the machinery-verified path.
3. Medium batch — DONE (this round): S-RC-2…S-RC-6, S-A-2, S-A-5, RC-3…RC-6,
   A-2, A-4, A-8, A-11, A-12, X-2, X-3, X-5…X-9 (see the Medium rows +
   deferred note for the consciously postponed remainder). Note: the medium
   agent's `WireParserTest` shipped with two bugs (missing kotlinx
   serialization imports; a recipes fixture that omitted the `recipe.settings`
   object the parser reads) — both fixed during verification, fixtures corrected
   to the real wire shape.
4. X-1 desktop migration — tracked separately.
