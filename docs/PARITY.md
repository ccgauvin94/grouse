# Client parity — the ratchet

Grouse has one core and several UIs. The core is shared, so a *protocol* change
lands once; the **client-side rendering of it does not**. The phone reads uniffi
records, the desktop reads JSON, and neither compiler can see the other. That is
how `Item.files` shipped on the phone and did not exist on the desktop for a
release, with every test green: nothing was broken, the desktop simply had never
heard of the field.

This file is the ledger that makes that impossible to do quietly. Two rules:

1. **A surface the core exposes must be dispositioned by every active client.**
   Implemented (`yes:`), or deliberately not (`excluded:` with a reason). There
   is no third state — "nobody got to it" is not a disposition.
2. **A user-facing feature added to one client is a row here in the same
   commit.** If it is not in the core (pure chrome — a chip, a gesture, a tab),
   add it yourself as a `ui.*` row. The phone is not allowed to be ahead of the
   desktop, and the desktop is not allowed to be ahead of the phone.

## How it is enforced

`scripts/check-parity.py` (CI: `.github/workflows/parity.yml`, locally
`just parity`):

- **The inventory comes from the core, not from this file.**
  `core/grouse-core/src/surface.rs` enumerates every record field, transcript
  op, item kind and listener callback the clients render, and that module is
  itself compile-guarded: it destructures every guarded record (`Item`,
  `ToolCall`, `SubagentStep`, `UploadedFile`, `SessionSummary`, `ConfigOption`,
  `ConfigChoice`, `PermissionRequest`, `PermissionOption`, `ProjectSummary`,
  `ActiveWork`, `SessionOwner`) with **no `..` rest** and matches the enums
  (`TranscriptOp`, `ItemKind`, `PromptBlock`, `FileEvent`) with **no `_` arm**;
  `FileEvent`'s fields shared by all three variants have their own exhaustive
  destructuring guard. Adding a field or a variant breaks the core build until
  the new id is added to the inventory — which is the same list this ledger is
  checked against. A surface therefore cannot appear in the core without
  appearing here.
- **Every id needs a cell per client**, and the client columns must match the
  clients that actually have source in `clients/`. The day `clients/cli` gains a
  `Cargo.toml` and a `src/`, CI demands a column for it — a new client cannot
  slip in unratcheted.
