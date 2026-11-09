# Changelog

All notable changes to Grouse are recorded here. The format is based on
[Keep a Changelog](https://keepachangelog.com/), and this project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added — the chat's work lanes (sub-agents and background work), both clients

- **`Core::activity()`** (`CONTRACT` §3.7): the active chat's lanes — every tool call that
  reported a **sub-agent** step, plus everything still `in_progress`. goose keeps sub-agent
  sessions out of ACP, so the parent's delegate row is the only record of what a delegate
  actually did; the core exposes it as `id`, `title`, `detail`, `status`, `subagent` and
  `steps`. It is a getter over the transcript the core already holds (no network, no extra
  state), so it works on a roam chat, a replayed chat and a cached one alike.
- **Desktop**: the per-chat panel is now DOCKED, not an overlay. The `Kirigami.OverlayDrawer`
  that floated over the conversation is replaced by a `SplitView` pane (`ChatSidePanel.qml`,
  draggable divider), so it can stay out while you read — and it carries two tabs: **Tools**
  (what it always had) and **Activity** (the lanes above, with each sub-agent's child tool
  calls listed). The Tools pill toggles the pane.
- **Android**: a header badge shows the live lane count while the chat is working and opens an
  Activity sheet (`ActivitySheet`) with the same lanes — the delegate's brief, its status and
  the sub-agent's own calls. Nothing appears in a quiet chat.
- Refreshed from the tool rows that carry step/status changes (`on_item`), on `on_run_ended`,
  and when the panel/sheet is shown; a no-op refresh does not repaint.

### Fixed — the Android Activity surface is reachable

- The header badge only appears while the chat HAS lanes, so it was invisible in exactly
  the chat you want to inspect after the fact. The composer's **+** sheet now carries an
  **Activity** row permanently (with the live count), alongside the tools list; the header
  badge stays as the at-a-glance version.
- A chat now learns its lanes when it is painted (`TranscriptOp.Window`), not only from a
  tool row — a delegate that finished before the chat was opened has no live row to nudge
  the refresh, so the badge could stay empty on entry.

grouse 0.2.91 (versionCode 193).
### Fixed — a project's older chats came back (session/list pages were never followed)

- **`session/list` answers 50 rows at a time, ordered by last message, with a `nextCursor` —
  and the core sent one request and stopped.** This box's Assistant thread chats and hourly
  scheduled runs fill that first page on their own, so every older chat fell off the end of
  what the clients were given: the Media project drew "Games" alone where it used to draw
  Movies, Music and the rest. Measured against the live server: page 1 = 50 rows with a
  cursor, page 2 = "Movies", "Music" and three media sessions.
- The core now walks `nextCursor` to the end (bounded at 40 pages), de-duplicates by id and
  emits the union as one `on_sessions` — and the same for every roam peer's list. A failed
  FIRST page still stays "no answer" (nothing painted) instead of an empty drawer that reads
  like every chat was deleted; a later page that fails keeps what was already collected.
- `tests/spine.rs` pins it: the fake server hands out a two-page list, and the test asserts
  both pages arrive AND that the second request carried `cursor`.

grouse 0.2.89 (versionCode 191).
### Changed — a launch opens nothing

- **No conversation loads by itself any more.** A launch used to land on the Assistant thread
  (immediately, or — when its id was not cached — as soon as a `session/list` arrived, which is
  what opened a chat a minute in on whatever screen you had walked to), and a relaunch could
  drop you back into the restored chat route. Now: the app connects, shows its landing page, and
  stays there until you pick something. The nav stack is reset to the landing page on launch, so
  a process death no longer reopens the chat you were in.
- **The wire still resumes the last main chat.** That is not an open — nothing adopts it, so
  `currentSession` stays empty — it just avoids minting a throwaway `session/new` (an empty
  "New Chat") on every launch. Tapping a chat adopts it as usual.
- Tapping Assistant before the session list has arrived still parks the open, and it is still
  dropped if you walk off to My Day or a project before it lands.
- The launch Ready still fetches the session list (that is what the landing page's recent chats
  come from): skipping the adoption must not skip the one fetch that made the page useful.
- Desktop parity: `autoConnect()` no longer leaves the landing page (`connectToServer()` clears
  it for the manual Connect path; a launch re-asserts it), so both clients open on their landing
  view with nothing loaded.

grouse 0.2.88 (versionCode 190).

### Fixed — the launch's Assistant landing can no longer open a chat later, elsewhere

- Opening the app is meant to land on the Assistant thread. When its session id is not cached
  yet the open is DEFERRED until a `session/list` arrives — and that list can land long after the
  app is usable, on whatever screen the reader is on by then, so a conversation appeared to open
  by itself while clicking around. The deferral is now bounded to the launch (15 s) and to the
  chat surface: going to My Day, a project, a roam endpoint or settings, or opening a session or
  a new chat, drops it. A landing the reader ASKED for by tapping Assistant still survives the
  drawer it was tapped in, and is dropped only if they then go somewhere else. Drops are written
  to the turn log ("dropped the deferred Assistant landing").

### Changed — My Day opens on its cache and refreshes behind it

- **The tab used to force a reload before it displayed anything.** One refresh is four tool
  calls plus a thread-map read, so an open was a spinner until all of them answered, and with no
  wire (cold start, offline) the lists never filled at all. The core now writes the last
  **complete** board to `<cache>/my-day.json` and replays it on open, marked `"cached": true`,
  while the refresh runs behind it — a paint is a disk read and one callback
  (`tasks_paint()`, C ABI `grouse_tasks_paint`). Both clients call it on open before asking the
  server; a cached paint keeps the refresh affordance turning, because it is the live envelope,
  never the cache, that ends it. Only an envelope whose three sections all answered is written —
  a partial refresh would replay "nothing is open" as if the server had said so.

### Fixed — a failing hidden session can no longer mint an agent per retry

- The core's three hidden-session users (the My Day host, the per-endpoint file-fetch host, the
  step-label summarizer) all drop their cached session on any failure — right for recovery,
  wrong as a retry policy, because each retry creates a **whole goose agent**: extensions leased,
  MCP servers spawned. Measured on the roam agent 2026-11-06: ~500 hidden sessions a day and 75
  `attachments-mcp` children after a burst. A failure now cools that purpose off for 30 s
  (`HIDDEN_RETRY_AFTER`, per purpose, so a broken My Day host cannot block file fetches) and the
  decision is recorded in the turn log (`hidden-session … created|failed`). The `attachments`
  extension itself also moved off stdio on the server side (one streamable-HTTP process per
  agent instead of one per session) — see `services/llm/AGENTS.md`.

### Fixed — a stale run id can no longer leave a chat saying "working…"

- **A `session_info_update` repeating the run id of a turn that had already ended re-armed
  it.** The client marks a chat busy the moment it sees an id, and there is no second end to
  wait for — so the chat showed `working…`, the composer switched to "Queue message…", and a
  message typed there was *parked instead of sent* (nothing appeared inline until a refresh
  reset the turn state). Measured 2026-11-08 on a roam chat, whose resync touches re-announce
  the last run. A run the SERVER said ended (`activeRunId: null`, or the `session/prompt`
  reply) is now inert; `turn_lost` deliberately does not record it, because a wire that
  dropped mid-turn leaves the run alive on the host and its re-announcement is the only way
  to re-adopt the chat (both cases pinned by tests).
- **`/compact` and other queued prompts are the victims, not the cause**: with `busy` stuck,
  `send` queues rather than sends.

### Fixed — sending a message no longer re-renders the previous reply as raw Markdown

