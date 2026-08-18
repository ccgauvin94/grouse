# Grouse — Architecture & Contract

This file is the single source of truth for how the grouse monorepo is organized
and how its pieces talk to each other. Read it before adding a platform, changing
the wire, or touching the core's public API.

## One Rust core, many thin native UIs

The repository has exactly one implementation of the grouse client logic: two
Rust crates in `core/`, built on the official goose GDK — the official ACP SDK
(`agent-client-protocol`, Client role, with the `agent-client-protocol-http`
WebSocket transport) — as a daemon-based integration, in the same family as Buzz,
Codex, and Claude Code. Every platform is a thin, native UI that drives that core
and does not reimplement client logic.

A UI is native per platform. There is no cross-platform UI framework, no shared
web view, and no embedded browser shell.

`core/` holds three crates — two split along the stable/unstable line, plus the
roam transport:

- **`grouse-core`** — the stable ACP surface on the official
  `agent-client-protocol` SDK: the initialize handshake, session lifecycle,
  prompt/streaming, tools, permissions, config, and the standard `session/update`
  tags. It owns the WebSocket transport, including a custom `ConnectTo` that
  sends the `X-Secret-Key` header and applies a trust-all TLS configuration (the
  server's self-signed cert).
- **`grouse-roam-core`** — the roam transport: iroh peer dialing, the device
  identity, connection cards, and the authenticated byte duplex ACP is framed
  over. It exposes its own uniffi Kotlin bindings and a C ABI (`src/capi.rs`),
  so a UI can pair and dial without going through `grouse-core`.
- **`grouse-unstable`** — the goose-fork compatibility shim: every
  `_goose/unstable/*` method, plus the SDK's `unstable_elicitation` feature and
  the goose-custom `status_message`/`message_usage` notifications. Marked for
  retirement as the GDK absorbs each feature upstream.

Together these crates own the ACP session state machine, the roam transport, and
everything else that must be byte-identical across platforms. The UIs own:
rendering, input, platform lifecycle, and platform services (notifications,
tiles, biometrics, tray integration).

## The uniffi contract is the API boundary

The core's public surface is defined once, as a uniffi interface. That interface
is the contract every foreign-language UI consumes; nothing else may reach into
the core.

- **Android (Kotlin/Compose)** consumes the uniffi-generated Kotlin bindings.
- **macOS (SwiftUI)** will consume the uniffi-generated Swift bindings.
- **Linux desktop (Qt6/KF6 Kirigami)** will consume a hand-written C ABI on the
  core (`extern "C"`), mirroring the `src/capi.rs` pattern proven in
  `core/grouse-roam-core`; Qt dlopens/links that ABI. **Not yet true — see the
  deviation below.**
- **CLI (Rust TUI)** consumes the crates directly as Rust libraries — no FFI.

Rule: a change to shared behavior changes the uniffi interface (and the C ABI)
first, regenerates the bindings, then adapts each UI. Never fork client logic
into a UI to avoid changing the contract.

### Known deviation: the desktop carries its own client

`clients/desktop/` predates the Rust core and still implements the client logic
itself, in C++: `src/acpclient.{h,cpp}` is a second ACP wire client,
`src/manager.{h,cpp}` a second session/chat state machine and cache (the format
`core/grouse-core/src/cache.rs` mirrors), and `src/{websocket,roam}transport.*`
second transports. It reaches the core only by dlopening
`libgrouse_roam_core.so` for the iroh dial.

This is the exact fork the rule above forbids, and it is a migration debt, not a
sanctioned pattern: two implementations of the session state machine will drift.
The path out is to give `grouse-core` the C ABI the table promises, then delete
`acpclient`, `manager`, and the transports in favour of it. Until that lands,
protocol fixes must be applied to BOTH the Rust core and the desktop's C++, and
`clients/desktop/AGENTS.md` documents the C++ side.

## Per-platform native toolkits

|Platform|Toolkit|Binding|
|---|---|---|
|Linux desktop|Qt6 + KF6 Kirigami (QML/C++)|C ABI (planned; today: own C++ client)|
|Android|Kotlin + Jetpack Compose|uniffi Kotlin|
|CLI|Rust TUI|direct crate dependency|
|macOS (later)|SwiftUI|uniffi Swift|
|Web|deferred — not a native UI|—|

Web is explicitly out of scope for now: it is not a native UI, and adding it
would break the one-core/many-thin-native-UIs model.

## Design language

A unified design language — shared as platform-agnostic design tokens — lives in
`design/` (`tokens.json` + `design-language.md`). That directory is the single
source of truth for color, typography, spacing, and motion; each UI renders the
tokens in its own native toolkit. `design/` is owned by the design-language
workstream, not by the platform directories.

## Protocol notes that matter

The server is `goose serve` (upstream goose, now held by the Linux Foundation).
The wire is **ACP (JSON-RPC 2.0) over WebSocket**. These are the details that
bite; keep them in mind when extending the core.

- **`_goose/unstable/*` lives in `grouse-unstable`, a compatibility shim — not
  the core's identity.** The recipes/schedules/skills/steer/remote/roam features
  behind these methods come from a goose fork; the official goose GDK is absorbing
  them upstream. `grouse-unstable` carries the shim so existing servers keep
  working, but new work should prefer the GDK surface, and the crate should be
  retired as the GDK lands each feature. The set in use includes `session/steer`,
  `session/export`, `session/rename`, `session/archive`, `session/unarchive`,
  `session/info`, `session/recipe/request-params`, `tools/list`,
  `sources/list|create|delete|update` (with `type: project|skill`),
  `recipes/list`, `schedules/list`, `config/extensions/list|set-enabled|add`,
  and `session/extensions/list|add`.
- **`session_info_update`** is the server's notification that a session changed —
  including changes made by another client. It carries `_meta.goose.activeRunId`,
  the live run id required to steer (`session/steer` sends `expectedRunId`).
  Clients debounce `session_info_update` bursts and re-probe via
  `_goose/unstable/session/info` rather than doing a full resync.
- **roam** is ACP over iroh direct peer streams (`goose serve --roam`): the same
  JSON-RPC, newline-framed (ACP's ByteStreams framing, identical to goose on
  stdio), over an iroh connection between two peers instead of a WebSocket to a
  host. Roam peers are parallel connections, not replacements for the main
  connection. The transport is `core/grouse-roam-core` — a workspace member, not
  an external dependency — exposing both uniffi Kotlin and a C ABI.
- **camelCase vs snake_case**: `recipes/*` and `schedules/*` params are
  snake_case (`cron_schedule`, `file_path`); nearly everything else is camelCase.
  A misspelled param is silently dropped, not rejected.
- **Sessions are typed by `_meta.client`.** `session/new` without it creates an
  `acp` session, which desktop and CLI never list. Each client sets its own
  `_meta.client` value on `session/new`.
- **`session/load` rewrites `working_dir` from the cwd you send.** Never guess a
  cwd — carry the session's real cwd from `session/list` or ask the server.
- **Recipe parameter requests** hard-fail unless the client declares
  `clientCapabilities._meta.goose.recipeParameterRequests: true` at initialize.
