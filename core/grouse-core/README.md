# grouse-core

The stable ACP client crate, built on the official `agent-client-protocol` SDK.

Scope: the initialize handshake, session lifecycle (`session/new`,
`session/load`, `session/delete`), prompt/streaming, tools, permissions, config,
and the standard `session/update` tags.

Transport: owns the WebSocket transport (via `agent-client-protocol-http`),
including a custom `ConnectTo` that sends the `X-Secret-Key` header and applies
a trust-all TLS configuration (the server uses a self-signed cert).