- The transcript painted "the newest row renders as plain text while a turn runs" — but right
  after a send the newest row is still the PREVIOUS reply, so its Markdown flipped to raw
  `**stars**` and backticks and stayed that way. Plain text is now driven by the core's own
  chunks: `AppendText` marks the ONE bubble being written to (`ConnectionManager.streamingRowId`),
  and the turn's end clears it so the bubble re-renders once as full Markdown.

### Fixed — a chat that will not load now says so instead of spinning forever

- **`session/load` had no timeout on either wire.** A load the host never answers held
  that chat's load slot for the whole await, so the client showed a spinner with nothing
  to report AND every retry of the same chat queued behind the first one — a chat could
  stop opening for good. Reproduced sequence: a turn running in one roam chat, a
  `/compact` in a second, then opening a third (a peer is ONE connection, so both turns
  and the open share one pipe; each open also makes the host build a fresh agent with
  its MCP extensions, which is where the time goes). The load is now bounded generously
  (`session::load_request`, 10 min — a full replay of a long session is legitimately
  minutes), frees the slot, and reports an open failure the client can retry.
- The wait is shared by both transports by construction: `replay_held` is the ONE
  `session/load` lifecycle, so a peer and the main socket fail the same way.

### Fixed — the roam cache fix was inert; the wrapper dropped it

- **`PrefixHost` did not forward the two stamp accessors.** The peer's paint plan asks
  its host for the freshness baseline (`listed_stamp`) and whether the store is truncated
  (`is_incomplete`); `PrefixHost` sits between the peer and the core, and inherited the
  TRAIT DEFAULTS instead of forwarding. So the peer went on comparing its cached
  transcript against its own session list — the drift that makes every open replay a
  tail — and treated an incomplete store as settled. The fix in 0.2.78 was therefore a
  no-op on roam; both accessors are now forwarded, with a regression test that would
  have caught it (a default that silently returns `""`/`false` is invisible to the
  compiler).
- Same trace: a share whose file had to be COPIED in (an import root) kept the caller's
  path, so the client fetched a path the store refuses and the row stayed a card reading
  `ValueError: '…' is outside the attachments directory`. The share reply's path now wins
  (0.2.80).


### Fixed — a picture shared from an import root now downloads, and paints

- **The row pointed at the wrong path.** A share whose file the extension had to COPY
  in (anything outside the attachments store — ComfyUI's `/mnt/data/comfyui/output`, for
  instance) returns the path it landed at, but the core kept the path the model *passed*
  and the client fetched that. The store refuses it, so the row stayed a card reading
  `ValueError: '…' is outside the attachments directory` instead of the picture —
  measured 2026-11-06 on a render shared straight from ComfyUI's output dir, while the
  same image shared by its store path rendered inline. The share reply is now the
  authority on where the bytes are (the name still comes from the call, because a stored
  copy is hash-prefixed).


### Fixed — a roam chat no longer re-fetches itself on every switch

- **Two stamps for one fact.** A peer chat's cached transcript is stamped with the
  core's `updatedAt` baseline (`save_cache_for`), but the peer decided freshness by
  comparing it against its OWN session list — a second copy of the same field that
  only moves on `session_info_update`, while a turn end re-baselines the core from a
  `session/info` probe. Once they disagreed they never agreed again, so `plan_paint`
  answered "stale" for every roam chat forever and each open merged a 100-message
  replay tail: opening or switching re-downloaded the conversation, every time. The
  plan now reads `DispatchHost::listed_stamp` — the one map the cache is written
  with — and falls back to the peer's list only for a session the core has never
  stamped. `DispatchHost::is_incomplete` is read the same way (the peer passed a
  hard-coded `false`).
- **A peer re-baselines after a load, like the main wire.** The spine probes its
  stamp after every replay; the peer never did, so a chat fetched with a bounded
  tail kept the stamp it had before that fetch and could not be recognised as
  current next time. It now probes (and persists) after a successful `session/load`.
- **Superseded opens are dropped instead of replayed.** Every open is a whole
  `session/load` awaited on the peer's single command loop, so tapping through four
  chats queued four full loads and the chat you finally landed on waited behind all
  of them — the "switch too much and it hangs" half of the report. A queued open is
  now coalesced into the newest one; other queued commands keep their order.


### Fixed — the tools sheet lists what the chat could have, not just what it has

- **The sheet offered only the chat's own lease.** It was built from
  `session/extensions/list`, the extensions a session was started with — so a chat whose
  lease predated an extension (a restart, a config edit, an old conversation) had no way to
  see that extension, let alone add it: the row simply was not there. Both clients now show
  the CATALOG of the server the chat lives on, unioned with the lease, with the switch state
  still decided by the lease.
- **A roam chat's catalog comes from its PEER.** `config/extensions/list` takes no session
  id, so the shim always asked the MAIN connection — where a phone that only talks to a
  peer has nothing at all, and the desktop reported the local server's rows as if they were
  the peer's. New `extension_catalog(session_id)` asks that method on the session's own
  connection and answers `on_extension_catalog(session_id, …)`; the C ABI is 12. The main
  connection's list is unchanged and still what the settings screen edits, so a peer-only
  key can never be written into this box's `config.yaml`.
- The peer note in the sheet now says only "this chat lives on <peer> — changes apply
  there": the "only extensions already in the chat are listed" caveat is gone, because it
  is no longer true.


### Fixed — a shared image is the picture's size, and a tap enlarges it

- **The frame hugs the picture.** Both clients sized the preview to the whole column, so
  a narrow image floated in the middle of a wide empty box. The frame (and on Android the
  image itself) now takes the picture's own aspect ratio, capped at 340x300dp on the phone
  and 200px tall on the desktop, and is anchored left.
- **A tap enlarges it.** A saved picture opens a full-window view over a dim backdrop —
  tap anywhere (or Escape) to close. On the phone that view carries "Open", because an
  inline image is deliberately never exported to Downloads and is otherwise only reachable
  through the core's private cache.
- The desktop's viewer is centred by coordinates rather than reparented to `Overlay`,
  which Qt warns about as a binding loop on every card it instantiates.
- `ImageCard`'s natural size is now ASSIGNED by the `Image` on decode instead of bound to
  it: a property binding to an id the component has not instantiated yet evaluates once to
  `undefined` and never runs again, so the size stayed 0 and every picture quietly fell
  back to a full-width frame.

### Added — an image the agent shares is shown, inline

- **A shared image paints as the picture.** The transcript still carries a reference and
  the bytes are still pulled on demand (a blob in a tool result is re-sent on every replay
  and shown to the model), but a raster image is now the reply's own content rather than a
  chip describing it: both clients fetch it as its row appears and paint it inline, and a
  tap opens the full-size copy. Raster only — PNG/JPEG/GIF/WebP/BMP/HEIC/AVIF up to 8 MiB —
  because neither toolkit can decode SVG, and an empty frame reads as a broken picture
  rather than as a file. Android keeps the private cached copy for inline images instead of
  exporting them to Downloads, since a `MediaStore` `content://` URI is not decodable by
  `BitmapFactory`; the tap still opens it through the FileProvider.
- The predicate is duplicated by necessity (`Files.kt` `isInlineImage`, `ChatPage.qml`
  `inlineImage`, ledger row `ui.fileInlineImage`) and pinned by tests on both sides:
  `InlineImageTest` on the JVM, `tst_imagecard.qml` for the card.

### Fixed — a thread tap opens the thread, and its cards are cards

