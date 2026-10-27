# Changelog

All notable changes to Grouse are recorded here. The format is based on
[Keep a Changelog](https://keepachangelog.com/), and this project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- **The desktop no longer prints the user's own message twice (0.2.38).** A send
  that carried attachments made `dispatchSend` append a user row of its own, and
  the core then echoed the prompt's row as it always does: two bubbles, one with
  thumbnails and one without. The echo is the only row now — the desktop instead
  queues its thumbnails against the prompt text and hands them to the core's row
  the first time an item asks for them with a non-zero attachment count, which is
  exactly how Android already did it. Matching on the count is what keeps a later
  text-only prompt with identical text from inheriting the pictures.
- **The desktop inserts by sequence, not by arrival (0.2.38).** `MessageListModel`
  appended every new item, so any row that the core delivered out of order — a
  replay's tail, a prefill's later seq — landed at the bottom of the chat instead
  of where it belongs. Insertion now walks to the first row whose `seq` outranks
  the new one and inserts before it, and appends only when nothing does. `seq` 0
  is a real position (a turn's first prompt), so the test is `>`, exactly as
  Android's `upsertBubble` does it.
- **A finished turn no longer leaves the chat stuck on "working" (0.2.37).** The
  core announced the run end correctly the whole time — the turn log showed
  `stage: announced` — and the app dropped it on its own main-thread hop: the
  listener override posted `main.post { onRunEnded(...) }`, which resolves to the
  override itself (same signature, nearer implicit receiver), so one core event
  re-posted forever instead of ever reaching `ConnectionManager.onRunEnded`. The
  reply rendered, but `busy` stayed true: the composer kept saying "working"/Stop,
  and because `activeRunId` had been cleared by the run-end notification, the next
  send went out as a new prompt instead of a steer. Every other callback already
  dodged this (`onCoreItem`, `onCoreActiveRun`, or an explicit
  `this@ConnectionManager.` — the same class of bug fixed for
  `onRoamPeerStatus`'s 744k-re-delivery storm); `onRunEnded` was the one left
  unqualified. It now posts to `onCoreRunEnded`, matching the convention, and logs
  `ffi: runEnded …` synchronously on the callback thread so a lost callback is
  distinguishable from a lost event.

### Added

- **The turn log can be copied, and it survives the dialog.** The stuck-turn
  readout was a 40-line ring buffer behind a scrollable dialog, which meant the
  one line that mattered could be off-screen and the only way to read it was a
  screenshot. The app-side log now keeps 200 lines, mirrors every line to logcat
  under the `GrouseTurn` tag, and appends to `files/turn.log` in app-private
  storage (pulled with `adb shell run-as id.gauvin.grouse cat files/turn.log`).
  The dialog gained a **Copy** button.

### Changed

- **The newest chip is labelled too, instead of waiting for the next step.** A
  step's chip appears when its call lands, and by then the thinking that opened it
  is finished — so it was never actually "still being thought". It is now
  labelled like every other step, which means the chip under the reader's eye
  stops showing a raw first line of thinking. The turn's last step is rewritten
  once more when the turn ends, since a run can gain calls after its first label.
- **A turn that has gone quiet says why.** After 20 s with nothing arriving while
  a chat is "working", the typing line gains `· run <id|none> · wire-busy|idle ·
  quiet <n>s`. That is the whole diagnosis: a live run id means goose still owns
  a run (something in the turn is hung), no run id means goose already closed it
  and the end signal never reached the client.

- **A tool run is now one chip per step, labelled with what it set out to do.**
  A turn used to collapse its whole loop into one chip carrying a paraphrase of
  the model's first thought ("The user is asking what time the Yankees ga…"), with
  the rest of the thinking and every call hidden inside it. Now a burst of
  thinking plus the calls it ran is ONE step — its own chip — so a turn reads as
  its intentions in order: *Looking up the Yankees game time* → *Checking your
  calendar for conflicts* → the answer. Calls that share a step (parallel calls,
  nothing thought between them) still collapse into one chip, since they are one
  decision. Issue #13 collapsed the loop into a single run when a chip could only
  show a count; with a label per step the loop reads better as its steps.
- **The step labels are written as intentions.** The end-of-turn summarizer (the
  existing hidden session — no second model) now asks for what the step *set out
  to do*, in the agent's own voice, and forbids quoting the thinking or starting
  with "The user": measured against the live model, "The user is asking what time
  the Yankees play today" becomes "Searching for today's Yankees game time". A
  turn's steps are capped at 40 per rewrite; past that the provisional label (the
  thinking's first line) stands.

### Added

- **The desktop can answer the two structured requests it used to drop (0.2.38).** A recipe
  that declares parameters, and an MCP tool that asks a question
  (`elicitation/create`), both park a responder on the server side and the turn
  waits on it — so a client with no form for them did not merely lose a feature,
  it hung the run until the request timed out. Both families now land in one
  `FormDialog`: booleans as checkboxes, `enum`/`oneOf` schemas as a ComboBox
  (with `valueRole`, or `currentValue` hands back the model item), everything
  else as a text field, with `required` gating submit. Recipe parameters answer
  as strings with submit|cancel — a decline is a cancel there — while an
  elicitation sends typed values with accept|decline|cancel. Every exit answers
  the request, including Escape and the window close, because a dropped request
  is a hung turn rather than a dismissed dialog. The core parks one responder
  per family, so a second request of the same family replaces the first queued
  form instead of stacking behind a request the server already abandoned.
- **A delegate's own tool calls are visible (0.2.38).** A `delegate` call carries the
  sub-agent's calls, which both clients were dropping on the floor: the chip
  said "4 tool calls" and nothing showed what they were. The item already
  carries them (`steps`), so the desktop now has a `steps` role, a *n* steps
  line on the chip header, and a `name · detail` list inside the open chip —
  the same rows Android renders.
- **Debug diagnostics are a switch on both clients, off by default (0.2.38).** The turn
  log and the live turn-state readout show internal state rather than
  conversation, so they now sit behind Settings → *Developer diagnostics*
  (Android: Experimental). Off, the chat title's long-press does nothing and the
  typing indicator has no polling behind it — the readout's `produceState` loop
  runs only while the switch is on. The core keeps the log either way; the flag
  is pure UI, stored per client (`debug_tools`), so turning it on does not
  change what the core does. The desktop's copy of the log is a dialog with
  Copy/Reload and the same live turn-state line.
- **Steps are labelled while the turn runs, not only when it ends.** A step's
  chip used to keep the provisional label (the thinking's first line) until the
  whole turn finished, which is most of what a reader actually sees. Now the
  moment a call opens a NEW step, the step before it is closed and its label is
  requested — debounced, so a fast loop coalesces into one request per quiet gap,
  and serialised against the end-of-turn pass so two prompts never share the
  hidden session. Measured on a real turn: `Creating temp directory and timestamp
  file` landed before the turn ended, and the remaining steps' labels followed
  right after it. A step with no absorbed thinking is labelled from its call
  titles instead of being skipped, since goose narrates between steps and the
  call after a narration opens a step with nothing thought directly above it.
- **The pulse is where the waiting is.** The thinking row and the "Grouse is
  thinking…" line tick `…` as well, so a turn in progress always shows movement —
  a chip's own pulse only lasts as long as its tool call, which for a shell
  command is under a second.
- **A running step's chip pulses.** While a step is actually working its label
  ticks `…` (and the collapsed run shows a spinner instead of the wrench), so a
  live chip reads as "Looking up the game time…" and settles the instant it
  finishes. It animates only while *this chat's* turn is in flight: a replayed
  row whose completion the server never re-sent keeps `in_progress` forever, and a
  chip pulsing in a finished chat would claim work that is not happening.
- **Concurrent turns, one per chat.** A reply can stream in a roam chat while
  another runs on a different peer or on the main connection: in-flight state is
  keyed per session, each chat keeps its own queue, and a completion clears only
  its own chat. Switching to a running chat restores its spinner and steer key
  instead of showing it idle; a prompt queued in a background chat starts when
  that chat's turn ends, even while another chat is on screen.

### Fixed

- **A chat can now say why it thinks it is working.** Long-press the chat title
  for the core's turn log: prompt sent (with the wire id), run start/end, the
  `session/prompt` reply, and every end-of-turn decision — including the ones it
  had to IGNORE because no turn was in flight, printed with the keys it was
  holding. Both end-of-turn signals are invisible to a client, so when one goes
  missing this is the difference between a guess and a fact. (Replaces the
  silence watchdog: a timer papers over a lost signal instead of showing it.)

- **A turn adopted after a reconnect could never end, so the chat sat on
  "working…" until Stop.** When the link dropped mid-turn the core released the
  turn (no reply could ever arrive) — correct — but `in_flight` was emptied with
  it. If goose re-announced the *still-running* run on reconnect, the app marked
  that chat busy again on the run id while the CORE no longer knew the turn
  existed, so `finish_turn` found nothing to release when the run ended and
  emitted no `run_ended`. The reply rendered in full, the composer said "Queue
  message…", and only Stop broke out. An announced run id now counts as a turn in
  flight, so the end signal releases it. Pinned by
  `a_run_adopted_after_a_reconnect_still_ends_its_turn` (verified to fail without
  the fix).
- **The quiet readout no longer lies.** A chip's label rewrite is not progress:
  counting those Upserts kept a stuck turn reading `quiet 0s`. Only rows whose
  text, output, steps, status or role actually changed reset the timer.

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
