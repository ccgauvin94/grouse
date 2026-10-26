# Changelog

All notable changes to Grouse are recorded here. The format is based on
[Keep a Changelog](https://keepachangelog.com/), and this project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **Concurrent turns, one per chat.** A reply can stream in a roam chat while
  another runs on a different peer or on the main connection: in-flight state is
  keyed per session, each chat keeps its own queue, and a completion clears only
  its own chat. Switching to a running chat restores its spinner and steer key
  instead of showing it idle; a prompt queued in a background chat starts when
  that chat's turn ends, even while another chat is on screen.

### Fixed

- **A reopened chat showed an older turn's tool runs at the END of the chat.**
  Two loads of one session could run at once. A replay *appends* to whatever the
  store holds, so the second one's rows landed after the first one's, whatever
  part of the conversation each was at: the remote-change resync (`session/load`
  again, spawned onto the runtime) cleared the store and queued its FULL replay
  while the open's bounded-tail replay was still in flight — the open then
  painted the sliver that had landed, and the resync's rows streamed in live from
  the FIRST turn, so the newest rows on screen were the beginning of the
  conversation. Reproduced against a real session: after the race the store held
  `[the open's tail][the conversation again from its first prompt]` and the
  client's last row was a mid-conversation turn. Three changes, one per layer:
  the store now has a per-session load slot (`TranscriptStore::begin_load`), held
  for the whole paint-ingest-repaint span, so no second load can start mid-flight;
  the resync goes through the same shared lifecycle as an open
  (`session::load_with_plan`) instead of clearing the store and requesting a
  bare load — so it also holds its rows and repaints ONCE instead of replaying
  1300 messages onto the UI; and the "discard replayed rows" window a
  current-cache open uses is now one per-session decision in the core
  (`DispatchHost::begin_replay(key, suppress)` → `accepts_chat`) instead of a flag
  on the main connection with the peer hardcoding "accept everything" — which had
  made every peer open with a current cache append a SECOND copy of the last turn
  (measured: +64 rows per open).
- **A chart or MCP-app row could jump to the top of the chat.** Those two item
  projections never carried `seq`, so the row was emitted at position 0 and an
  ordered client put it above the question, not where the call happened.
- **A roam chat's recent prompts appeared twice after reopening: inline and
  again at the bottom.** Two causes, both in the reopen path. First, the peer
  wrote its own copy of the `session/load` replay lifecycle and had lost two
  steps of it — no held emissions and no merge close — so the replayed tail was
  appended onto a cache paint whose stale suffix the merge anchor had already
  truncated *in the store*. The client was never told about the truncation, so it
  kept drawing the old rows inline while the replayed copies landed after them.
  The lifecycle now lives in one place (`session::replay_session`) and both wires
  call it, so the peer cannot drift again. Second, a cached user row had no
  server identity at all: the item's `id` is the client's own `@n` key, so a
  replayed prompt could only be matched by its TEXT — a merge could not anchor on
  a prompt, and one failed match stayed at the head of the echo queue and blocked
  every later prompt from matching too. `Item::message_id` now carries the
  server's id (cache v8; a v7 file repaints once), and the echo queue is keyed by
  row uid instead of a bubble index that a paint or an anchor invalidates.
- **A roam turn could stay "thinking" until Stop was pressed.** A turn ended
  only on its `session/prompt` response, so when that response reached the app
  late or not at all the chat stayed busy and its composer queued every later
  send behind a turn nothing could finish. goose's `activeRunId: null`
  notification — which it emits *before* the response — is now a first-class
  end-of-turn signal, and whichever of the two arrives first ends the turn once
  (a late second signal is a no-op, not a second end). A dropped roam link also
  ends the turn it was carrying, instead of leaving the core believing that chat
  is still busy forever.
- **A generated session name no longer reads as "the run ended".** The
  session-name update carries only `title`/`messageCount`; reading its missing
  `activeRunId` as null cleared the steer key in the middle of a live run.
  `activeRunId` is now parsed three-state (absent / null / id).
- **Android: a send could park behind a dead turn.** When the app believed a
  turn was running, no run id was known, and the core had already closed it, the
  message was queued for a `RunEnded` that never came. It is now sent.

## [0.2.22] - 2026-10-23

### Added

- **A delegate row shows what its subagent is doing.** goose keeps subagent
  sessions off ACP, but forwards each child tool request onto the parent's
  delegate row; the row now carries those calls (name plus one line of
  arguments, bounded to the last 200). The chip shows "N steps", the expanded
  body lists them live, and a one-line strip above the composer follows a
  running delegate that is scrolled out of view — tap it to jump there. Steps
  ride the individual call, so a delegate inside a collapsed run keeps them,
  and they survive a reload.

### Changed

- **The transcript is ordered by the core, not by arrival.** Every item carries
  `seq`, its position in the store; the calls inside a tool run each carry
  their own. Clients insert by that position instead of appending, so a prompt
  echoed after the reply it prompted, a page of older rows, or a re-delivered
  row lands where it belongs.
- **The core owns the prompt row, attachments included.** One row per prompt,
  with `Item.attachments` counting the non-text blocks; the client matches its
  own pixels to that row. An image-only prompt gets a row with an empty caption.
- **A hard refresh returns the history, not a screenful.** Android drains the
  `loadOlder` pages after a FULL refresh (400 items at a time) instead of
  stopping at the newest 60.
- The C ABI exposes `grouse_abi_version()` and `on_run_ended` carries the
  session id; the desktop refuses a core library whose ABI version does not
  match the app's.

### Fixed

- **A replay could silently discard thousands of messages.** The store cap was
  2,000 bubbles; a full replay of a measured 4,761/6,161-bubble session dropped
  everything past it and wrote the truncated list back over the cache, so a
  hard refresh could never recover it. The cap is now 20,000 — a safety net for
  a pathological session, not a screenful estimate.
- **Text rows piled above the tool rows.** Only one item branch stamped a
  position, so ordered insertion put every user/agent/thought row at the top.
- **Charts, MCP apps and tool runs jumped to the top** of a refreshed chat:
  their rows carried no position, and a run's calls all shared one.
- **A roam chat's reasoning was folded into its tool runs.** A peer's
  successful open never cleared the "replaying" flag, so every thought chunk
  was treated as replay framing.
- **A main-connection replay silenced a roam peer.** The suppression window was
  connection-wide; it now applies only to the session being replayed in.
- **A turn end in one chat cleared another chat's busy state.** `on_run_ended`
  now carries the session id, and each client ignores a completion for a chat
  it is not prompting in.
- **An attachment prompt could be shown twice** (a queued one re-added by the
  client, then echoed by the core), and a text-only prompt that reused an
  earlier prompt's text inherited its thumbnails.

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

[0.2.22]: https://github.com/ccgauvin94/grouse/compare/v0.2.14...v0.2.22
[0.2.14]: https://github.com/ccgauvin94/grouse/compare/v0.2.10...v0.2.14
[0.2.0]: https://github.com/ccgauvin94/grouse/compare/v0.1...v0.2.0
[0.1]: https://github.com/ccgauvin94/grouse/releases/tag/v0.1