- **The tap binds the chat itself.** `open_thread` used to have the CORE open the session;
  its `Ready` arrived for an id the client never asked to display, so every late-Ready
  guard refused it and the tap left the reader in the previous chat — usually the
  Assistant thread — while the panel showed an area that was not on screen. The core now
  only RESOLVES (map, or a fresh mint) and answers `on_thread_ready`; the client opens that
  id through its ordinary path. A label the mirror already carries opens immediately.
- **`current_thread_label` is the map and nothing else.** It used to fall back to "the label
  we last asked for", which leaked: when an open never landed, that label stuck and the
  panel painted one area's cards above every later conversation — including roam chats,
  which have no thread.
- **The panel paints the same cards My Day does**, not a one-line restatement of each row,
  and it no longer restores a saved "collapsed" — a `rememberSaveable` from an earlier
  build brought the chevron back over the cards.

### Fixed — My Day opens inline, not folded

- **A thread group starts OPEN.** Both clients defaulted its header to collapsed, so a
  label holding several rows showed only "N items" behind a chevron — a number you cannot
  act on, and not the inline board that was asked for. The state now records the groups you
  CLOSED, so open is the default and the chevron only takes rows away.

### Fixed — threads actually open with their context

- **The composer cleared for real.** Opening a thread sent an EMPTY draft, and the chat
  screen treated an empty one as "no instruction" (`if (input.isBlank()) input = pending`),
  so the previous chat's text — usually the "About item …" line — came back into a
  brand-new thread. The draft is now an instruction: empty means start clean.
- **The opening line lands even on a thread that already existed.** The relay that writes
  it (`notify__thread_note`) gained `once`: it writes only while the conversation is
  empty, so the core can re-offer the line on every open. That repaired every thread minted
  before the extension was wired (they had opened blank) and makes a mint-time failure
  self-healing rather than permanent.
- **A seed failure is no longer silent.** Both seed paths log the error instead of
  swallowing it — the silence is why a batch of threads opened blank with no way to tell.
- **The context donut no longer lies.** `usage` is the last `usage_update` seen and goose
  only sends one DURING a turn, so the previous chat's reading survived a session switch and
  a freshly opened thread (one seeded line, no turns) showed a nearly-full ring. Both
  clients now clear it on a switch: no donut is honest, the wrong donut is not.

### Added

- **The agent can hand you a file, in the chat.** A tool that shares one
  (`attachments__attachment_share`) now produces a card in the transcript with
  the file's name, type and size, and tapping it downloads the file and opens it
  — no Nextcloud link, no pasted path. The card carries a **reference only**: the
  bytes are pulled on demand in 512 KiB chunks over the ACP connection that is
  already open, so nothing enters the conversation. That is a measurement, not a
  preference — a tool result containing an embedded file does reach the client,
  but goose stores it verbatim in the session, re-sends it on every replay and
  shows it to the model (a 1 MiB PDF would cost ~350k tokens of context per
  turn), while the card costs ~200 bytes. Downloads land content-addressed in the
  client's cache, so a second tap — or reopening the chat — costs no round trip.
  `attachment_share` / `attachment_read` are new tools on the `attachments`
  extension; they only share from the attachments directory, so a
  prompt-injected agent cannot hand you the container's keys.

### Changed

- **A thread now has exactly ONE conversation, and the assistant makes it before
  you ask.** The label → session map moved to the server (assistant-state
  `thread_sessions`, schema v6) and rides every My Day refresh as
  `threads: {label: sessionId}`; the core mints the missing ones — a real session
  filed under the assistant project, named `Thread: <Label>`, created without
  binding so it never moves the chat you are reading — and both clients open
  through it. Two devices used to keep private maps, which forked a conversation
  between them: on 2026-11-01 "Thread: Global entry" existed twice, 82 and 99
  messages, 21 minutes apart. A dead link is dropped by the core at the open
  failure, and a failed map read keeps the previous map rather than pretending no
  thread has a chat (which is what minted the duplicates).
- **The assistant's actions now land in the thread they belong to.** A new
  `thread_note(label, text)` tool appends one line into that area's own
  conversation — "Closed: the Ramp approval — Heike owns it now" — so opening a
  thread later reads as a log of what was actually done instead of a blank page.
  Deterministic and cheap (one row, no model turn), never a substitute for
  `notify` (that is what reaches you), and declined rather than failed when the
  thread has no conversation yet. The briefing, morning digest and reconcile
  sweep all call it.

- **My Day is a timeline: every row says when it arrived, and the list is
  ordered by that.** An item now carries `first_seen` (and a closed one
  `closed_at`) all the way from assistant-state to the row — they were on the
  wire from the start and the core's projection dropped them, which is why
  nothing on screen had a date. Each row prints "added 17:36" for today, "added
  yesterday", "added Mon" within the week and "added Oct 24" beyond it, a
  handed-over task says "started …", and a closed one says "closed …". The
  in-play list and "Recently done" are sorted **newest first** on that stamp:
  with a date on every row, a salience-ordered list reads as dates scattered at
  random. Rows with no stamp (an older server) sort last rather than to the top.

- **My Day: the rows inside a thread are indented, on both clients.** A label with
  more than one entry paints a header, and its cards used to sit flush with that
  header's own edges — so a group read as a list of siblings of its chevron
  rather than as contents. A row behind a header now steps in from the start
  edge (20 dp on Android, one spacing step on the desktop) while keeping its
  right edge, which is what makes the nesting legible instead of merely
  narrower. A row with no header never moves.

- **Android: a downloaded share lands in `Downloads/Grouse`, not in the app's
  private data.** The core has to write somewhere it can write unaided, so it
  still fetches into `filesDir/files/`; the client now copies the finished file
  into the public Downloads collection through MediaStore (a pending row until
  the bytes are complete, so a half-written installer can never be opened) and
  deletes the private copy. The card says "saved to Downloads" and opens the
  public URI. Below API 29 the public directory needs a storage permission, so
  those devices keep the private copy and the FileProvider path.

### Fixed

- **A chat opens at its bottom, and the jump-to-bottom arrow gets there in one
  press — on both clients.** "Going to the bottom" was a one-shot scroll in each
  client, but the end of a transcript moves afterwards: a new row is inserted at
  the newest position (which pushes the reader up one message), the newest bubble
  measures itself only when it is first composed (markdown re-parse, code blocks,
  attachment cards, images), the waiting bubble appears on send, and the keyboard
  resizes the viewport. So the scroll landed on the end as it was *estimated* at
  that instant, which is why an opened chat sat a message short of the bottom, why
  the arrow sometimes needed two presses (the first spent the distance that had
  appeared since the last measurement), and why sending while at the bottom left
  the new message below the fold. Both clients now treat "at the bottom" as a
  mode driven from the list's own layout: Android follows on every change to the
  item count / newest row's measured size / viewport height and decides whether to
  keep following when a scroll *settles*; the desktop re-pins in bounded passes
  and stops only when a pass lands at the end with the content height unchanged
  (the old loop stopped at the first `atYEnd` sample, which is also true while the
  transcript is still short). Also: sending pins on both clients, the desktop's
  jump button is now offered whenever the view is not at the end (it used to need
  two viewports of overscroll), and its per-chat scroll memory stores the distance
  from the end rather than `contentY` — the core fills history in pages, so the
  same reading position is a different absolute offset on every open.
  Opening at the *first unread* message is still not possible: neither client nor
  the core tracks a read marker, so the choice is the very bottom.

