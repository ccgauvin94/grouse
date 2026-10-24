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

One implementation of client logic exists: **the Rust core in `core/`** (three
crates — see below), built on the official goose GDK (the
`agent-client-protocol` SDK, Client role; the WebSocket transport is this repo's
own `ConnectTo` implementation, because the stock `agent-client-protocol-http`
client cannot send the `X-Secret-Key` header or verify TLS the way grouse needs).
Every platform is a thin, native UI that drives that core and does not reimplement
client logic. There is no cross-platform UI framework and no shared app view —
MCP App templates are the one HTML surface, rendered in a WebView (Android,
desktop with QtWebEngine) or the user's browser (desktop loopback bridge), never
as app chrome.

## Architecture & Data Flow

### One Rust core, many thin native UIs

`core/` holds three crates, split along the stable/unstable line plus the roam
transport:

- **`grouse-core`** — the stable ACP surface on the official
  `agent-client-protocol` SDK: initialize handshake, session lifecycle,
  prompt/streaming, tools, permissions, config, and the standard `session/update`
  tags. `session.rs` is the single session layer shared by both transports —
  ACP parameter builders, wire→item translators, the `PaintPlan` cache policy,
  the `SessionHub` store + display gate, the `session/update` dispatch, and the
  one `ChatEndpoint` transmit surface. The transports own only their wire:
  `transport.rs` + `spine.rs` for the WebSocket (`ConnectTo` sending the
  `X-Secret-Key` header; WebPKI-verifying TLS with an `accept_invalid_certs`
  opt-out) and `roam.rs` for iroh peers. It also carries the hand-written C ABI
  (`capi.rs`) and the client-shared policy modules (`notify.rs`, `speak.rs`,
  `summarize.rs`).
- **`grouse-roam-core`** — the roam transport: iroh peer dialing, device identity,
  connection cards, and the authenticated byte duplex ACP is framed over. An rlib
  dependency of `grouse-core` (the roam uniffi package is exported from
  `libgrouse_core.so`), so no separate `.so` ships; its own `grc_*` C ABI
  (`src/capi.rs`) is currently unused in-tree — the desktop dials roam through the
  core.
- **`grouse-unstable`** — the retirement boundary for the goose-fork
  `_goose/unstable/*` shim: it re-exports `GrouseUnstable` (implemented in
  `grouse-core/src/unstable.rs` + the session mutators in `lib.rs`) and pins the
  disconnected no-op contract. The shim also carries the SDK's
  `unstable_elicitation` feature and the goose-custom
  `status_message`/`message_usage` notifications, and shrinks as the GDK absorbs
  each feature upstream.

### The uniffi contract is the API boundary

The core's public surface is defined once — as the uniffi interface — and exposed
twice: the generated bindings, and a hand-written C ABI (`capi.rs`) that mirrors
the same surface for the desktop. Nothing else may reach into the core.

- **Android (Kotlin/Compose)** consumes the uniffi-generated Kotlin bindings.
- **macOS (SwiftUI, later)** will consume the uniffi-generated Swift bindings.
- **Linux desktop (Qt6/KF6 Kirigami)** consumes the hand-written C ABI
  (`extern "C"`, `core/grouse-core/src/capi.rs`), dlopened as `libgrouse_core.so`
  by `src/corebridge.cpp`.
- **CLI (Rust TUI, not yet written)** consumes the crates directly — no FFI.

**Rule: a change to shared behavior changes the uniffi interface (and the C ABI)
first, regenerates the bindings, then adapts each UI.** Never fork client logic
into a UI to avoid changing the contract.

### The desktop is a thin client over the C ABI

`clients/desktop/` no longer carries a client. `src/corebridge.{h,cpp}` dlopens
`libgrouse_core.so` and resolves every `grouse_*` symbol; the C ABI exposes the
whole core surface (stable + unstable) as JSON C-strings over a
`GrouseCoreListener` callback table, and `src/manager.{h,cpp}` is a thin QML
facade (`Mgr`) over it. There is no C++ ACP client, transport, or transcript
store in the tree. **A protocol fix is made in `core/` only** — a new intent or
event lands in `capi.rs` (and `corebridge`) alongside the uniffi export.
`clients/desktop/AGENTS.md` documents the C++/QML side.

