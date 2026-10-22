# Changelog

All notable changes to Grouse are recorded here. The format is based on
[Keep a Changelog](https://keepachangelog.com/), and this project adheres to
[Semantic Versioning](https://semver.org/).

## [0.2.14] - 2026-10-21

### Added

- **One session pipeline for both transports.** `session.rs` now owns the ACP
  parameter builders, wire→item translation, the update dispatch, one
  transcript store per chat, the cache paint policy and the turn lifecycle.
  The WebSocket and roam transports contribute only dial/framing, reconnect
  policy, browse lists and the `roam:` id namespace.
- **Roam chats get the session features that were main-only.** MCP Apps
  hydrate, consecutive tool calls collapse into groups, a run of tools gets a
  model-written label, recipe parameter prompts and elicitation requests work,
  turn-end `status_message`/`message_usage` arrive, and multiple permission
  prompts can be answered concurrently.
- **Background chats keep their state without stealing the paint.** A chat
  updating while another is displayed accumulates silently, shows the green
  dot, and is painted once when opened.

### Changed

- Android's "reload chat" calls the core's single refresh path; the client no
  longer picks a wire. The desktop needed no change — the C ABI is unchanged.
- The hidden summarizer runs one session per connection, using the same
  hidden-session marker for both transports.

### Fixed

- **Reload chat on a roam session ignored `force`.** The refresh now resolves
  the session's owning connection and replays when asked, instead of always
  probing the main connection's bound session.
- **Steer could target the wrong chat.** It resolved the main connection's
  bound session even when the displayed chat belonged to a peer; it now steers
  the displayed session on its owner.
- **A prompt sent while a peer was re-dialing was lost.** Prompts queue per
  owner until that connection can accept them, then flush in order.
- **Remote-change resync never ran for peer sessions.** The debounced
  `session_info_update` probe → in-place `session/load` replay → re-probe now
  runs on the session's own connection.
- **One chat's failed turn marked every chat's cache incomplete.** Incomplete
  tracking is per session, so a dropped turn no longer forces unrelated chats
  to full-replay.
- **A roam turn could wedge the app busy forever.** A prompt sent while the
  peer was re-dialing has no connection to go out on, and `rpc()` returned
  without ending the turn — so `busy` stayed set with nothing left to clear it.
  Every `session/prompt` exit now ends the turn, so the client shows an error
  instead of a permanent "Grouse is thinking…".
- **Peer titles never reached the drawer.** A peer's live `session_info_update`
  carried the raw server id, which matches no `roam:<label>:<id>` chat, so
  auto-generated titles and renames only appeared after a full session re-list.
  They now carry the prefixed id.
- **Android: a send could be lost, or a queued one stranded.** The send gate was
  the *main* socket's readiness, so a roam send went out on a peer that was
  re-dialing (and died) and a send made while the core was bound elsewhere was
  silently rejected. It is now the target chat's own wire plus the core's bound
  session, and anything that cannot go out is queued — with a flush on the
  peer's `ready` to match the main Ready path. A prompt typed before any chat
  was bound is re-keyed onto the session that binds, and switching chats
  (roam included) clears the previous chat's busy/compacting state.
- **Roam transcript rows are keyed by the server's message id.** A replayed
  turn no longer folds distinct messages together because their text matched.
- **A roam prompt is echoed locally.** The question and the turn boundary
  appear before the server replies, instead of after the first chunk.

## [0.2.0] - 2026-10-12

### Fixed

- **Transcript reliability.** A cached transcript is trusted only when it is
  *settled*. A store saved while a turn was in flight — or after a mid-stream
  drop truncated one — is replayed in full on open instead of being shown
  as-is, so reopening after a drop no longer loses messages or tool calls.
- **Empty "Goose" bubbles.** goose emits the assistant's text as a
  whitespace-only message between tool calls; it no longer paints a blank
  bubble.
- **Duplicated rows after a reconnect.** A reconnect replays the whole session,
  and goose re-assigns tool-call ids on every replay. The replayed rows now
  *replace* the live transcript instead of appending onto it, so messages and
  tool chips are no longer duplicated and re-assigned ids no longer collide.
- Providers: every picker lists only providers you have actually configured
  (credentials set), not goose's broader "usable" set.
- Desktop: `build-flatpak.sh` checks for `flatpak-builder` inside the build
  container, not on the host.

### Added

- **Reload chat** — a pull-up handle on Android and a header button on desktop
  force a full replay of the current session (the escape hatch after a drop).
- **Projects at creation** — a new chat is filed under a project via
  `_meta.projectId` on `session/new`, so project chats are correct from the
  first message.
- MCP Apps render inline on desktop (QtWebEngine base app) and dock on Android,
  gated on the client capability goose requires.
- Roam: opt-in peer transport (Settings toggle), a rich per-peer transcript, and
  session metadata (snippet + message count) in the drawer.
- Desktop: UnifiedPush receive-only notifications; steering a running turn
  instead of queueing it; a thinking-effort picker beside the model.
- Recipes: master–detail layout with a cron picker and model/provider dropdowns.
- Projects and skills: editable instructions.
- Selectable message bubbles with on-demand generation stats.

### Changed

- Transcripts are now the core's **item stream** end to end on both desktop and
  Android — windowed, keyed by item id, and updated incrementally. The legacy
  flat transcript/stream channels are gone.
- Desktop: the Qt QML disk cache is disabled — it pinned old bytecode across
  in-place Flatpak updates.
- CI: CodeQL runs via advanced setup.

## [0.1] - 2026-09-12

Initial release: Android (Kotlin/Compose) and Linux desktop (Qt6/KF6 Kirigami)
clients for a self-hosted `goose serve`, over ACP (JSON-RPC 2.0) on WebSocket,
sharing one Rust core (`libgrouse_core.so`) and the roam peer transport.

[0.2.14]: https://github.com/ccgauvin94/grouse/compare/v0.2.10...v0.2.14
[0.2.0]: https://github.com/ccgauvin94/grouse/compare/v0.1...v0.2.0
[0.1]: https://github.com/ccgauvin94/grouse/releases/tag/v0.1