- **The desktop's bundled core can no longer be stale.** Found while chasing "the
  desktop isn't rescaling images": `core/target/release/libgrouse_core.so` on the
  build host was from 2026-10-29 07:41, seven hours before the outbound image
  policy landed, so it exported neither `grouse_set_image_rescale` nor
  `grouse_send_prompt_with_paths` — a bundle built from it would have shipped a
  desktop with no image rescaling and no file attachments, and nothing in the app
  would have said so. `build-flatpak.sh` now asserts those symbols exist and that
  the lib is newer than every `core/**/*.rs`.

### Added

- **A parity ratchet: no client may grow a surface the other does not have.**
  `docs/PARITY.md` is the ledger, `scripts/check-parity.py` is the check, and
  `core/grouse-core/src/surface.rs` is the inventory — compile-guarded, so
  adding a field to `Item`, an op to `TranscriptOp` or a callback to
  `CoreListener` breaks the core build until the ledger dispositions it. A
  `yes:` cell must cite a real token in that client's source; an `excluded:`
  needs a reason and moves a pinned counter. `Item.files` shipped on the phone
  and was invisible on the desktop for a release with every test green — this
  is the test that would have failed.
- **The image policy is testable on its own** (`imgrescale::apply`), with a test
  that feeds it the exact `{"blocks":[{"Image":{"mime_type":…,"data":…}}]}` JSON
  the desktop's `promptJson()` emits — so "the policy covers both clients" is a
  test rather than a comment.

## [0.2.61] - 2026-11-02

### Added

- **The desktop draws attachment cards too**, from the same record the phone
  does (`Item.files`): icon by kind, name, and `kind · size`. Before this a
  non-image attachment on the desktop was invisible in the transcript — the
  send-time chip queue was only populated for prompts that carried inline
  blocks, so a PDF that uploaded perfectly showed up as nothing at all.
- **A replayed chat shows its attachments again.** goose stores the *rendered*
  prompt, so an attachment comes back as its own link stub — a JSON object where
  the message had been, and nothing saying a file was attached. The core now
  splits the stub out at ingest: the row shows the user's message and a file
  card (name recovered exactly, kind from the name, no size — the stub carries
  none). A file-carrying prompt also adopts its own echo now, which it previously
  could not (the stub text never matched what the user typed), so a reopened
  thread no longer lists the same question twice.

### Fixed

- **The name in the card is YOUR file name, not our sanitised one.** The upload
  server rewrites a name to be safe on disk (`Q3 report (final).pdf` →
  `Q3-report-final-.pdf`, spaces to dashes) and was reporting that form back, so
  every client captioned a file the user never named. The rename is now confined
  to the path: `commit` returns the picker's name verbatim, and the transcript,
  the card and the `resource_link` all use it.
- Nothing is renamed any more on the way in, which also retires 0.2.60's
  extension-appending: a file called `document 513` stays `document 513`.

## [0.2.60] - 2026-11-01

### Added

- **An attached file looks like a file.** The composer chip and the sent bubble
  both draw a card: an icon chosen by kind (PDF, document, spreadsheet,
  presentation, image, audio, video, archive, code, text), the file's name, and
  its size. Before this an attachment was a chip reading just the name — and,
  once sent, nothing at all: the bubble carried a count
  (`Item.attachments`) that said "1 attachment" whether or not anything reached
  the server.
- **The bubble's cards survive a reload.** They are drawn from the row's own
  record (`Item.files`: name, MIME, bytes) rather than from a client-side queue,
  so a replayed chat still says what it carried.

### Fixed

- **A file whose picker name has no extension gets one from its MIME.**
  Android's SAF hands back `document-513` for some providers, which the agent
  then had to identify by content (`file`, `head -c`) and which read as an
  unnamed blob in a bubble; it is now stored and linked as `document-513.pdf`.
  The picker's own name always wins when it has a suffix — this only fills a
  hole.

## [0.2.59] - 2026-11-01

### Added

- **Attach any file, and the agent actually receives it.** Files are uploaded to
  goose's `attachments` extension and the prompt carries a `resource_link` instead
  of the bytes. This is a bug fix wearing a feature's clothes: goose silently
  DROPS a `resource` content block whose contents are a base64 `blob` — no
  placeholder, no error — so attaching a PDF showed a chip, echoed "1
  attachment", and delivered nothing. The server resolves the link itself: a text
  file is inlined as its contents (no bytes on the wire), a PDF/docx/xlsx becomes
  a path the agent reads with its own tools, and extraction happens server-side.
- **Prompt rows name the files they carried.** `Item.files` records name, MIME,
  size, server path and sha256, so an attachment is identifiable rather than a
  count.
- **Streaming upload.** The app hands the core a `FileSource` and the core pulls
  8 MiB at a time, so a 100 MB file never exists whole in either language. The
  desktop, being a local app, passes paths instead and the core opens them.
- **Desktop attachments work at all** for non-text files. The desktop had the same
  bug in its own shape (it read the file and sent a blob); it now uploads by path.

### Changed

- Text files attached on the desktop are uploaded rather than inlined client-side.
  Same result at the model, one code path instead of two.
- A prompt whose upload fails is NOT sent, and says so. A message that silently
  lost an attachment is the failure this release removes.

## [0.2.58] - 2026-11-01

### Added

- **Chats load recent-first, and the history behind them arrives a page at a time.**
  Opening a chat this device had not seen was all-or-nothing: `session/load` went out
  with no `_meta.replayTail`, so the server streamed the WHOLE conversation, the core
  ingested it silently and painted 60 rows at the end — measured on the Assistant
  thread, **4,221 messages before the first row appeared**. Every load is bounded now
  (1 message for a settled, current cache; 100 for a stale one it merges; **80** when
  there is nothing to paint), and the reply's `_meta.replaySkipped` is kept as the
  **backlog**: history the server holds that this client has not fetched. That number
  is what keeps `has_older` true at the store's own origin, so a tail-first open is a
  conversation with more behind it rather than a 60-row one.
  `load_older` serves the store first and then fetches ONE page. goose has no
  backwards cursor, so a page is a LONGER trailing window — doubling, floored at the
  rows held plus a page — which makes a 4,221-message chat seven pages and ~2x the
  transcript on the wire, where a linear crawl would re-send it seventeen times.
  Measured live on a 2,012-message chat: `replaySkipped` 1576 -> 1494 -> 1324 -> 992 ->
  376 -> 0 in six pages, the union of which is exactly the whole replay-visible
  conversation, 28.7 MiB total, 300-570 ms a page after a 2 s cold first.
  A page lands in a scratch store (`SessionHub::dispatch_store` routes that key's
  notifications to it), so a half-ingested page cannot touch the transcript on screen;
  its rows are deduped by item id, because a tail measured from the end of a
  conversation that grew between pages can overlap what we already hold; and they are
  spliced in FRONT stamped strictly below every position the client holds, so the
  client prepends them in order when it asks and nothing moves before it does. A
  background fill walks the rest in, one page per 600 ms, while the chat is displayed
  and idle, and the cache records whether it reaches the origin — a fresh-but-partial
  cache still paints instantly *and* still pages.
  The robustness is a set of guards, each for a corruption it prevents: no page while
  a turn is in flight (its rows would go to the scratch store), while a replay owns the
  session, or while another page is in flight; a prompt sent during a page is parked
  and flushed after; the store's rebuild generation is captured before the page and
  compared after, so a paint that landed meanwhile discards it instead of grafting one
  conversation's history onto another's; a page that makes no progress closes the
  cursor, and two failures close it until the next open, so a wire that cannot serve
  pages never turns scroll-back into a retry storm; and a reconnect replay never asks
  for LESS than the store already holds, so bounding it cannot truncate the transcript
  it resumes.

