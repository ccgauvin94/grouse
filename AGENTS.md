# Repository Guidelines

This file is the single source of truth for how the grouse monorepo is organized,
how its pieces talk to each other, and how to work on it. Read it before adding a
platform, changing the wire, touching the core's public API, or editing any UI.

**How to read this repo:** this document replaces nothing and cites everything —
every path, command, and pattern here was verified against the current tree. Where
a documented convention across the repo differs from what you find, the code and
the inner contract files (`core/CONTRACT.md`, `core/grouse-core/INTERNAL.md`,
`clients/desktop/AGENTS.md`, `CONTRIBUTING.md`) govern.

---

## Project Overview

Grouse is a set of native chat clients for a self-hosted **goose** server
(`goose serve`, upstream goose, held by the Linux Foundation). The wire is
**ACP (JSON-RPC 2.0) over WebSocket** (and over iroh for the "roam" peer
transport). Clients expose sessions, transcripts, tools, permissions, and recipe
runs; the server owns sessions, memory, tools, and model choice — the clients
hold only caches.

One implementation of client logic exists: **two Rust crates in `core/`**, built on
the official goose GDK (the `agent-client-protocol` SDK, Client role, with the
`agent-client-protocol-http` WebSocket transport) as a daemon-based integration.
Every platform is a thin, native UI that drives that core and does not reimplement
client logic. There is no cross-platform UI framework, no shared web view.

## Architecture & Data Flow

### One Rust core, many thin native UIs

`core/` holds three crates, split along the stable/unstable line plus the roam
transport:

- **`grouse-core`** — the stable ACP surface on the official
  `agent-client-protocol` SDK: initialize handshake, session lifecycle,
  prompt/streaming, tools, permissions, config, and the standard `session/update`
  tags. Owns the WebSocket transport (`transport.rs`: custom `ConnectTo` sending
  the `X-Secret-Key` header; trust-all TLS for the server's self-signed cert).
- **`grouse-roam-core`** — the roam transport: iroh peer dialing, device identity,
  connection cards, and the authenticated byte duplex ACP is framed over. Exposes
  its own uniffi Kotlin bindings **and** a C ABI (`src/capi.rs`). Linked into
  `grouse-core` (no separate shipped `.so` on Android).
- **`grouse-unstable`** — the goose-fork compatibility shim: every
  `_goose/unstable/*` method, plus the SDK's `unstable_elicitation` feature and
  the goose-custom `status_message`/`message_usage` notifications. Marked for
  retirement as the GDK absorbs each feature upstream.

### The uniffi contract is the API boundary

The core's public surface is defined once as a uniffi interface. That interface
is the contract every foreign-language UI consumes; nothing else may reach into
the core.

- **Android (Kotlin/Compose)** consumes the uniffi-generated Kotlin bindings.
- **macOS (SwiftUI, later)** will consume the uniffi-generated Swift bindings.
- **Linux desktop (Qt6/KF6 Kirigami, planned)** will consume a hand-written C ABI
  (`extern "C"`, mirroring `core/grouse-roam-core/src/capi.rs`). **Not yet true.**
- **CLI (Rust TUI, not yet written)** consumes the crates directly — no FFI.

**Rule: a change to shared behavior changes the uniffi interface (and the C ABI)
first, regenerates the bindings, then adapts each UI.** Never fork client logic
into a UI to avoid changing the contract.

### Known deviation: the desktop carries its own client

`clients/desktop/` predates the Rust core and still implements the client logic
in C++: `src/acpclient.{h,cpp}` is a second ACP wire client, `src/manager.{h,cpp}`
a second session/chat state machine and cache (the format
`core/grouse-core/src/cache.rs` mirrors), and `src/{websocket,roam}transport.*`
second transports. It reaches the core only by dlopening `libgrouse_roam_core.so`
for the iroh dial. This is migration debt, not a sanctioned pattern: **until it
lands, protocol fixes must be applied to BOTH the Rust core and the desktop's
C++**, and `clients/desktop/AGENTS.md` documents the C++ side.

### Per-platform native toolkits

|Platform|Toolkit|Binding|
|---|---|---|
|Linux desktop|Qt6 + KF6 Kirigami (QML/C++)|C ABI (planned; today: own C++ client)|
|Android|Kotlin + Jetpack Compose|uniffi Kotlin|
|CLI|Rust TUI|direct crate dependency (placeholder — see Runtime/Tooling)|
|macOS (later)|SwiftUI|uniffi Swift|
|Web|deferred — not a native UI|—|

### Data flow (core model)

- **Runtime model** (`core/CONTRACT.md` §1): UI → core **intents** are
  synchronous, fire-and-forget enqueues onto the core's single tokio runtime
  (one worker thread). Outcomes arrive as typed methods on the `CoreListener`
  callback interface; **getters** return immutable `Record` snapshots. UIs marshal
  events onto their own main thread.
- **Intents** never return the result of network work; they change a state machine
  or queue a request. Outcomes arrive as events.
- **Session lifecycle** is owned by the core: `ready` ⇔ an open session id exists;
  `send_prompt`/`set_config_option`/tool queries queue until `ready`, then flush
  in order. Reconnect uses exponential backoff (500ms·2ⁿ, cap 15s, 6 attempts,
  reset on Ready; none on explicit `disconnect`). Remote-change resync debounces
  `session_info_update` (1.5s) → probes `_goose/unstable/session/info` → in-place
  `session/load` replay → re-probes at 8s ×3.

### Protocol notes that matter

The server is `goose serve`; the wire is ACP over WebSocket. These details bite;
keep them when extending the core.

- **`_goose/unstable/*` lives in `grouse-unstable`, a compatibility shim — not
  the core's identity.** Those methods come from a goose fork; the GDK is
  absorbing them upstream. Prefer the GDK surface for new work. Set in use:
  `session/steer`, `session/export`, `session/rename`, `session/archive`,
  `session/unarchive`, `session/info`, `session/recipe/request-params`,
  `tools/list`, `sources/list|create|delete|update` (`type: project|skill`),
  `recipes/list`, `schedules/list`, `config/extensions/list|set-enabled|add`,
  `session/extensions/list|add`.
- **`session_info_update`** is the server's notification that a session changed —
  including changes by another client. It carries `_meta.goose.activeRunId`, the
  live run id required to steer (`session/steer` sends `expectedRunId`). Debounce
  bursts and re-probe via `_goose/unstable/session/info` rather than full resync.
- **roam** is ACP over iroh direct peer streams (`goose serve --roam`): same
  JSON-RPC, newline-framed (ACP ByteStreams framing, identical to goose on stdio),
  over an iroh connection between two peers instead of a WebSocket to a host.
  **Roam peers are parallel connections, not replacements** for the main
  connection. Sessions bound to a peer use the `roam:<peer>:<id>` id prefix for
  sidebar grouping; session-bound unstable RPCs route to the owning peer.
- **camelCase vs snake_case**: `recipes/*` and `schedules/*` params are snake_case
  (`cron_schedule`, `file_path`); nearly everything else is camelCase. **A
  misspelled param is silently dropped, not rejected.**
- **Sessions are typed by `_meta.client`.** `session/new` without it creates an
  `acp` session, which desktop and CLI never list. Each client sets its own
  `_meta.client` value on `session/new` (e.g. `"grouse-desktop"`, `"grouse"`,
  `"grouse-cli"`).
- **`session/load` rewrites `working_dir` from the cwd you send.** Never guess a
  cwd — carry the session's real cwd from `session/list` or ask the server
  (core resolves it via `_goose/unstable/session/info`).
- **`session/new` needs an absolute cwd** that exists inside the goose container;
  goose has no default. It is asked for at connect time.
- **Recipe parameter requests** hard-fail unless the client declares
  `clientCapabilities._meta.goose.recipeParameterRequests: true` at initialize.
- **WebSocket TLS is deliberately trust-all** (self-signed cert; tailnet-only,
  authed by `X-Secret-Key`). A regenerated cert must not lock clients out.

## Key Directories

|Path|Purpose|
|---|---|
|`core/`|Rust workspace (`core/Cargo.toml` is the workspace root — the repo root has no Cargo.toml). The three crates above. `core/CONTRACT.md` = the uniffi interface spec; `core/grouse-core/INTERNAL.md` = pinned internal module seams + threading model.|
|`core/grouse-core/src/`|`lib.rs` (uniffi surface + Core/GrouseUnstable objects), `transport.rs` (WsTransport), `spine.rs` (live connection + handshake + notification dispatch), `transcript.rs` (TranscriptStore: chunks → Message bubbles), `cache.rs` (CacheStore: per-session transcript + tools), `unstable.rs` (35 shim methods), `roam.rs` (peer registry). Bindings staged in `bindings/kotlin/`.|
|`core/grouse-roam-core/src/`|iroh transport (`lib.rs`), C ABI (`capi.rs`).|
|`clients/desktop/`|Qt6/KF6 Kirigami C++/QML desktop client + KRunner plugin (its own `AGENTS.md` is the C++-side contract).|
|`clients/android/`|Kotlin/Compose client (app) + `grouse-core-aar` module carrying the uniffi bindings and native `.so`s.|
|`clients/cli/`|Rust TUI — **README placeholder only, no `Cargo.toml` yet.**|
|`design/`|Shared design tokens (`tokens.json` + `design-language.md`), owned by the design-language workstream, not platform dirs.|
|`scripts/`|`build-android-libs.sh` (Android native libs + bindings), `dev-env.sh` (devcontainer wrapper on podman).|
|`.github/workflows/`|CI: `core.yml`, `android.yml`, `flatpak.yml`, `secrets.yml`.|
|`.devcontainer/`|Rocky Linux 9 dev container for core + CLI only.|

## Development Commands

All commands are the exact ones CI and the repo docs use.

### Core (Rust) — the main development loop

```sh
# Build / test the Rust workspace
cargo build --manifest-path core/Cargo.toml
cargo test  --manifest-path core/Cargo.toml

# Lint gate (CI enforces it with -D warnings)
cargo clippy --manifest-path core/Cargo.toml --all-targets -- -D warnings
```

### Justfile recipes (top-level entrypoints)

```sh
just build         # cargo build --manifest-path core/Cargo.toml
just test          # cargo test  --manifest-path core/Cargo.toml
just desktop       # cd clients/desktop && ./build-flatpak.sh
just android-libs  # ./scripts/build-android-libs.sh   (regenerate .so + uniffi Kotlin)
just android       # cd clients/android && ./gradlew assembleDebug
just scan          # gitleaks detect --source .
```

### Android

```sh
cd clients/android && ./gradlew assembleDebug        # debug APK
cd clients/android && ./gradlew --no-daemon testDebugUnitTest   # JVM unit tests
```

- After touching `core/`, regenerate native libs + uniffi bindings FIRST:
  `just android-libs` (requires rustup targets `aarch64-linux-android` +
  `x86_64-linux-android`, `cargo-ndk`, `uniffi-bindgen`, NDK). Outputs are staged
  into `clients/android/grouse-core-aar/src/main/{jniLibs,kotlin}` and
  `core/grouse-core/bindings/kotlin/`. CI does NOT run this — the `.so` +
  bindings are committed pre-staged.
- `clients/android/env.sh` sets `JAVA_HOME` (JDK 17) and `ANDROID_HOME`; source it.
- Release signing is env-driven (`GROUSE_KEYSTORE`/`GROUSE_STORE_PASSWORD`/
  `GROUSE_KEY_ALIAS`/`GROUSE_KEY_PASSWORD` or gradle props), with debug-signing
  fallback when unset.

### Linux desktop (Qt6/KF6)

```sh
distrobox enter kde-build -- bash -lc 'cmake -B build -S . -DCMAKE_BUILD_TYPE=Debug && cmake --build build -j$(nproc)'
cd clients/desktop/build && ctest --output-on-failure     # full desktop suite
just desktop                                             # Flatpak bundle
```

See `clients/desktop/AGENTS.md` for the complete desktop workflow (qmllint,
`xvfb-run` page-load check, KRunner plugin build/install, flatpak-builder footguns).

### Devcontainer

```sh
scripts/dev-env.sh                                             # interactive shell
scripts/dev-env.sh cargo build --manifest-path core/Cargo.toml  # one command
```

### Secrets

```sh
gitleaks detect --source .    # CI-gated; a commit must not contain secrets
```

### CI (`.github/workflows/`)

|Workflow|What it runs|Required check|
|---|---|---|
|`core.yml`|`cargo test` + `cargo clippy --all-targets -- -D warnings` on stable|`Core / test`|
|`android.yml`|`./gradlew --no-daemon assembleDebug` + APK artifact|`Android / apk`|
|`flatpak.yml`|KRunner + roam `.so` host build, then Flatpak bundle (org.kde.Platform 6.10)|`Flatpak / host-components`, `Flatpak / flatpak`|
|`secrets.yml`|gitleaks|`Secrets scan / gitleaks`|

**There are NO test jobs in CI for Android unit tests or desktop `ctest`** — run
those locally.

## Code Conventions & Common Patterns

### Rust core

- **Versioning/toolchain**: edition 2021, no `rust-toolchain.toml`, no pinned MSRV.
  CI uses latest stable + clippy; the devcontainer pins 1.97.1. `uniffi = "0.32"`
  and `tokio = "1"` across members.
- **Threading**: one tokio runtime on a worker thread; all `#[uniffi::export]`
  intents are synchronous (enqueue + return). `connect()` is the sole exception
  (bounded blocking until initialized). All network I/O, reply dispatch, and
  `CoreListener` callbacks run on the runtime thread. Request/reply uses the
  SDK's `send_request(...).block_task().await` — there is NO global request-id
  table.
- **Error handling**: FFI-facing errors are `#[derive(Debug, uniffi::Error)]`
  enums (e.g. `RoamError::Message(String)`). `thiserror`/`anyhow` appear only
  transitively; the crates hand-write boundary errors. `Conn::rpc(...) ->
  Result<Value, AcpError>` is the ONLY seam slices use to talk to the server
  (`INTERNAL.md` §pinned seams).
- **Module invariants**: `transcript.rs` and `cache.rs` do NO network. The
  `TranscriptStore` owns bubble shaping: chunks append into a bubble keyed by
  `(role, message_id)`; live output appends, the completion update replaces;
  replay chunks are gated by message id; consecutive tool calls collapse into a
  group. `CacheStore` is dumb I/O keyed by session id — freshness (via
  `updatedAt`) is the caller's job.
- **Naming**: Rust is `snake_case`; protocol JSON is `camelCase` (except
  `recipes/*`/`schedules/*` params) — do not confuse the two; a misspelled param
  is silently dropped.

### Desktop (C++/QML) — see `clients/desktop/AGENTS.md` for the full list

- camelCase everywhere: QML properties, C++ methods, `Mgr` Q_PROPERTYs.
- QML ↔ C++: read via context properties / `model.<role>`; write via Q_INVOKABLE
  + signals. **Never two-way bindings** (QQC2 TextField/TextArea break `text:`
  bindings on edit); dialogs push one-way.
- Never republish a streaming transcript as a `QVariantList` — QML treats it as a
  brand-new model (full reset, scroll jump, quadratic). Use incremental
  `insertRows` + deferred `dataChanged` (see `MessageListModel::updateDeferred`).
- Streamed chat HTML renders `Text.PlainText` while streaming and `Text.RichText`
  only at finalize (RichText re-parses every frame — quadratic).
- ComboBox with a JS array of `{value, name}` MUST set `valueRole: "value"`.
- Comments explain *why* something is not the obvious thing; do not restate code.

### Design language (`design/`)

- Tokens describe **what** a surface is, never how to draw it; each platform maps
  them to its native toolkit (QML singleton, Compose theme, etc.).
- `tokens.json` groups: `color` (raw palette + `semantic` light/dark), `type`,
  `spacing` (4dp base), `radius`, plus a `component` group. UI code consumes
  `semantic.*` and named steps (`spacing.4`, `radius.xl`) — never raw hexes, never
  magic numbers. Semantic-over-cosmetic naming; every filled role gets an `on*`
  partner.

### Cross-cutting

- **Protocol fixes must land in BOTH `grouse-core` Rust and desktop C++** until
  the desktop migration lands (and be reflected in `core/CONTRACT.md` +
  `clients/desktop/AGENTS.md`).
- Never hardcode a machine-specific path into a shared file.

## Important Files

|File|Role|
|---|---|
|`core/CONTRACT.md`|The uniffi interface spec — the API boundary. Read before touching the core's public surface.|
|`core/grouse-core/INTERNAL.md`|Pinned internal module seams, threading model, event fan-out rules.|
|`core/grouse-core/src/lib.rs`|uniffi surface: records/enums, `Core` + `GrouseUnstable` objects, listener fan-out, status machine, reconnect + resync orchestration.|
|`core/grouse-core/src/{spine,transport,transcript,cache,unstable,roam}.rs`|The live connection / WebSocket transport / transcript store / cache store / shim / peer registry.|
|`core/grouse-roam-core/src/capi.rs`|C ABI for the roam transport (the pattern the desktop's planned C ABI mirrors).|
|`core/Cargo.toml`|The only Cargo workspace (members: `grouse-core`, `grouse-roam-core`, `grouse-unstable`). Release profile: `opt-level = "z"`, `codegen-units = 1`, `strip`, `lto`, `panic = "unwind"` (uniffi catch_unwind must turn panics into foreign exceptions).|
|`Justfile`|Top-level build/test/scan entrypoints (see Development Commands).|
|`CONTRIBUTING.md`|Contribution flow (one-core-first; devcontainer; per-platform extras; CI).|
|`scripts/build-android-libs.sh`, `scripts/dev-env.sh`|Android native/bindings build; devcontainer wrapper.|
|`clients/desktop/AGENTS.md`|Complete desktop contract: architecture, C++ conventions, protocol gotchas, QA.|
|`design/tokens.json`, `design/design-language.md`|Shared design tokens + rationale.|

## Runtime/Tooling Preferences

- **Rust core** (`core/`): `cargo`; std test harness; tokio runtime. No pinned
  toolchain — CI runs `dtolnay/rust-toolchain@stable` (+ clippy); devcontainer
  pins 1.97.1. Workspace release profile is size-optimized (`opt-level "z"`,
  `lto`, `strip`) because the cdylibs ship inside the APK.
- **Android**: Gradle wrapper 8.9, AGP 8.5.2, Kotlin 2.0.20 (Compose +
  serialization), JDK 17, compileSdk/targetSdk 34, minSdk 26. App id
  `id.gauvin.grouse`. Native libs via `cargo ndk` (targets `aarch64-linux-android`
  + `x86_64-linux-android`) + `uniffi-bindgen` (`cargo install uniffi --features
  cli`); JNA 5.14.0@aar. Note: the desktop/roam `.so` is bundled only on desktop;
  on Android only `libgrouse_core.so` ships (roam resolves to it).
- **Desktop**: CMake ≥ 3.16, C++17, Qt6 (Quick/Qml/QuickControls2/WebSockets/
  Widgets/DBus) + KF6 (Kirigami/CoreAddons) + ECM; built in a `kde-build`
  distrobox (Fedora) or the org.kde.Platform 6.10 Flatpak SDK. No CMake linking
  against cargo-built libs — `RoamTransport` dlopens `libgrouse_roam_core.so`.
- **Devcontainer**: Rocky Linux 9 (matching the home-server deployment OS),
  scoped to core + CLI only — no Android NDK or Qt inside. Run via
  `scripts/dev-env.sh` (rootless podman, `--userns=keep-id`, UID/GID build args).
- **Secrets**: gitleaks must stay clean on every commit.
- **Endpoints/config**: no secrets committed; local endpoint configuration is
  developer-specific. There is no `GOOSE_SERVER` env var.

## Testing & QA

### Rust core

- Framework: **std test only** — bare `#[test]`, no `tokio::test`, `rstest`, or
  proptest anywhere. Async spine tests drive a `FakeServer` on its own
  tokio-runtime thread via ports + recorded frames (std `mpsc`).
- Location: inline `#[cfg(test)] mod tests` in library sources
  (`grouse-core/src/cache.rs`, `roam.rs`, `transcript.rs`, `transport.rs`,
  `unstable.rs`; `grouse-roam-core/src/lib.rs`) plus integration dirs:
  - `core/grouse-core/tests/spine.rs` — end-to-end against an in-process
    **scripted WebSocket server** (the "Rust twin" of the desktop's
    `tests/fakeserver.h`). It records every frame including the `X-Secret-Key`.
  - `core/grouse-roam-core/tests/core.rs` (identity/card units) and
    `tests/e2e.rs` — **`#[ignore = "requires a live serve --roam host"]`**;
    run with `cargo test --test e2e -- --ignored --nocapture`.
- **`grouse-unstable` has no tests.**
- Run: `cargo test --manifest-path core/Cargo.toml`. This is the only test suite
  CI runs (in `core.yml`).

### Desktop

- Qt Test (`QTEST_MAIN`/`QTEST_GUILESS_MAIN`) + QtQuickTest for QML; registered
  in `clients/desktop/tests/CMakeLists.txt` (built when `BUILD_TESTING` ON).
- Suites: `tst_markdown`, `tst_messagelistmodel`, `tst_sessionlistmodel`,
  `tst_acpclient` (wire tests vs `tests/fakeserver.h` — pin the protocol
  gotchas), `tst_manager` (integration with fake goose), `tst_roamcodec`,
  `tst_roamtransport` (dlopens the real `.so`; QSKIP unless `GROUSE_ROAM_CORE`
  points at it), `tst_roamlistmodel`, `tst_qml` (ChartBubble + ComboBox).
- Run locally: `cd clients/desktop/build && ctest --output-on-failure`, or one
  suite with `ctest -R tst_acpclient --verbose`. **CI runs none of these.**
- Manual QA on top: `qmllint` (static QML), `xvfb-run` page-load (silence =
  success — offscreen won't catch page-internal type errors), and a live
  `goose serve` smoke test of the changed path.

### Android

- JUnit4 JVM unit tests only (`clients/android/app/src/test/`), deliberately
  framework-free: "parsers and wire framing only — no Android framework, no
  Robolectric". No `src/androidTest/`, no Compose UI tests. `mockwebserver` is
  declared but currently unused.
- Run: `./gradlew testDebugUnitTest`. Not wired into CI.

### Coverage

No coverage tooling is configured anywhere (no tarpaulin/llvm-cov/JaCoCo), and no
coverage expectations are stated. Tests are the repo's guardrails for the protocol
gotchas and pure logic; keep them so — do not add coverage gates.

### Known stale scaffolding (do not fixate on these)

- `clients/cli/` is a README placeholder; `.devcontainer/devcontainer.json`
  references `clients/cli/Cargo.toml`, which does not exist yet.
- Android CI does not regenerate native libs — the `.so` + bindings are committed;
  regenerate with `just android-libs` after core changes.
- The desktop README's "Not yet" list is stale.