Core modules that are NOT ACP plumbing, and are shared by every client:
`notify.rs` (push envelope + show/don't-show policy, CONTRACT §5.5),
`speak.rs` (markdown → sentence-sized TTS chunks), `summarize.rs` (model-written
one-line labels for a run of tool calls), `capi.rs` (the C ABI above).

### Per-platform native toolkits

|Platform|Toolkit|Binding|
|---|---|---|
|Linux desktop|Qt6 + KF6 Kirigami (QML/C++)|C ABI (`grouse_*`, dlopened `libgrouse_core.so`)|
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
  `send_prompt`/`set_config_option`/tool queries queue per session until that
  chat's owner can accept them, then flush in order (`Core::owner_for` →
  `ChatEndpoint`). Turns are concurrent **per session** (`CoreState::in_flight`
  is a set of display ids): a reply can stream in a roam chat while another
  runs on a different peer or the main wire, and a completion clears only its
  own chat. Reconnect uses exponential backoff
  (500ms·2ⁿ, cap 15s, 6 attempts, reset on Ready; none on explicit `disconnect`);
  a roam peer re-dials on the same curve with its own budget. Remote-change
  resync debounces `session_info_update` (1.5s) → probes
  `_goose/unstable/session/info` on the session's owner → in-place
  `session/load` replay → re-probes at 8s ×3, for main and peer sessions alike.
- **One session pipeline, two transports.** `core/grouse-core/src/session.rs`
  owns the ACP params, wire payload translation, cache paint policy, one
  `TranscriptStore` per display session id (`SessionHub`), and the single
  `session/update` dispatch. `spine::Conn` and `roam::RoamPeer` contribute only
  dial/framing, reconnect policy, browse-mode session list, and id namespace
  (`roam:`); a background session accumulates silently (green dot) until it is
  displayed.
- **The transcript crosses the boundary as rich items**, never as a republished
  list: `on_item` carries `Upsert`/`AppendText`/`AppendOutput`/`Remove`/`Reset`/
  `Window` ops keyed by item id, the core windows what it paints (newest ~60) and
  serves older items on request. See `docs/TRANSCRIPT_MODEL.md`.
- **Push is receive-only.** The repo ships no sender; the core only decodes the
  envelope and decides whether to show (one policy, `notify.rs`), and each client
  only registers/receives (Android's UnifiedPush connector, the desktop's
  `pushclient.cpp`). See `docs/NOTIFICATIONS.md`.

### Protocol notes that matter

The server is `goose serve`; the wire is ACP over WebSocket. These details bite;
keep them when extending the core.

- **`_goose/unstable/*` lives in `grouse-unstable`, a compatibility shim — not
  the core's identity.** Those methods come from a goose fork; the GDK is
  absorbing them upstream. Prefer the GDK surface for new work. The shim's
  methods are implemented in `grouse-core/src/unstable.rs` plus the session
  mutators in `lib.rs`; the `grouse-unstable` crate is the retirement boundary
  (re-export + the disconnected no-op contract tests). In use:
  `session/steer|export|info|update|rename|archive|unarchive`,
  `session/project/update`, `session/working-dir/update`,
  `session/recipe/request-params`, `session/conversation/truncate`,
  `session/extensions/list|add|remove`, `tools/list|call`,
  `sources/list|create|update|delete` (`type: project|skill`),
  `recipes/list|save|delete|schedule`, `schedules/list|update|pause|run-now`,
  `config/read|upsert`, `config/extensions/list|set-enabled|add`,
  `providers/list`, `providers/config/read`, `providers/supported-models/list`,
  `resources/read`, `dictation/config|transcribe`. `session/delete` is the one
  plain (non-`_goose`) method the clients use for deletion.
- **`session_info_update`** is the server's notification that a session changed —
  including changes by another client. It carries `_meta.goose.activeRunId`, the
  live run id required to steer (`session/steer` sends `expectedRunId`). Debounce
  bursts and re-probe via `_goose/unstable/session/info` rather than full resync.