- **Desktop: My Day, thread chats, and a `task` push that brings the page up.** The
  desktop client catches up with the last four phone releases. **My Day** takes the
  chat area (`Mgr.myDayVisible`, opened from the sidebar, left with "Back to chat" —
  opening any conversation also returns): the assistant card with a one-line composer,
  then one Open list that interleaves handed-over work with open loops and groups them
  by thread label (a label holding more than one row collapses; a single-entry label
  never does), then Snoozed, then Recently done. The four intents are the server's own
  vocabulary — close with the outcome line it requires, snooze to the next 05:00, wake
  (a snooze into the past), refresh — and the rows come from the same deterministic
  `tools/call` the phone reads, so there is no model turn anywhere on the page.
  **Discuss** opens the thread's own conversation: a real session filed under the
  assistant PROJECT (slug read from the core via a new `grouse_assistant_project`, so
  the filing and the list filter that hides those sessions can never drift), renamed
  `Thread: …`, remembered label → session in QSettings, and drawn with that thread's
  rows collapsed above its transcript. A stale mapping is dropped when the open
  FAILS, not by testing it against `session/list` — thread chats are filtered out of
  every list, which is exactly the bug the phone had. `src/myday.cpp` is the pure half
  (envelope decode + due/wake/thread/meta formatters, clock injected) with
  `tests/tst_myday` on it, and the page's grouping and delegate tap-throughs are
  covered by a QuickTest. A `task` push now opens My Day when the window is inactive
  (the only time a notification is shown) and otherwise just refreshes the lists.
  Two small parity fixes ride along: **New Chat clears the composer** (the field
  outlives `newChat()`, so a draft used to follow you into the next conversation), and
  phone-side, a thread chat is no longer forgotten-and-recreated on every Discuss.

- **Push: a `task` type that lands on My Day, and a tab that refreshes on arrival.**
  Handed-over work that reaches `review` now posts its own notification type instead of
  riding as a briefing, because the two want different destinations: a briefing has no
  single row to open, a task does — the row, its result line and the rest of the thread
  are on My Day. The tap deep-links there and the tab re-reads on arrival (deterministic
  `tools/call`, no model turn), and **every** proactive push now refreshes the lists, so
  a fresh notification can no longer sit over a stale tab. `notify` gained
  `target: thread|tasks` on the sender side; an older client treats an unknown type as a
  briefing — a correct notification with the wrong destination, never silence. The
  show/don't-show rule is unchanged and still one implementation in `notify.rs`.

- **A thread chat now lives in the assistant PROJECT, and the project stays out of
  every chat list.** discussing a thread files its session under the `assistant`
  goose project, which is what makes the persona apply: goose injects a filed
  session's project instructions into its prompts, so the model answers as the
  assistant without the client pasting anything in. The other half is subtraction —
  that project's sessions are the assistant's working memory for an area, not chats
  in the user's list, so the core drops them from `session/list` (main and peer) and
  drops the project from `sources/list`. It is one rule in one place because both
  clients inherit it; the way into those chats is My Day's thread groups. Their cwds
  are still recorded, because a thread chat is opened BY ID and `session/load`
  rewrites the working dir from the cwd sent — dropping the side-table entry would
  quietly move it on the first resume.

- **My Day: threads group, and each one opens its own conversation.** A thread label
  with more than one thing in play now renders as a collapsible row — name, count,
  chevron, and **Discuss** — and expands in place to its items and handed-over work.
  A single-entry label gets no header: nesting one row under a chevron costs a tap to
  reveal the identical thing, and the last count was 21 in-play entries over 19
  labels, so a header per label would have been all headers and no rows. Expansion
  survives rotation (the state is a separator-joined string, because a `Set` is not
  Bundle-friendly).

- **A thread chat: one real goose session per thread, with the thread painted in.**
  Discuss creates the session once per label, renames it `Thread: …`, remembers its id
  (a title is not a safe key — you can rename it and goose can auto-name it) and
  returns to it on the next tap. It is an ordinary session, so it carries every
  globally-enabled extension — assistant-state, k_ir, chatrecall, memory — with no
  scoping or faking. Above the conversation it shows that thread's open loops and
  handed-over work, collapsed to "3 open · 1 handed over", refreshed on entry (the
  same deterministic `on_tasks` payload the tab reads; no model turn).
  Tapping a **labelled** item now opens its thread chat instead of the always-on
  assistant thread; an unlabelled one still goes to the assistant thread, since it has
  no thread to open. Either way the item id rides in the prefilled line.

- **My Day: handed-over work, above the loops.** assistant-state v4 (2026-11-01)
  split "an act" out of "a fact": an `item` is a loop that needs attention, a
  `task` is something you handed over — `running` while it works, `review` when it
  stopped and is waiting for a look, with a one-line result and the paths it left
  in the agent's workspace. This tab now paints those (`running`/`review` only) in
  a **Handed over** section above Open: `review` rows are tinted and say "ready for
  review", running ones show what was asked for, and the whole card taps through to
  the session that did the work. **No new ABI** — the core grew a third section in
  the existing `on_tasks` envelope (`tasks`, projected by its own field list, with
  `request` trimmed to 300 characters since every byte crosses the FFI) and the app
  parses it alongside `open`/`done`. A server older than v4 has no `task_list`,
  which surfaces as `errors.tasks` rather than an empty section that would read
  like "nothing was handed over".

- **My Day: the Assistant tab, with the assistant's own working state under it.**
  Android's drawer loses the "Assistant" button and gains **My Day** (still gated
  by the Assistant switch): the always-on assistant thread at the top — status
  dot from briefing recency, its last line, and a composer that sends straight
  into the thread — then the assistant-state items that are open, then the ones
  recently closed with the line that closed them. Each open item can be closed
  (`done` / `dropped`, both asked for a one-line outcome, which is what stops the
  next briefing re-raising it), snoozed a day, or tapped through to the assistant
  thread for a discussion. The lists come from assistant-state over a
  deterministic `_goose/unstable/tools/call` — **no model turn**, so the tab opens
  in milliseconds instead of paying a 60k-token prefill to ask "what is open" —
  and because goose refuses an app tool call outside `auto` mode, the core binds
  them to one hidden, never-prompted `auto` session rather than disarming the chat
  thread's approvals (CONTRACT §3.6). New core surface: `on_tasks` on
  `CoreListener`, and `tasks_refresh`/`task_close`/`task_snooze`/`task_react` as
  intents. This is a listener-table ABI change: `GROUSE_ABI_VERSION` is now **6**
  (`on_tasks` appended last; the desktop's `corebridge.h` mirrors the table, and
  `Manager` gained `tasksJson` + the four invokables — the desktop has no My Day
  surface yet). No push for task state: the core refreshes on tab open, on the
  refresh affordance, and after every action.

- **My Day: the item's thread is a label again.** assistant-state schema v3
  (2026-10-29) dropped its `thread` table — an item now carries one nullable
  label naming its area, and one label can hold many items over its life — so
  `state_items` answers `thread` as a string where it used to answer
  `{slug, title}`. The app still read the object shape, which meant the label
  silently disappeared from every row. The core now normalises both shapes to
  one string (`tasks.rs::normalise_thread`, blank dropped) and the row renders
  it humanised ("Chami billing") instead of the raw slug. No ABI change: the
  `on_tasks` payload only loses a nesting level. Grouping rows by label was
  deliberately NOT added — measured against the live store, 21 open/snoozed
  items span 19 labels (two labels hold two items each), so collapsing would
  cost a tap to reveal one row; the label on the row says the same thing today.

