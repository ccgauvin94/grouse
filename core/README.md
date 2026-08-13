# grouse core

The Rust crates that implement all grouse client logic, built on the official
goose GDK — the official ACP SDK — as a daemon-based integration.

Two crates, one boundary:

- **`grouse-core`** — the stable ACP surface on the official
  `agent-client-protocol` SDK: the initialize handshake, session lifecycle,
  prompt/streaming, tools, permissions, config, and the standard `session/update`
  tags. It owns the WebSocket transport (via `agent-client-protocol-http`),
  including a custom `ConnectTo` that sends the `X-Secret-Key` header and applies
  a trust-all TLS configuration (the server's self-signed cert).
- **`grouse-unstable`** — the goose-fork compatibility shim: every
  `_goose/unstable/*` method, plus the SDK's `unstable_elicitation` feature and
  the goose-custom `status_message`/`message_usage` notifications. Marked for
  retirement as the GDK absorbs each feature upstream.

The public API surface is the uniffi contract (Kotlin/Swift bindings, plus the
C ABI the Qt desktop uses). The CLI depends on the crates directly.