- **`yes:` is evidence, not a claim.** The cell cites `file#token` (relative to
  that client's root, declared below) and the checker greps for the token OUTSIDE
  comments. Paths must be relative and may not use `..`; a commented-out call
  or a file outside the client root is not evidence. A rename in the client
  breaks its own row instead of silently retiring it.
- **`excluded:` moves a pinned counter.** `excluded = N` below must equal the
  number of excluded cells. Opting a client out of a surface is allowed — it is
  one line, in a diff, with a reason a reviewer reads. It is *not* allowed to
  happen by accident, and the number is the ratchet's teeth: it can be lowered
  any time for free, and raising it is a decision someone signs.

`LISTENER_EVENTS` and `LISTENER_SIGNATURES` in `surface.rs` are the hand-kept
lists (the `CoreListener` trait has default bodies, so an added `fn on_*` — or a
changed payload type behind the same name — compiles without noticing); the
checker parses the trait out of `lib.rs` and compares normalized signatures, so
a C ABI trampoline built for an old payload cannot pass parity by name alone.

The checker tests itself: `python3 scripts/check-parity.py --self-test` feeds it
a missing row, a rotted row, a dead anchor, a traversal/absolute anchor, a
comment-only token, a changed callback signature, a placeholder exclusion and
both directions of pin drift, and fails if any of them stops being caught.

## Adding a feature

- **Core-driven** (a new field, op, kind or callback): the core build tells you
  the id exists. Add a row; for each client either implement it and cite the
  token, or write the exclusion.
- **Pure chrome**: add a `ui.*` row with an anchor in each client that has it.
  `ui.*` rows are free-form — the checker never asks for one, because no
  compiler knows your chip exists. They are the part of this ledger that relies
  on the reviewer, and they are why the rule above is a policy and not a test.

## The ledger

Anchors are `file#token`; `+` joins more than one (all must resolve). Roots:

android root = clients/android/app/src/main/java/id/gauvin/grouse
desktop root = clients/desktop

excluded = 21

| surface | android | desktop |
|---|---|---|
| `item.id` | yes: ConnectionManager.kt#item.id | yes: src/manager.cpp#value(QStringLiteral("id")) |
| `item.kind` | yes: ConnectionManager.kt#item.kind | yes: src/manager.cpp#value(QStringLiteral("kind")) |
| `item.text` | yes: ConnectionManager.kt#item.text | yes: src/manager.cpp#value(QStringLiteral("text")) |
| `item.detail` | yes: ConnectionManager.kt#item.detail | yes: src/manager.cpp#value(QStringLiteral("detail")) |
| `item.output` | yes: ConnectionManager.kt#item.output | yes: src/manager.cpp#value(QStringLiteral("output")) |
| `item.status` | yes: ConnectionManager.kt#item.status | yes: src/manager.cpp#value(QStringLiteral("status")) |
| `item.app_key` | yes: ConnectionManager.kt#item.appKey | yes: src/manager.cpp#value(QStringLiteral("app_key")) |
| `item.calls` | yes: ConnectionManager.kt#item.calls | yes: src/manager.cpp#value(QStringLiteral("calls")) |
| `item.thought` | yes: ConnectionManager.kt#item.thought | yes: src/manager.cpp#value(QStringLiteral("thought")) |
| `item.summary` | yes: ConnectionManager.kt#item.summary | yes: src/manager.cpp#value(QStringLiteral("summary")) |
| `item.steps` | yes: ConnectionManager.kt#item.steps | yes: src/manager.cpp#value(QStringLiteral("steps")) |
| `item.attachments` | yes: ConnectionManager.kt#item.attachments | yes: src/manager.cpp#value(QStringLiteral("attachments")) |
| `item.files` | yes: ConnectionManager.kt#item.files | yes: src/manager.cpp#value(QStringLiteral("files")) |
| `item.images` | yes: ConnectionManager.kt#item.images | yes: src/manager.cpp#value(QStringLiteral("images")) |
| `item.message_id` | excluded: clients key rows on `id`; the server id is core-internal merge state | excluded: same — the row model keys on `id`, never on the server's message id |
| `item.seq` | yes: ConnectionManager.kt#item.seq | yes: src/manager.cpp#value(QStringLiteral("seq")) |
| `item.created` | excluded: the area TIMELINE is its consumer (it places chat rows against My Day cards and actions); no chat row prints a date yet, and the field is additive so a client may ignore it | excluded: same — nothing on the desktop chat surface dates a row |
| `work.id` | yes: ChatScreen.kt#key = { it.id } | yes: ui/ChatSidePanel.qml#modelData.id |
| `work.title` | yes: ChatScreen.kt#lane.title | yes: ui/ChatSidePanel.qml#modelData.title |
| `work.detail` | yes: ChatScreen.kt#lane.detail | yes: ui/ChatSidePanel.qml#modelData.detail |
| `work.status` | excluded: the TOOL CALL's lifecycle, and a delegate's call completes its ack in a second — the badge prints `work.outcome` instead (2026-11-13) | excluded: same — `status` is what made the panel say "done" for a running sub-agent |
| `work.running` | yes: ChatScreen.kt#lane.running | yes: ui/ChatSidePanel.qml#modelData.running |
| `work.outcome` | yes: ChatScreen.kt#lane.outcome | yes: ui/ChatSidePanel.qml#modelData.outcome |
| `work.subagent` | yes: ChatScreen.kt#lane.subagent | yes: ui/ChatSidePanel.qml#modelData.subagent |
| `work.steps` | yes: ChatScreen.kt#lane.steps | yes: ui/ChatSidePanel.qml#modelData.steps |
| `call.id` | yes: ConnectionManager.kt#c.id | yes: src/manager.cpp#c.value("id") |
| `call.title` | yes: ConnectionManager.kt#c.title | yes: src/manager.cpp#c.value("title") |
| `call.detail` | yes: ConnectionManager.kt#c.detail | yes: src/manager.cpp#c.value("detail") |
| `call.output` | yes: ConnectionManager.kt#c.output | yes: src/manager.cpp#c.value("output") |
| `call.status` | yes: ConnectionManager.kt#c.status | yes: src/manager.cpp#c.value("status") |
| `call.thought` | yes: ConnectionManager.kt#c.thought | yes: src/manager.cpp#c.value("thought") |
| `call.seq` | yes: ConnectionManager.kt#c.seq | excluded: the desktop paints a group's calls in core order, which is already seq order |
| `call.steps` | yes: ConnectionManager.kt#c.steps | yes: src/manager.cpp#c.value("steps") |
| `step.name` | yes: Bubbles.kt#step.name | yes: src/manager.cpp#s.value(QStringLiteral("name")) |
| `step.detail` | yes: Bubbles.kt#step.detail | yes: src/manager.cpp#s.value(QStringLiteral("detail")) |
| `file.name` | yes: ConnectionManager.kt#AttachedFile(it.name | yes: src/manager.cpp#o.value(QStringLiteral("name")) |
| `file.mime` | yes: ConnectionManager.kt#it.mime | yes: src/manager.cpp#o.value(QStringLiteral("mime")) |
| `file.bytes` | yes: ConnectionManager.kt#it.bytes | yes: src/manager.cpp#o.value(QStringLiteral("bytes")) |
| `file.path` | excluded: a server-side path — clients show the file, never address it | excluded: same; the desktop reads name/mime/bytes off the record only |
| `file.sha256` | excluded: content identity belongs to the core and the server, not the UI | excluded: same reasoning — the desktop never hashes a file it did not upload |
| `session.id` | yes: Wire.kt#sessionId = id | yes: src/manager.cpp#o.value("id") |
| `session.title` | yes: Wire.kt#title = title | yes: src/manager.cpp#o.value("title") |
| `session.updated_at` | yes: Wire.kt#updatedAt = updatedAt | yes: src/manager.cpp#o.value("updated_at") |
| `session.last_message_snippet` | yes: Wire.kt#lastMessageSnippet.orEmpty() | yes: src/manager.cpp#o.value("last_message_snippet") |
| `session.project_id` | yes: Wire.kt#projectId = projectId | yes: src/manager.cpp#o.value("project_id") |
| `session.message_count` | yes: Wire.kt#messageCount = messageCount.toInt() | yes: src/manager.cpp#o.value("message_count") |
| `session.model` | yes: Wire.kt#model = model | excluded: the sidebar shows title, snippet and mode but never a per-session model; the model picker reads ConfigOption instead |
| `session.has_recipe` | yes: Wire.kt#hasRecipe = hasRecipe | yes: src/manager.cpp#o.value("has_recipe") |
| `session.has_new` | yes: Wire.kt#hasNew = hasNew | excluded: no green-dot state in the desktop sidebar model; a staged chat is not marked there |
| `session.mode` | yes: Wire.kt#mode = mode.orEmpty() | yes: src/manager.cpp#o.value("mode") |
| `session.archived` | yes: Wire.kt#archived = archived | yes: src/manager.cpp#o.value("archived") |
| `owner.kind` | yes: ConnectionManager.kt#classifyOwner(kind: String?, label: String?) | yes: src/manager.cpp#grouse_session_owner |
| `owner.label` | yes: ConnectionManager.kt#classifyOwner(kind: String?, label: String?) | yes: src/manager.cpp#grouse_session_owner |
| `config.id` | yes: Wire.kt#id = id | yes: src/manager.cpp#o.value("id").toString() |
| `config.value` | yes: Wire.kt#currentValue = value | yes: src/manager.cpp#o.value("value").toString() |
| `config.name` | yes: Wire.kt#name = name | yes: src/manager.cpp#{"name", o.value("name").toString()} |
| `config.choices` | yes: Wire.kt#choices = choices.map | yes: src/manager.cpp#o.value("choices").toArray() |
| `choice.value` | yes: Wire.kt#Choice(it.value, it.name) | yes: src/manager.cpp#co.value("value") |
| `choice.name` | yes: Wire.kt#Choice(it.value, it.name) | yes: src/manager.cpp#co.value("name") |
| `permission.tool_call_id` | yes: ConnectionManager.kt#request.toolCallId | yes: src/manager.cpp#req.value("tool_call_id") |
| `permission.title` | yes: ConnectionManager.kt#request.title | yes: src/manager.cpp#req.value("title") |
| `permission.detail` | yes: ConnectionManager.kt#request.detail | yes: src/manager.cpp#req.value("detail") |
| `permission.options` | yes: ConnectionManager.kt#request.options.map | yes: src/manager.cpp#req.value("options").toArray() |
| `permission_option.option_id` | yes: ConnectionManager.kt#it.optionId | yes: src/manager.cpp#oo.value("option_id") |
| `permission_option.name` | yes: ConnectionManager.kt#it.name | yes: src/manager.cpp#oo.value("name") |
| `permission_option.kind` | yes: ConnectionManager.kt#it.kind | yes: src/manager.cpp#oo.value("kind") |
| `project.path` | yes: Wire.kt#path = path | yes: src/manager.cpp#o.value("path").toString() |
| `project.name` | yes: Wire.kt#name = name | yes: src/manager.cpp#o.value("name").toString() |
| `project.description` | yes: Wire.kt#description = description.orEmpty() | yes: src/manager.cpp#o.value("description").toString() |
| `prompt_block.Text` | yes: ConnectionManager.kt#PromptBlock.Text(text) | yes: src/manager.cpp#QJsonObject{{"Text" |
| `prompt_block.Image` | yes: ConnectionManager.kt#PromptBlock.Image(img.mimeType | yes: src/manager.cpp#QJsonObject{{"Image" |
| `prompt_block.Resource` | excluded: the phone never builds a blob Resource block; goose drops it, so files go through sendPromptWithFiles and the core appends the link itself | yes: src/manager.cpp#QJsonObject{{"Resource" |
| `prompt_block.Link` | yes: ConnectionManager.kt#core.sendPromptWithFiles | yes: src/manager.cpp#grouse_send_prompt_with_paths |
| `file_event.Progress` | yes: ConnectionManager.kt#FileEvent.Progress | yes: ui/ChatPage.qml#ev.Progress !== undefined |
| `file_event.Saved` | yes: ConnectionManager.kt#FileEvent.Saved | yes: ui/ChatPage.qml#ev.Saved !== undefined |
| `file_event.Failed` | yes: ConnectionManager.kt#FileEvent.Failed | yes: ui/ChatPage.qml#ev.Failed !== undefined |
| `file_event.session_id` | yes: ConnectionManager.kt#event.sessionId | yes: src/manager.cpp#value(QStringLiteral("session_id")) |
| `file_event.request_id` | excluded: the core sends an empty request id until the wire has a per-fetch correlation id, so there is nothing for the phone to key or show | excluded: same — the field is reserved in the payload, but the core emits an empty string for every fetch today |
| `op.Reset` | yes: ConnectionManager.kt#TranscriptOp.Reset | yes: src/manager.cpp#QStringLiteral("Reset") |
| `op.Upsert` | yes: ConnectionManager.kt#TranscriptOp.Upsert | yes: src/manager.cpp#QStringLiteral("Upsert") |
| `op.AppendText` | yes: ConnectionManager.kt#TranscriptOp.AppendText | yes: src/manager.cpp#QStringLiteral("AppendText") |
| `op.AppendOutput` | yes: ConnectionManager.kt#TranscriptOp.AppendOutput | yes: src/manager.cpp#QStringLiteral("AppendOutput") |
| `op.Remove` | yes: ConnectionManager.kt#TranscriptOp.Remove | yes: src/manager.cpp#QStringLiteral("Remove") |
| `op.Window` | yes: ConnectionManager.kt#TranscriptOp.Window | yes: src/manager.cpp#QStringLiteral("Window") |
| `kind.User` | yes: ConnectionManager.kt#ItemKind.USER | yes: src/manager.cpp#QLatin1String("User") |
| `kind.Agent` | yes: ConnectionManager.kt#ItemKind.AGENT | yes: src/manager.cpp#QStringLiteral("agent") |
| `kind.Thought` | yes: ConnectionManager.kt#ItemKind.THOUGHT | yes: src/manager.cpp#QLatin1String("Thought") |
| `kind.Tool` | yes: ConnectionManager.kt#ItemKind.TOOL | yes: src/manager.cpp#QLatin1String("Tool") |
| `kind.ToolGroup` | yes: ConnectionManager.kt#ItemKind.TOOL_GROUP | yes: src/manager.cpp#QLatin1String("ToolGroup") |
| `kind.Chart` | yes: ConnectionManager.kt#ItemKind.CHART | yes: src/manager.cpp#QLatin1String("Chart") |
| `kind.McpApp` | yes: ConnectionManager.kt#ItemKind.MCP_APP | yes: src/manager.cpp#QLatin1String("McpApp") |
| `kind.Error` | yes: ConnectionManager.kt#ItemKind.ERROR | yes: src/manager.cpp#QLatin1String("Error") |
| `listener.on_status` | yes: ConnectionManager.kt#override fun onStatus | yes: src/corebridge.cpp#cb_on_status |
| `listener.on_sessions` | yes: ConnectionManager.kt#override fun onSessions | yes: src/corebridge.cpp#cb_on_sessions |
| `listener.on_item` | yes: ConnectionManager.kt#override fun onItem | yes: src/corebridge.cpp#cb_on_item |
| `listener.on_usage` | yes: ConnectionManager.kt#override fun onUsage | yes: src/corebridge.cpp#cb_on_usage |
| `listener.on_run_ended` | yes: ConnectionManager.kt#override fun onRunEnded | yes: src/corebridge.cpp#cb_on_run_ended |
| `listener.on_speak` | yes: ConnectionManager.kt#override fun onSpeak | excluded: `cb_on_speak` accepts and drops the chunk — the desktop has no TTS engine yet |
| `listener.on_config` | yes: ConnectionManager.kt#override fun onConfig | yes: src/corebridge.cpp#cb_on_config |
| `listener.on_permission_request` | yes: ConnectionManager.kt#override fun onPermissionRequest | yes: src/corebridge.cpp#cb_on_permission |
| `listener.on_session_touched` | yes: ConnectionManager.kt#override fun onSessionTouched | yes: src/corebridge.cpp#cb_on_session_touched |
| `listener.on_projects` | yes: ConnectionManager.kt#override fun onProjects | yes: src/corebridge.cpp#cb_on_projects |
| `listener.on_roam_peer_status` | yes: ConnectionManager.kt#override fun onRoamPeerStatus | yes: src/corebridge.cpp#cb_on_roam_peer_status |
| `listener.on_roam_sessions` | yes: ConnectionManager.kt#override fun onRoamSessions | yes: src/corebridge.cpp#cb_on_roam_sessions |
| `listener.on_peer_new_session` | yes: ConnectionManager.kt#override fun onPeerNewSession | yes: src/corebridge.cpp#cb_on_peer_new_session |
| `listener.on_active_run` | yes: ConnectionManager.kt#override fun onActiveRun | yes: src/corebridge.cpp#cb_on_active_run |
| `listener.on_commands` | yes: ConnectionManager.kt#override fun onCommands | yes: src/corebridge.cpp#cb_on_commands |
| `listener.on_session_open_failed` | yes: ConnectionManager.kt#override fun onSessionOpenFailed | yes: src/corebridge.cpp#cb_on_session_open_failed |
| `listener.on_session_opened` | yes: ConnectionManager.kt#override fun onSessionOpened | yes: src/corebridge.cpp#cb_on_session_opened |
| `listener.on_tasks` | yes: ConnectionManager.kt#override fun onTasks | yes: src/corebridge.cpp#cb_on_tasks |
| `listener.on_thread_ready` | yes: ConnectionManager.kt#override fun onThreadReady | yes: src/corebridge.cpp#cb_on_thread_ready |
| `listener.on_replay_progress` | yes: ConnectionManager.kt#override fun onReplayProgress | yes: src/corebridge.cpp#cb_on_replay_progress |
| `kind.File` | yes: ConnectionManager.kt#ItemKind.FILE | yes: src/manager.cpp#QLatin1String("File") |
| `listener.on_file` | yes: ConnectionManager.kt#override fun onFile | yes: src/corebridge.cpp#cb_on_file |
| `listener.on_thread_timeline` | yes: ConnectionManager.kt#override fun onThreadTimeline | excluded: no desktop timeline surface yet — the C ABI slot and trampoline are in, the corebridge wiring is not |
| `listener.on_sensitivity` | yes: ConnectionManager.kt#override fun onSensitivity | excluded: no desktop sensitivity surface yet — the knob is per-board and the desktop has no My Day strip to put it on |
| `listener.on_advisor_report` | yes: ConnectionManager.kt#override fun onAdvisorReport | yes: src/corebridge.cpp#cb_on_advisor_report |
| `ui.chat-file-card` | yes: ChatScreen.kt#fun SharedFileRow | yes: ui/FileCard.qml#objectName: "cardName" |
| `ui.fileInlineImage` | yes: Files.kt#fun isInlineImage | yes: ui/ImageCard.qml#objectName: "cardImage" |
| `ui.chat-attachment-card` | yes: Files.kt#AttachmentCard | yes: ui/AttachmentCard.qml#objectName: "cardName" |
| `ui.my-day` | yes: MyDayScreen.kt#MyDayScreen | yes: ui/MyDayPage.qml#objectName: "myDayCardBody" |
| `ui.chat-work-panel` | yes: ChatScreen.kt#fun ActivitySheet | yes: ui/ChatSidePanel.qml#objectName: "workList" |
| `ui.context-breakdown` | yes: ChatScreen.kt#fun ContextBreakdown | yes: ui/ContextBreakdown.qml#function categoryLabel |
| `ui.my-day-lazy-cache` | yes: ConnectionManager.kt#fun paintTasks | yes: src/manager.cpp#void Manager::paintCachedTasks |
| `ui.my-day-thread-chat` | yes: ConnectionManager.kt#fun openThreadChat | yes: src/manager.cpp#grouse_open_thread |
| `ui.chat-scroll-pinned` | yes: ChatScreen.kt#var follow by remember | yes: ui/ChatPage.qml#function pinToEnd |
| `ui.chat-stream-hold` | yes: ChatScreen.kt#var heldStream | excluded: the desktop has no frozen streamed row; pinToEnd only scrolls at bounded intervals while the bubble is rewritten in place, so it is not a hold |
| `ui.chat-stream-markdown` | yes: Bubbles.kt#fun settledParagraphEnd | excluded: the desktop streams the reply as plain text and re-parses rich text once at finalize (ui/ChatPage.qml) — its own answer to the same per-token cost |
| `ui.tools-sheet-catalog` | yes: ConnectionManager.kt#fun onExtensionCatalog | yes: src/manager.cpp#void Manager::coreOnExtensionCatalog |
| `ui.dev-mode` | yes: Drawer.kt#ic_dev_mode+ConnectionManager.kt#fun newRoamSession | yes: ui/main.qml#newChatInMode+ui/main.qml#dev-mode-symbolic |
| `ui.advisor` | yes: ChatScreen.kt#fun AdvisorCard+SettingsScreens.kt#Review each dev turn | yes: ui/ChatPage.qml#advisorCard+ui/SettingsDialog.qml#advisorBox |