- **Tap an inline chat image to expand it.** Attached images in a user
  bubble open fullscreen on tap: Android launches a fullscreen dialog
  (black backdrop, tap anywhere to close), and the desktop opens a modal
  overlay over the window (click or Escape to close). The inline thumbnail
  is unchanged; this just keeps the pixels available at full size in both
  clients.

- **Attached images are rescaled in the core before sending (toggle, default
  on).** A phone camera photo otherwise rides the prompt — and every later
  turn's re-sent history — as megabytes of base64. The core now downscales
  every outbound image to a 1600 px long edge at `send_prompt` (one
  transform covers the wire payload, the echoed row and the transcript):
  JPEG re-encodes at q80, PNG keeps its format and transparency, WebP/HEIC
  or an unreadable payload passes through unchanged with an advisory
  alert. Both clients expose the toggle in Settings ("Rescale attached
  images" / "Rescale attached images for sending"), persisted locally,
  on by default.

- **A message sent mid-turn now parks as a card above the composer, on both
  clients (0.2.39).** Sending while a reply streams used to steer the running
  turn silently (desktop) or park invisibly behind a "1 queued" count (both).
  A steer REDIRECTS the reply being written — the agent drops what it was saying
  and answers the new message instead — so it can no longer be a side effect of
  pressing send. The message now slides up as a card with a **Queued** label and
  the reason it is waiting (`sends when this turn ends`, `with attachment — …`,
  `not connected — sends when it reconnects`), plus **Steer** to promote it into
  the live turn and **✕** to discard it. Steering is offered only while this
  chat owns a live run id (the key the server validates against) and only for a
  text-only message: a steer carries no blocks, so one with an attachment keeps
  waiting rather than losing the file. The queue is also per chat on the desktop
  now — it used to be one global FIFO cleared whenever a chat was opened or
  started, which meant a prompt parked in chat A could be delivered into chat B,
  and switching chats silently threw away input. Draining, counting, and Stop's
  discard all name the on-screen chat, matching Android.

- **The turn log can be copied, and it survives the dialog.** The stuck-turn
  readout was a 40-line ring buffer behind a scrollable dialog, which meant the
  one line that mattered could be off-screen and the only way to read it was a
  screenshot. The app-side log now keeps 200 lines, mirrors every line to logcat
  under the `GrouseTurn` tag, and appends to `files/turn.log` in app-private
  storage (pulled with `adb shell run-as id.gauvin.grouse cat files/turn.log`).
  The dialog gained a **Copy** button.

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

### Changed

- **Android: a drop reconnects in the background, and says so with a dot.** The chat
  title no longer turns into "not connected — tap to reconnect": the header keeps the
  conversation's name and the link state becomes one small dot — green connected,
  slowly blinking amber while it retries on the existing backoff, red once
  auto-reconnect has given up — and tapping the dot opens the connection settings. A
  roam peer is drawn the same way by colouring its endpoint bubble, and tapping that
  bubble opens THAT endpoint's settings. A reconnect never opens a different chat, on
  either transport.

- **Android: scroll-back says when it is waiting on the server.** Past what this device
  has already fetched, an older batch is a round trip now, so the top of the list
  carries a "loading older messages…" row while the core pages the wire — instead of
  the list simply stopping with nothing to show for the gesture.

- **Desktop: the fenced-code pane follows the palette.** It painted
  `Kirigami.Theme.view`, which in this app's custom palette resolves to the platform's
  light view background: a white card inside a dark bubble, with the theme's warm text
  on it (measured off a screenshot: 1.7:1). It now tints the text colour over whatever
  the bubble is — a subtle card in either scheme, ~8:1 — and the syntax palette is
  picked from the TEXT colour's lightness rather than `colorScheme`, which could
  disagree with the palette and put light-theme token colors on a dark pane. The copy
  button moved into a header row beside the language label, so it no longer floats on
  top of the first line of code, and a fence with no language is left monochrome:
  coloring every timestamp and port in a pasted log turned the pane into confetti.
  The horizontal bar is an overlay, so a pane whose content just fit its height had
  its last line underneath it; the bar's height is now reserved inside the scrollable
  content, and the pane grows by it.

- **Reload is a real rebuild, and it says how far along it is.** A refresh trusted
  a settled cache and merged a bounded tail, which is right for an open and
  useless for a transcript known to be wrong — so the reload path (`RefreshMode::Full`,
  the Reload button on the desktop and the pull-up handle on the phone) now
  DELETES that session's cache files instead of merely disbelieving their stamp,
  clears the painted chat and replays the whole thing from the server. Nothing
  stale is left on disk to be merged into or re-saved from, which is the thing a
  reload exists to escape.
  It is **single-flight per session**: a second reload while one is in flight is
  dropped rather than restarting the rebuild, and both clients disable the gesture
  while it runs. Progress arrives on a new listener event (`on_replay_progress`,
  ABI 7) as `loaded` / `total` counted in MESSAGES — distinct message ids the
  replay ingested, against the server's `messageCount` — so a long thread shows
  "412 / 4215" beside a 3px bar instead of a spinner that means nothing. `done`
  is the terminal event, so no client has to guess when to put the bar away.

- **Code blocks have a copy button.** A small icon sits at each fenced
  block's top-right (a sibling of the scroll pane, so it does not scroll
  away with the content): one tap copies the raw code; the button flips
  to a check briefly to acknowledge. Desktop (`CodeFence` ToolButton →
  `Mgr.copyToClipboard`) and Android (`CodeCopyButton` in `CodeBlock.kt`
  → `LocalClipboardManager`).

- **A failed `session/set_config_option` is now visible instead of silent.** (peer refused, timed out,
  peer dropped) previously just left the dropdown on its old value (the core
  only updated state on `Ok`). The core now emits the new advisory
  `ConnectionStatus::Alert(message)`: the Android app shows a toast, and the
  desktop prints the message in the status bar — without flipping either
  client's online/turn state (that is what `Error` means, and this isn't it).

- **Roam chat lists sort newest-first everywhere.** Android already sorted the
  Roam tab per peer by parsed `updatedAt`, but its parser gave up on timestamps
  missing an offset (all tie at 0, and the ascending-id tie-break then reads
  exactly as "most recent chat at the bottom"); it now also accepts naive
  datetime shapes. The desktop applied NO sort at all to a peer's list
  (`RoamListModel` took the core's JSON verbatim) — it now stable-sorts
  newest-first as well.

- **Compaction and context-window status are now keyed to the session on
  screen.** The core's custom-notification dispatch (`status_message`,
  `usage_update`, `message_usage`) drops any notification whose `sessionId` is
  not the displayed chat: goose's own scheduled runs compact constantly, and
  their status lines (and token numbers) were reaching whichever client window
  happened to be idle — the root of the random "compacting" spinner. The
  `usage_update` branch is NEW: before, the custom channel carried no
  context-window data at all, so a roam session's context bar (which only
  receives custom notifications) never refreshed. This is a listener-table ABI
  change (the surface gained `on_usage`); bump `GROUSE_ABI_VERSION` to 4 and
  mirror in `corebridge` — both done.