- **A cached transcript is trusted only when it is *settled*.** A store saved
  while a turn is in flight (or after it ended in error because the wire dropped
  mid-stream) is written with an empty `updatedAt`, so it can never suppress the
  replay that would complete it; a settled-but-stale cache merges a bounded tail,
  an incomplete one full-replays. `refresh_session(id, mode)` is the clients'
  "reload chat" action: `Quick` lets freshness decide (so an unchanged chat
  reloads nothing, and a dropped turn — whose cache carries an empty stamp —
  still full-replays), `Full` forces the replay. Either way the replay is
  ingested silently and painted once, newest window first (grouse#18).
- **A failed open never substitutes a session** (grouse#19). `session/load` a
  server rejects (stale/archived) reports `on_session_open_failed` and leaves the
  connection with no session bound; a fresh empty session silently becoming the
  active chat is what made a tapped thread open blank.
- **roam** is ACP over iroh direct peer streams (`goose serve --roam`): same
  JSON-RPC, newline-framed (ACP ByteStreams framing, identical to goose on stdio),
  over an iroh connection between two peers instead of a WebSocket to a host.
  **Roam peers are parallel connections, not replacements** for the main
  connection. Sessions bound to a peer use the `roam:<peer>:<id>` id prefix for
  sidebar grouping; session-bound unstable RPCs route to the owning peer. A peer
  runs the SAME `session.rs` pipeline (params, dispatch, store, cache policy,
  `ChatEndpoint`) and the same SDK handler table, so turn-end custom
  notifications (`status_message`, `message_usage`) and recipe/elicitation
  requests behave identically there; only dial/framing, the reconnect budget,
  the browse-mode list and the id namespace are peer-specific.
- **camelCase vs snake_case**: `recipes/*` and `schedules/*` params are snake_case
  (`cron_schedule`, `file_path`); nearly everything else is camelCase. **A
  misspelled param is silently dropped, not rejected.**
- **Sessions are typed by `_meta.client`.** `session/new` without it creates an
  `acp` session, which desktop and CLI never list. Each client sets its own
  `_meta.client` value on `session/new` (e.g. `"grouse-desktop"`, `"grouse"`,
  `"grouse-cli"`). `_meta.projectId` on `session/new` is persisted to goose's
  `project_id` column at creation — the stock-goose create-time project bridge
  (grouse#5 P0); after creation only the doomed
  `_goose/unstable/session/project/update` can change it.
- **`session/load` rewrites `working_dir` from the cwd you send.** Never guess a
  cwd — carry the session's real cwd from `session/list` or ask the server
  (core resolves it via `_goose/unstable/session/info`).
- **`session/new` needs an absolute cwd** that exists inside the goose container;
  goose has no default. It is asked for at connect time.
- **Recipe parameter requests** hard-fail unless the client declares
  `clientCapabilities._meta.goose.recipeParameterRequests: true` at initialize.
- **MCP Apps** hydrate only if the client declares
  `clientCapabilities._meta.goose.mcpHostCapabilities.extensions["io.modelcontextprotocol/ui"]`
  at initialize; without it goose strips the tool's `_meta.goose.mcpApp` and every app
  degrades to a plain tool chip. The server also only hydrates apps whose TOOL DEFINITION
  carries `_meta.ui.resourceUri` (not merely the call result).
- **Trust boundary** for `content`/`output`/`appHtml`/chart specs: the core
  never escapes server-provided content; every UI must treat it as untrusted at
  its rendering surface and sanitize or sandbox any HTML/JS renderer. See
  `core/CONTRACT.md` §8.
- **WebSocket TLS verifies by default** (WebPKI chain + hostname; the public
  host serves a valid Let's Encrypt cert). `ServerConfig.accept_invalid_certs`
  is the documented trust-all opt-out for self-signed tailnet hosts, and
  `ca_cert_pem` trusts a private CA. There is no per-client TLS implementation
  left — both clients pass `ServerConfig` through their binding, so the desktop's
  remaining parity gap is a hardcoded `accept_invalid_certs = true` in
  `Manager::serverConfigJson()` (`clients/desktop/src/manager.cpp`), while
  Android has the user-facing verify toggle.

## Key Directories

|Path|Purpose|
|---|---|
|`core/`|Rust workspace (`core/Cargo.toml` is the workspace root — the repo root has no Cargo.toml). The three crates above. `core/CONTRACT.md` = the uniffi interface spec; `core/grouse-core/INTERNAL.md` = pinned internal module seams + threading model.|
|`core/grouse-core/src/`|`lib.rs` (uniffi surface + Core/GrouseUnstable objects), `capi.rs` (the `grouse_*` C ABI for the desktop), `transport.rs` (WsTransport), `session.rs` (the single session layer: ACP params, wire→item translation, `PaintPlan`, `SessionHub` + display gate, `DispatchHost`/`dispatch`, `ChatEndpoint`, `connect_client`), `spine.rs` (main connection: dial + handshake + reconnect + server requests), `transcript.rs` (TranscriptStore: chunks → items, the 60-item window), `cache.rs` (CacheStore: per-session transcript + tools), `unstable.rs` (the `_goose/unstable/*` shim), `roam.rs` (peer registry: iroh transport, supervisor, id namespace), `notify.rs` (push policy), `speak.rs` (TTS chunking), `summarize.rs` (tool-run labels). Bindings staged in `bindings/kotlin/`.|
|`core/grouse-roam-core/src/`|iroh transport (`lib.rs`), C ABI (`capi.rs`, `grc_*` symbols). An rlib dependency of `grouse-core` — no separate `.so` ships.|
|`clients/desktop/`|Qt6/KF6 Kirigami thin client: QML + `corebridge` (dlopen of `libgrouse_core.so`, the only wire path), plus the host-side KRunner plugin (its own `AGENTS.md` is the C++-side contract).|
|`clients/android/`|Kotlin/Compose client (app) + `grouse-core-aar` module carrying the uniffi bindings and native `.so`s.|
|`clients/cli/`|Rust TUI — **README placeholder only, no `Cargo.toml` yet.**|
|`docs/`|`TRANSCRIPT_MODEL.md` (the item stream the core emits) and `NOTIFICATIONS.md` (push envelope + policy).|
|`design/`|Shared design tokens (`tokens.json` + `design-language.md`), owned by the design-language workstream, not platform dirs.|
|`scripts/`|`android-env.sh` (runs a command inside the Android build image: SDK/NDK + the JDK), `build-android-libs.sh` (Android native libs + bindings), `publish-apk.sh` (verify a release-signed APK and publish it to Nextcloud), `dev-env.sh` (devcontainer wrapper on podman).|
|`.github/workflows/`|CI: `core.yml`, `android.yml`, `codeql.yml`, `flatpak.yml`, `secrets.yml`; `release.yml` (tag-triggered APK + Flatpak → GitHub Release).|
|`.devcontainer/`|Rocky Linux 9 dev container for the Rust core only (no Android NDK, no Qt).|

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
just android-image                                   # build the toolchain image (once)
just android-libs                                    # rust core -> .so + uniffi Kotlin
just android                                         # debug APK
just android-release                                 # signed release APK
just publish-apk <slug>                              # verify + upload to Nextcloud
```

Everything above runs inside the `grouse-android` image
(`.devcontainer/android/Containerfile`) via `scripts/android-env.sh`, because the
host has no NDK/Android SDK. Gradle + cargo caches live in `~/.cache/grouse/`, not
the repo; the `GROUSE_*` signing vars are forwarded from your shell and the
keystore is mounted read-only at its own path. Inside the container the commands
are the plain ones:

```sh
clients/android/gradlew -p clients/android assembleDebug        # debug APK
clients/android/gradlew -p clients/android testDebugUnitTest     # JVM unit tests
```

- **A core change is not in the APK until `just android-libs` runs.** The
  committed `libgrouse_core.so` is what ships, and the APK has no way to notice
  it is stale. Bindings are regenerated too — with uniffi CLI **0.32.1**, the
  version the committed output was generated by (0.32.0 and 0.32.2 each rewrite
  it); a byte-identical diff afterwards means the public surface did not move.
- Outputs are staged into `clients/android/grouse-core-aar/src/main/{jniLibs,kotlin}`
  and `core/grouse-core/bindings/kotlin/` and MUST be committed. CI does NOT run
  this — it builds from the committed `.so` + bindings.
- `scripts/publish-apk.sh` refuses an APK signed by anything but the release key
  (SHA-256 `95b51fd4…dd84474e`) or one whose `versionCode` is not ahead of what is
  already published, then copies into `/mnt/data/nextcloud/colin/files` and runs
  `occ files:scan` on **that one file**.
  **Scan per-file, never `--path=/colin/files`.** The Nextcloud account also holds
  an intentional synced copy of a grouse source tree
  (`colin/files/Documents/Software/Grouse/grouse/`), Rust `target/` dirs included;
  a directory-wide scan walks tens of thousands of those files and sits in
  D-state for many minutes (measured: killed at 13). The sync is deliberate —
  do not "clean it up". Bump `versionCode` in `app/build.gradle.kts`
  before every sideloaded build — a same-code install is silently skipped.
- `clients/android/env.sh` is only for a host that HAS the SDK (it sets
  `JAVA_HOME`/`ANDROID_HOME`); the container does not need it.
- Release signing is env-driven (`GROUSE_KEYSTORE`/`GROUSE_STORE_PASSWORD`/
  `GROUSE_KEY_ALIAS`/`GROUSE_KEY_PASSWORD`, or `~/services/grouse/signing/grouse-release.env`
  on the build host), with debug-signing fallback when unset.

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
|`core.yml`|`cargo test` + `cargo clippy --all-targets -- -D warnings`, pinned toolchain 1.97.1|`Core / test`|
|`android.yml`|`./gradlew --no-daemon assembleDebug` + APK artifact|`Android / apk`|
|`codeql.yml`|CodeQL analysis of the Android build (push + weekly schedule)|CodeQL `analyze`|
|`flatpak.yml`|KRunner plugin + `libgrouse_core.so` host build (Fedora container), then Flatpak bundle (org.kde.Platform 6.10)|`Flatpak / host-components`, `Flatpak / flatpak`|
|`secrets.yml`|gitleaks|`Secrets scan / gitleaks`|
|`release.yml`|On a `v*` tag: signed release APK + desktop Flatpak, published as GitHub Release assets|— (not a PR check)|

**There are NO test jobs in CI for Android unit tests or desktop `ctest`** — run
those locally.

**Releases** are cut by pushing a version tag (`git tag v0.69 && git push origin
v0.69`). `release.yml` then builds the signed `assembleRelease` APK and the
Flatpak bundle and publishes them on the GitHub Release. Android signing reads
the same env vars as local builds (`GROUSE_KEYSTORE` + the three passwords); CI
prefers `GROUSE_KEYSTORE_BASE64` (+ the three password secrets) and falls back
to `GROUSE_DEBUG_KEYSTORE_B64` for a sideload-only, mutually-upgradeable
debug-signed APK. The APK is built from the **committed** native `.so` +
bindings, so run `just android-libs` and commit the result after any `core/`
change before tagging.

## Code Conventions & Common Patterns

### Rust core

- **Versioning/toolchain**: edition 2021, no `rust-toolchain.toml`, no pinned MSRV.
  CI pins stable 1.97.1 (`core.yml`, `components: clippy`) and the devcontainer
  pins the same `RUST_VERSION`; `uniffi = "0.32"` and `tokio = "1"` across
  members.
- **Threading**: one tokio runtime on a worker thread; all `#[uniffi::export]`
  intents are synchronous (enqueue + return). `connect()` is the sole exception
  (bounded blocking until initialized). All network I/O, reply dispatch, and
  `CoreListener` callbacks run on the runtime thread. Request/reply uses the
  SDK's `send_request(...).block_task().await` — there is NO global request-id
  table.
- **Error handling**: FFI-facing errors are `#[derive(Debug, uniffi::Error)]`
  enums (e.g. `RoamError::Message(String)`). `thiserror`/`anyhow` appear only
  transitively; the crates hand-write boundary errors. Main-wire slices use
  `Conn::rpc(...) -> Result<Value, AcpError>`; anything that must work on either
  transport goes through a `session::ChatEndpoint` (or `session::request`, which
  applies the shared timeout policy), never a transport directly (`INTERNAL.md`
  §pinned seams).
- **Module invariants**: `transcript.rs`, `cache.rs`, `notify.rs`, `speak.rs`,
  `summarize.rs` and `session.rs` do NO network of their own (and no OS access);
  `session.rs` reaches a server only through a `ChatEndpoint`. The
  `TranscriptStore` owns bubble shaping: chunks append into a bubble keyed by
  `(role, message_id)`; live output appends, the completion update replaces;
  replay chunks are gated by message id; consecutive tool calls collapse into a
  group. `CacheStore` is dumb I/O keyed by display session id — freshness (via
  `updatedAt`) is the caller's job (`session::plan_paint` / `Core::save_cache_for`).
  Protocol changes land in `session.rs` once, never once per transport.
- **The C ABI (`capi.rs`) is a second, hand-written face of the same core**: every
  symbol is `#[no_mangle] extern "C"`, `catch_unwind`-wrapped, and passes records
  as JSON C-strings that the consumer must copy inside the callback. A new intent
  or listener event lands in `capi.rs` and `clients/desktop/src/corebridge.cpp`
  in the same change — the desktop cannot reach the core any other way.
  `grouse_abi_version()` gates the listener table: the desktop refuses a library
  whose version does not match (a signature change would otherwise be a callback
  called with the wrong arity).
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

- **Shared behavior lands in the core, once.** A wire/protocol change is made in
  `grouse-core` — for session/ACP semantics specifically, in `session.rs`, the
  one place both transports share — reflected in `core/CONTRACT.md`, and reaches
  the UIs through the bindings — uniffi for Android (and macOS later) and the C
  ABI (`capi.rs` ↔ `clients/desktop/src/corebridge.cpp`) for the desktop. The
  desktop holds no protocol logic to mirror.
- Never hardcode a machine-specific path into a shared file.

## Important Files

|File|Role|
|---|---|
|`core/CONTRACT.md`|The uniffi interface spec — the API boundary. Read before touching the core's public surface.|
|`core/grouse-core/INTERNAL.md`|Pinned internal module seams, threading model, event fan-out rules.|
|`core/grouse-core/src/lib.rs`|uniffi surface: records/enums, `Core` + `GrouseUnstable` objects, listener fan-out, status machine, reconnect + resync orchestration.|
|`core/grouse-core/src/session.rs`|The single session layer both transports share: ACP parameter builders, timeouts, wire→item translators, `PaintPlan` cache policy, `SessionHub` + `SessionGate`, `DispatchHost`/`PrefixHost`/`dispatch`, `ChatEndpoint`, and the SDK client builder (`ClientHandlers`/`connect_client`). A protocol change lands here once.|
|`core/grouse-core/src/{spine,transport,transcript,cache,unstable,roam,notify,speak,summarize}.rs`|Main WebSocket connection (dial, handshake, reconnect, server requests) / WebSocket transport / transcript store / cache store / shim / peer registry (iroh dial, supervisor, `roam:` namespace) / push policy / TTS chunking / tool-run labels.|
|`core/grouse-core/src/capi.rs`|The `grouse_*` C ABI the desktop dlopens — the same core surface as uniffi, hand-written. Add a symbol here and to `corebridge.cpp` together.|
|`clients/desktop/src/corebridge.{h,cpp}`|The desktop's only wire path: resolve `grouse_*`, install `GrouseCoreListener`, marshal events onto the Qt main thread.|
|`core/grouse-roam-core/src/capi.rs`|C ABI for the roam transport (`grc_*` symbols) — the convention `capi.rs` above follows. No in-tree consumer today.|
|`core/Cargo.toml`|The only Cargo workspace (members: `grouse-core`, `grouse-roam-core`, `grouse-unstable`). Release profile: `opt-level = "z"`, `codegen-units = 1`, `strip`, `lto`, `panic = "unwind"` (uniffi catch_unwind must turn panics into foreign exceptions).|
|`docs/TRANSCRIPT_MODEL.md`, `docs/NOTIFICATIONS.md`|The core's item stream (ops, windowing, item ids) and the push envelope + notification policy.|
|`Justfile`|Top-level build/test/scan entrypoints (see Development Commands).|
|`CONTRIBUTING.md`|Contribution flow (one-core-first; devcontainer; per-platform extras; CI).|
|`scripts/build-android-libs.sh`, `scripts/dev-env.sh`|Android native/bindings build; devcontainer wrapper.|
|`clients/desktop/AGENTS.md`|Complete desktop contract: architecture, C++ conventions, protocol gotchas, QA.|
|`design/tokens.json`, `design/design-language.md`|Shared design tokens + rationale.|

## Runtime/Tooling Preferences

- **Rust core** (`core/`): `cargo`; std test harness; tokio runtime. No
  `rust-toolchain.toml`, but both CI and the devcontainer pin 1.97.1
  (`dtolnay/rust-toolchain@stable` with the explicit toolchain + clippy).
  Workspace release profile is size-optimized (`opt-level "z"`, `lto`, `strip`)
  because the cdylib ships inside the APK and the desktop bundle.
- **Android**: Gradle wrapper 8.9, AGP 8.5.2, Kotlin 2.0.20 (Compose +
  serialization), JDK 17, compileSdk/targetSdk 34, minSdk 26. App id
  `id.gauvin.grouse`. Native libs via `cargo ndk` (targets `aarch64-linux-android`
  + `x86_64-linux-android`) + `uniffi-bindgen` (`cargo install uniffi --features
  cli`); JNA 5.14.0@aar. **Exactly one `.so` is shipped anywhere:
  `libgrouse_core.so`** — `grouse-roam-core` is an rlib linked into it (the
  `grouse_core` uniffi package exports the roam symbols), and the desktop
  dlopens that same file.
- **Desktop**: CMake ≥ 3.16, C++17, Qt6 (Quick/Qml/QuickControls2/Widgets/DBus/
  Network; `WebEngineQuick` optional — MCP Apps render in the user's browser via
  the `AppBridge` loopback host when it is absent) + KF6 (Kirigami/CoreAddons) +
  ECM; built in a `kde-build` distrobox (Fedora) or the org.kde.Platform 6.10
  Flatpak SDK. No CMake linking against cargo-built libs — `CoreBridge` dlopens
  `libgrouse_core.so` (via `GROUSE_CORE`, the app image's `../lib`, or `/app/lib`).
- **Devcontainer**: Rocky Linux 9 (matching the home-server deployment OS),
  scoped to core only — no Android NDK or Qt inside. Run via
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
  (`grouse-core/src/{cache,notify,roam,speak,transcript,transport,unstable}.rs`;
  `grouse-roam-core/src/lib.rs`) plus integration dirs:
  - `core/grouse-core/tests/spine.rs` — end-to-end against an in-process
    **scripted WebSocket server** (`FakeServer`, its own tokio runtime thread,
    std `mpsc` for frames). It records every frame including the `X-Secret-Key`;
    it is the only wire-level protocol test in the repo.
  - `core/grouse-roam-core/tests/core.rs` (identity/card units) and
    `tests/e2e.rs` — **`#[ignore = "requires a live serve --roam host and
    relay"]`**; run with `cargo test --test e2e -- --ignored --nocapture`.
- **`grouse-unstable`** holds only the re-exports + the disconnected no-op
  contract tests; the shim's behavior is tested in `grouse-core`.
- Run: `cargo test --manifest-path core/Cargo.toml`. This is the only test suite
  CI runs (in `core.yml`).

### Desktop

- Qt Test (`QTEST_MAIN`/`QTEST_GUILESS_MAIN`) + QtQuickTest for QML; registered
  in `clients/desktop/tests/CMakeLists.txt` (built when `BUILD_TESTING` ON).
- Suites: `tst_markdown`, `tst_messagelistmodel` (deferred/coalesced model
  updates), `tst_sessionlistmodel` (grouping / collapse / cwdFor / no-op reset
  guard), `tst_roamlistmodel`, `tst_appbridge` (the loopback MCP-Apps host),
  `tst_manager` (intent mapping: `Mgr` Q_INVOKABLE → `CoreBridge` → the ABI),
  `tst_qml` (QtQuickTest: ChartBubble, ComboBox `valueRole`, RecipesDialog).
- Run locally: `cd clients/desktop/build && ctest --output-on-failure`, or one
  suite with `ctest -R tst_manager --verbose`. **CI runs none of these** — the
  desktop's protocol behavior is covered by `core/grouse-core/tests/spine.rs`.
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

- `clients/cli/` is a README placeholder (no `Cargo.toml`); the `just build`
  recipe deliberately builds only `core/`.
- Android CI does not regenerate native libs — the `.so` + bindings are committed;
  regenerate with `just android-libs` after core changes.
- The desktop README's "Not yet" list is stale (the features listed there exist;
  see `clients/desktop/AGENTS.md`).