- **Only the phrases goose uses for a RUNNING compaction arm the spinner**
  (desktop + Android both matched any status text containing "compact"). The
  arm strings are the three goose emits ("Exceeded auto-compact threshold of
  N%", "goose is compacting the conversation...", "Context limit reached.
  Compacting to continue conversation...") and the terminators are
  complete/error/failed/cleared; the end of the turn clears it as a backstop,
  and the desktop/Android now display goose's actual status line verbatim
  instead of a static "Compacting…" placeholder.

- **The parked-message card reads like a card, not a clipped row.** Its text shared a
  `RowLayout` with the Steer/✕ buttons, so it never got the card's width and broke off
  mid-sentence under them, and the card's height came from a row whose implicit height
  lagged the wrap. It is three bands now: a "Queued" badge with the note
  ("sends when this turn ends", or the attachment caveat) and a proper close button
  across the top, the message at full width with up to three lines, and Steer
  right-aligned below out of the reading path.

- **The APK that GitHub CI publishes is now signed with the release key.** The
  workflow built the debug variant and let it fall back to the runner's throwaway
  debug keystore, which GitHub regenerates on every run — so the artifact could not
  install over an installed release-signed build (`INSTALL_FAILED_UPDATE_INCOMPATIBLE`,
  same code, different signer) and no two CI builds upgraded each other either. It
  now configures the same keystore the release workflow uses, and a verification step
  compares the APK's certificate against the keystore's and **fails the build** if
  they differ, so a silent fallback cannot ship.

- **The desktop transcript no longer keeps every bubble alive for the life of a
  chat.** `cacheBuffer` was 20000px ("keep every delegate realized", which
  stabilised the scrollbar) while each scroll-back prepends another 40 rows from
  the core's 60-item window — so a long read grew the live set without bound, and
  every row had already paid for all four bubble variants (user, agent, tool chip,
  tool group) because they share one delegate. The cache is bounded and rows are
  recycled, which is what made scrolling smooth again. Per-row UI state (thinking
  bubbles, tool chips) moved into the model and is keyed by a stable row uid
  rather than the row index, so it survives both scrolling away and being recycled.

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

### Fixed

- **Android: the Assistant tab opened a blank chat instead of the assistant thread.**
  The thread had been filed into the assistant PROJECT, and that project is invisible to
  every chat list by design (its sessions are the assistant's per-area working chats,
  reached from My Day) — so a lookup by title found nothing and the tab minted a fresh
  "New Chat" on every tap. Measured cost: a 4,221-message conversation that looked gone.
  A cached id is authoritative and the list is a hint — the staleness signal is the open
  FAILING (grouse#19), which is where a cached id is dropped — so the thread now opens by
  its id even when no list mentions it, and `openThreadChat`'s mapping already worked
  that way. The thread itself was re-filed out of the project on the server.

- **A reload that was cancelled could wedge a chat forever.** `begin_replay` puts a
  session in the "replaying" set and only a completed load removed it, so a
  `session/load` that never returned — a roam peer's link dying under it, the task being
  dropped, a laptop going to sleep — left the session marked forever: a forced reload
  refused to run (`replaying` is the single-flight gate), and the client's reload bar
  spun with nothing to report. A drop guard now ends the replay and releases the held
  emissions however the future ends, and the phone disarms its own bar after 8 s so a
  command a disconnected peer legitimately discards cannot leave a dead handle.

- **Desktop: a reply containing a fenced code block rendered as raw markdown.**
  `ensureRowHtml` splits such a row into `blocks` and deliberately leaves `html`
  empty (each code chunk gets its own horizontal scroller), but the delegate read
  that one role as `mdel.blocks` — the only role in the file not read through the
  model. Roles are context properties on a delegate, not members of the delegate
  `Item`, so it was always `undefined`: `hasBlocks` never became true, the flat
  `TextEdit` stayed visible with an empty `html` and fell back to `messageText`,
  and the code panes were handed `undefined` too. The wrapper now exposes
  `messageBlocks`, like every other role.

- **Desktop: the newest bubble stayed raw markdown until the next message.**
  A row's `html` is computed once, at its authoritative upsert, and the streaming
  deltas clear it on purpose (RichText re-parses the whole document every frame).
  That leaves the TAIL depending on the turn-end seal: a late or lost `run_ended`
  kept the last bubble showing its source until the following message re-sealed it
  — which is why the previous bubble seemed to fix itself one message later.
  `renderTail()` now renders the tail once the reply has been quiet for 700 ms,
  independent of the seal, and skips it when the row did not change.

- **Desktop: the window failed to load at all once My Day shipped.** `main.qml`
  instantiates `MyDayPage`, but `ui/MyDayPage.qml` was never added to the
  `qt_add_resources` list in `CMakeLists.txt`, so the type did not exist at runtime
  and `QQmlApplicationEngine` failed the whole component
  (`qrc:/main.qml:750:13: MyDayPage is not a type`) — host build and flatpak alike,
  nothing rendered. Registered it with the other pages.

- **`tst_myday`'s meta-line case only passed in UTC.** It built `now` as a
  machine-local `QDateTime` and fed it `…T18:00:00Z` snooze strings, so "wakes in 6h"
  was the expectation only in a UTC zone: in US Mountain 18:00Z *is* 12:00 local, the
  delta was zero and the row read "wakes now". `now` is pinned to UTC, as in the
  `wakeLabel` case above it.

- **Desktop: a fenced code block came with a blank bubble at each end, and a message
  without one was split anyway.** `markdownBlocks` pushed a newline into the prose
  accumulator *before* flushing it, so a fence at the start of a message (or one closed
  at its end) emitted a whitespace-only prose chunk — and `markdownToHtml("\n")` is a
  complete HTML document with an empty body, i.e. a live element in the bubble that
  occupies space and shows nothing. It also answered "blocks" for a message with no
  fence at all, which is the caller's cue to split; prose-only messages now return an
  empty list and take the single-document path, and whitespace-only prose is dropped
  rather than rendered. Three `tst_markdown` cases had been failing on this and were
  assumed to be pre-existing breakage, which is how it shipped.

- **Every due date read backwards.** `ChronoUnit.DAYS.between(a, b)` is `b - a`, and the call
  had `now` and the due date the wrong way round, so the sign of every branch inverted: the
  year-end-review item due **2026-11-06** (seven days in the future) rendered "overdue 7d",
  and the flu shot fifteen days past its Oct 15 date rendered "due in 15d". Nothing failed
  loudly, because a due date is only ever read by a human and the briefing titles carry their
  own prose — "eight days past its Oct 15 due date" sat directly above "due in 15d" and the
  contradiction stayed invisible on screen. The fix is the argument order. `DueLabelTest`
  pins both directions, today/tomorrow, the two-week switch to weeks, a naive datetime, and
  junk/blank input.

- **A new chat started with the previous chat's draft.** `input` is `rememberSaveable`, and
  starting a chat while already on the chat screen pops the `chat` route and pushes it again —
  Compose reuses the composition slot across that, so the holder (and the half-typed message,
  and any picked attachments) came straight back and the "new" chat was born pre-filled.
  Nothing in the core needed changing; `newSession()` now bumps `freshChatSeq` and the chat
  screen empties the composer once per bump, so a rotation cannot eat a draft you are still
  writing while an explicit "New chat" always starts clean.

- **My Day's "Open" did nothing visible.** `openAssistant()` only *selects* the thread in
  the core, and My Day is its own route — so the tap switched the chat behind the screen
  and left you looking at My Day. It read as a dead button, and the next back press
  revealed a chat you never asked to open. The card's Open, the composer, and a row's
  tap-through now all go through one `toChat()` that navigates, so every entry point on
  the tab lands in the conversation.

- **Snooze looked like it did nothing.** The write always worked (assistant-state
  flips the item to `snoozed` with a `snooze_until`, and the pulse log records it),
  but the tab re-read the same open list and painted the row *identically* —
  server-side a snoozed item is still open, just quiet, and the core was not even
  carrying `snooze_until` across the FFI. Now the core keeps that field, My Day
  splits the open list into **Open** and **Snoozed** (muted card, wake time in the
  meta line), and a snoozed row's Snooze button becomes **Wake**. Snooze also
  means *tomorrow morning* (next 05:00 local) rather than "+24h": an evening tap
  used to land after the 06:00 digest and skip a whole day of the assistant's
  attention. A snooze whose time has passed reads as open again, which is what the
  server's own wake pass does on its next rank.

- **My Day read "briefed never" (red dot) on any device that had not received a
  briefing push since install.** The status dot was driven by `lastBriefingAt`,
  which the PUSH path stamps — so a fresh install, or one with push off, showed a
  dead assistant over a healthy one. It now takes whichever is newer of that
  stamp and the Assistant thread's own `updatedAt` (briefings append to the
  thread; nothing else moves it while the assistant is idle), and when neither is
  known it shows **no dot and no claim**: an unknown age is not a stale one.

- **Due dates rendered as raw ISO.** `due_at` is usually a bare date
  (`2026-07-05`), which `relativeTime` cannot parse, so rows read "due
  2026-07-05". A dedicated `dueLabel` says what a reader needs instead —
  "overdue 16w", "overdue 8d", "due today", "due in 3d".

- **Tapping an item now discusses THAT item.** It opens the assistant thread with
  `About item <id> (<title>): ` already in the composer instead of an empty chat:
  the id is the handle that lets the assistant call `state_items`/`state_close`
  on the item on screen rather than guessing. A one-shot `pendingDraft` carries
  it in and is consumed on entry, so it never resurfaces on a later visit.

- **User images were flattened to `[image]` in the transcript on replay.**
  ACP is fine: goose sends the full base64 image in every `session/update`
  chunk, live and on `session/load`. The core's own `content_block_text()`
  collapsed every non-text ContentBlock into a placeholder, so anything that
  re-opened or reloaded a chat lost the pixels. The core now carries images
  natively: `Item.images` (data URIs, ABI 5) is filled at send time by the
  echoed prompt and on replay by `append_image` (deduping into the adopted
  echo row), and both clients render from it — the old text-matching pixel
  queues remain harmless fallbacks.

- **Roam chats lost all per-message stats (tok/s, TTFT, tokens) and
  compaction/context status.** The ABI-4 custom-notification gate above keys on
  the DISPLAYED session id, and a peer chat displays `roam:<label>:<raw>` — but
  `RoamPeer` forwarded goose's custom notifications (`message_usage`,
  `status_message`, `usage_update`) with the server's NAKED sessionId, so every
  one of them for an open roam chat failed the gate and was silently dropped.
  `RoamPeer` now namespaces the key (`roam:<label>:<raw>`) before dispatch.
  Main-wire chats were unaffected (their two ids already match).

- **Reload on the desktop now forces the replay.** A click used to be the QUICK
  refresh — a freshness probe that re-loads the newest row and repaints nothing when
  the cache already matches the server — so on an unchanged chat the button silently
  did nothing, and the force path existed only behind a long-press nobody discovers.
  A click is the full replay now (`reloadSessionFromStart`), which is what the label
  promises and what Android already offers as its own explicit action; `refreshSession`
  (Quick) stays for the automatic paths.

- **A Reload press on a roam chat could do nothing at all.** `RoamPeer::open_session_with`
  accepted only `Ready | Connecting` and returned silently for every other status, and
  `Syncing` is a real one (a resync, a reconnect's sync) — so a reload pressed in that
  window issued no request, no status and no error, on peers only; the main wire has no
  such gate. It accepts `Syncing` now, which is safe for the same reason `Connecting` is:
  the command queue lives on the peer, so the command is buffered and runs when the loop
  picks it up. (The Reload button itself is unchanged in intent: a click is the QUICK
  refresh — it probes the newest row and suppresses the repaint when the cache is
  current — and press-and-hold is the full replay, as its tooltip says.)

- **A chat opens at its newest message, on both clients.** Both snapped once, then
  let the transcript paint over the top of that jump: rows are realized lazily (and
  recycled) on the desktop, and Android inserts each batch at the head of a
  `reverseLayout` list, so in both cases the view drifted up and left the reader a
  few messages short of the end. The desktop now re-pins until the list reports
  `atYEnd`; Android snaps until the transcript stops growing, or the reader takes
  over. Neither client tracks unread state, so every open is "newest".

- **Expansion stayed on the wrong message after a scroll-back.** `MessageListModel`
  keyed per-row UI state (a thinking bubble's open state) by row index while
  `prepend()` shifted every row and never remapped it: open a bubble, scroll up
  once, and a different bubble came back expanded, with no `dataChanged` for the
  rows that had inherited it. Keyed by a per-row uid now, remapped by construction.

- **A roam chat showed the transport handle instead of its title.** `roam:<peer>:<id>`
  is an internal namespace; the human title lives only in the peer's session list,
  so the open now seeds from it and the list keeps an on-screen title in sync (a
  peer names a chat after its first turn, or renames it later). An untitled chat
  reads `peer · id`, never the namespace.

- **A roam chat said "loading…" forever.** The status is set locally on open and
  only the connection-level `Ready` cleared it — which a peer open never emits. The
  core's `Window` op marks the painted transcript and is the only event a
  *successful* open raises, so it settles the status now (and the same latent hole
  on the main wire: a second chat opened in the same connection).

- **The bottom of a roam session row was dead to hover and clicks.** The gap came
  from `ListView.spacing`, which belongs to no delegate: use `spacing: 0`, draw the
  gap inside the row, and let the hit target cover the whole row.

- **The desktop logged 61 "Unable to assign [undefined]" warnings per launch.** The
  roam delegate is one `Item` hosting both row layouts and a hidden branch still
  evaluates its bindings, so every role has to answer for every row type; the
  session map's own lookups are coerced too (a peer may not carry a key). A
  guardrail test now asserts every declared role is valid for both row types.

- **The Tools and Recipes dialogs clipped their right edge.** Tools put its trailing
  type tag under the overlay scrollbar (now reserved, like the chat bubbles); Recipes
  had six fixed-width cron pickers summing to ~620px in a ~500px pane, which pushed
  a horizontal scrollbar onto the whole detail area — they wrap now — and its
  provider/model row claimed fixed widths that cut off the Save button.

- **A provider could be listed twice in the model picker.** Two providers can share
  a display label (a CLI-detected entry and its API twin). An exact duplicate is
  dropped; a shared label with different values keeps both, the later one carrying
  its id so it is distinguishable.

- **A roam chat's sidebar had no rename/edit menu, and its model bar could vanish.**
  The roam rows' `MouseArea` counted only `Qt.LeftButton`, so right-click did
  nothing there — rename/archive/export/delete were reachable for main-wire chats
  only. Right-click now opens the same `sessionMenu` (already roam-aware: it hides
  "Move to project…" for `roam:` ids, and the core routes session mutations to the
  owning peer). The provider/model strip was gated on `Mgr.online` — the MAIN
  WebSocket's state — so a peer chat hid its model picker whenever the main wire was
  down, even with a working model over roam; it now gates on the session's own
  options. The pickers also rebuild on a session switch, which previously left the
  outgoing chat's provider/model on screen until that chat's config arrived.

- **The form dialog's fields never initialised.** `function reset()` collided with
  Qt 6's `Dialog` `reset` signal, so QML rejected it as an invalid override and the
  pass that filled in defaults never ran: booleans did not start `false`, enum
  pickers did not pre-select, and a server-parked form opened blank.

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
