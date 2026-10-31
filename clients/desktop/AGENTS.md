# Repository Guidelines

Grouse Desktop — a KDE-native (Kirigami/Qt6/C++) client for a self-hosted goose
(`goose serve`), spoken to over ACP (JSON-RPC over WebSocket).

This lives in the grouse monorepo at `clients/desktop/`. **Read the repo-root
`AGENTS.md` first** — it owns the architecture contract. This directory is now
a THIN CLIENT: it consumes `grouse-core` through the C ABI (`grouse_*`,
dlopen'd as `libgrouse_core.so` via `src/corebridge.{h,cpp}`), which
owns the ACP connection, the transcript, streaming, reconnect and the roam
transport. No protocol logic lives here anymore; a protocol fix is made in
`core/` only. This file documents the thin (native-UI-only) side.

## Project Overview

- The app holds no state of its own. The server owns sessions, memory, tools,
  model choice. Anything this app keeps is a cache that another client (Desktop,
  CLI, the phone) can make stale, so prefer asking the server to patching local
  state.
- Single-window QML shell: `Controls.ApplicationWindow` with a sidebar + chat
  page; every dialog is a declared instance, a direct child of the window root.
  No pageStack/PageRow anywhere.
- Identity: app id `id.gauvin.Grouse`, target `grouse-desktop` (C++17, CMake
  ≥ 3.16, Qt6 + KF6 Kirigami). QSettings under org `grouse` / app
  `grouse-desktop`.
- README's "Not yet" list (recipes, schedules, projects, skills, extensions,
  charts, steering, attachments) is stale — all of those exist and are wired in
  `ui/main.qml`.

## Architecture & Data Flow

- `Manager` (QML context property `Mgr`) is the thin native facade. It owns NO
  wire client; it holds the three QAbstractListModels (`messageModel`,
  `sessionsModel`, `roamModel`) and a single `CoreBridge`
  (dlopen of `libgrouse_core.so` -> the `grouse_*` C ABI). Events from the core
  arrive on the listing thread, are marshalled onto the Qt main thread by
  `CoreBridge`, and drive the models.
- QML → `Mgr` Q_INVOKABLEs → `grouse_*` intent (via `CoreBridge::api()`) → the
  Rust core → server. Server → core (`on_status`/`on_item`/`on_usage`/
  `on_run_ended`/`on_config`/...) → `CoreBridge` listener → `Manager` `coreOn*`
  slots → models
  → QML delegates.
- Connection lifecycle: `main.cpp` calls `manager.autoConnect()` at startup
  (resumes the last chat when settings exist); `Mgr.connectToServer()` otherwise.
  The core owns connect/open/new and reconnect (exponential backoff, core-side).
- **Roam peers are PARALLEL connections, not replacements.** `connectRoam`
  dials a `goose serve --roam` peer through the core's bundled roam transport
  (no separate dlopen of `libgrouse_roam_core.so` — it is inside
  `libgrouse_core.so`). The sidebar's Main|Roam tabs both stay live; the ROAM
  tab lists endpoints as drop-downs of sessions. Chat-scoped and
  session-bound ops route through the core, which resolves the owning peer from
  the `roam:<label>:<id>` session prefix.
- **Roam ids never cross the wire, and empty chats are unlistable.** The
  `roam:` prefix is client-side: the peer's connection rewrites `sessionId` to
  its raw id at the single rewrite point (`RoamPeer::rpc` → `wire_params`), and
  the peer's `session/list` only returns sessions that HAVE messages. The peer
  layer therefore unions its open session into every emitted `on_roam_sessions`
  list — a just-created chat shows up immediately even though the server can't
  list it yet. Do not "fix" the duplicated strip in the unstable shim, and do
  not drop the union when refactoring `apply_sessions`.
- **The C ABI is the only wire path.** `CoreBridge` resolves the exact
  `grouse_*` symbols from `core/grouse-core/src/capi.rs` and installs the
  `GrouseCoreListener` callback table. The library resolves via `GROUSE_CORE`,
  the app image's `../lib`, `/app/lib`, or the system path. Set `GROUSE_CORE`
  in tests/dev, bundle the .so for the flatpak.
- Transcript rendering: the core owns the transcript and emits it as **rich
  items** on `on_item` (`Upsert` / `AppendText` / `AppendOutput` / `Remove` /
  `Reset` / `Window`, docs/TRANSCRIPT_MODEL.md). The desktop renders from the
  item stream alone — `Manager::coreOnItem` maps an item to a model row and the
  model keys rows by item id, so a re-delivered item updates in place. Text
  streams as `AppendText` deltas (rendered plain) and the core's finalizing
  `Upsert` restores the markdown once per message. The core windows the paint
  (newest ~60); `Mgr.loadOlder(n)` asks for older items and `coreOnItem`
  prepends them. Usage and turn-end are the two non-item families (`on_usage` /
  `on_run_ended`); there is no legacy channel. Never republish a QVariantList to
  `MessageListModel` per event: use the model's `insertRows`/`dataChanged`
  (coalesced via `requestMessagesUpdate`).
- Bubble text is a read-only `TextEdit` (not a `Label`) so it can be selected;
  left-clicking a reply opens a `Popup` with its generation stats, right-clicking
  opens a `Menu` (Copy / Select all / Show stats). `onMessageUsage` stamps the
  `usage` role onto the last agent row — the server's `message_usage` carries no
  message id — so replayed history has no stats (the core cache does not carry
  them either).
- **My Day is a third view of the chat area, not a fourth column.** `MyDayPage`
  takes the slot `ChatPage` occupies (`Mgr.myDayVisible`), opened from the
  sidebar and left with "Back to chat" — opening any session also returns. Data is
  the core's `on_tasks` envelope (CONTRACT §3.6): assistant-state's `item` rows
  (open loops, snoozed included, plus a closed tail) and its `task` rows (work the
  assistant handed over, `running`/`review`) fetched over ONE deterministic
  `tools/call` — no model turn, so the page opens in milliseconds. `Manager`
  decodes it through `src/myday.cpp`; the page only paints. The four intents are
  the server's whole vocabulary: close (`done`/`dropped`, outcome line REQUIRED by
  the workflow), snooze (to the next 05:00 — back before the 06:00 digest), wake
  (a snooze into the past), refresh.
- **A thread label groups rows, and only a crowded one gets a header.** Handed-over
  work and open loops share one label ("chami-billing"), so the Open list
  interleaves them and collapses a label only when more than one row sits under it
  (a single-entry label nested under a chevron costs a tap to reveal the identical
  thing, and My Day held 21 entries over 19 labels at the last count). "Discuss"
  opens that thread's own conversation — a real session filed under the assistant
  PROJECT so goose injects the persona, renamed "Thread: <Label>", hidden from every
  chat list by the core's one filter, and remembered label → session in QSettings.
- Per-session transcript/tool caches live in `QStandardPaths::CacheLocation`,
  keyed by sessionId. Reconnects use exponential backoff.

### Protocol gotchas that WILL bite

- **camelCase vs snake_case**: `recipes/*` and `schedules/*` params are
  snake_case (`cron_schedule`, `file_path`); almost everything else is camelCase.
  A wrong spelling is silently dropped, not rejected.
- **`session/load` rewrites working_dir from the cwd you send.** Never guess a
  cwd: carry the session's real cwd (from `session/list`, via
  `SessionListModel::cwdFor`) or ask the server.
- **Sessions are typed by `_meta.client`.** `session/new` without it is an `acp`
  session, which Desktop/CLI never list. Fresh chats set
  `_meta.client = "grouse-desktop"`.
- **`session/new` needs an absolute cwd** that exists inside the goose container;
  goose has no default. It is asked for at connect time.
- **The live wire carries NO `user_message_chunk` for a prompt you sent** — the
  prompt next appears in a `session/load` replay. The core keeps its own copy of
  a pure-text prompt (`spawn_prompt`), so the client must NOT also echo one: it
  would show twice. `Manager::dispatchSend` therefore echoes only when the send
  carries attachments, which the item model cannot represent (and which are also
  what renders the thumbnails).
- Recipe sessions that declare parameters hard-fail unless
  `clientCapabilities._meta.goose.recipeParameterRequests` is true.
- **TLS is the core's, and this client asks it not to verify.**
  `Manager::serverConfigJson()` sends `accept_invalid_certs = true` (historical:
  goosed used a self-signed cert, we are tailnet-only and authed by the
  `X-Secret-Key` header, and a regenerated cert must not lock us out). There is
  no C++ TLS code any more — the core verifies by default (WebPKI) and only
  skips it because of this flag; Android exposes the same flag as a user toggle.
- Streaming chat: the server sends HTML; the delegate shows `Text.RichText` only
  once `model.html` is set (at finalize), `Text.PlainText` while streaming —
  RichText re-parses the whole chunk every frame (quadratic).
- **A thread mapping is a cache, and the open FAILURE is its only invalidation.**
  Thread chats live in the assistant project, which the core filters out of
  `session/list` — so a mapping cannot be checked against the sidebar (that check
  would discard a live chat and mint a duplicate on every second Discuss; the phone
  had exactly that bug). `coreOnSessionOpenFailed` drops the mapping instead.
- **The composer is a surviving QML object, so `newChat()` clears it from the
  Manager.** `draftRequested(text)` is the one-way push into `chatInput`: "" when a
  conversation starts (a draft belongs to the chat it was typed in), a thread's
  opening line, or the item named when a My Day card is tapped through.
- **DBus exports of nested containers are opaque**: a `Q_SCRIPTABLE` slot
  returning `QVariantList`/`QVariantMap` of maps crosses the bus as unreadable
  `QDBusArgument` objects (the outer shell demarshals, inner maps don't).
  `DbusAdapter::ListSessions` therefore returns a JSON string; the KRunner
  plugin parses it. Keep any new DBus payloads flat (`as`/`s`).

## Key Directories

|Dir|What lives there|
|---|---|
|`src/`|C++ core: `corebridge.{h,cpp}` (dlopen of `libgrouse_core.so`, the ONLY wire path), `manager.{h,cpp}` (thin facade, `Mgr`), `messagelistmodel`, `sessionlistmodel`, `roamlistmodel` (Roam tab), `markdown.{h,cpp}`, `myday.{h,cpp}` (My Day's envelope decode + the due/wake/thread formatters — pure, unit-tested), `appbridge.{h,cpp}` (loopback MCP-Apps host), `notifier.{h,cpp}` (freedesktop notifications), `pushclient.{h,cpp}` (UnifiedPush receive), `dbusadapter.{h,cpp}` (session-bus service for KRunner), `main.cpp`|
|`krunner/`|Host-side KRunner plugin (searches/opens sessions via the DBus service); standalone-buildable CMake, excluded from the flatpak (`BUILD_KRUNNER=OFF`)|
|`ui/`|All QML, embedded into the binary via `qt_add_resources` (PREFIX `/`): `main.qml` (window root, sidebar, all dialog instances), `ChatPage.qml` (transcript/input/attachments/charts, thread panel), `MyDayPage.qml` (the assistant's own lists), feature dialogs|
|`data/`|Desktop file + SVG icon (single scalable icon; no size dirs, no index.theme)|
|`.github/workflows/`|Flatpak build CI (only workflow)|

## Development Commands

```sh
# Local build (inside distrobox 'kde-build')
distrobox enter kde-build -- bash -lc 'cmake -B build -S . -DCMAKE_BUILD_TYPE=Debug && cmake --build build -j$(nproc)'
```

- **KRunner plugin**: the runner is a HOST component — the flatpak sandbox
  can't install into the host's plugin dirs, and the flatpak SDK has no
  KRunner dev headers, so the plugin is built by the CI/local build
  (`build-krunner.sh --build-only`, against a current KF6) and shipped
  inside the flatpak at `/app/lib/grouserunner.so`. On first run the app
  copies it to `~/.local/lib/qt6/plugins/kf6/krunner` and writes
  `~/.config/plasma-workspace/env/grouse-krunner.sh` (QT_PLUGIN_PATH) — the
  manifest grants exactly those two paths (`--filesystem=…:create`). The
  plugin takes effect on the next KRunner spawn; the env file at the next
  login. For native (non-flatpak) builds, `./build-krunner.sh` does the
  same install directly. The host is an atomic Fedora (Bazzite): `/usr` is
  read-only and `dnf install` is blocked, so the plugin must live in the
  user dir — never copy `.so` files by hand, and don't put the plugin in
  the system dirs.

- Fresh container needs `qt6-qtbase-devel qt6-qtdeclarative-devel
  qt6-qtwebsockets-devel kf6-kirigami-devel kf6-kcoreaddons-devel
  kf6-krunner-devel kf6-kservice-devel kf6-ki18n-devel extra-cmake-modules
  kf6-qqc2-desktop-style` (CMakeLists runs `find_package(ECM REQUIRED)`; without
  the last one the `org.kde.desktop` QQuickStyle in `main.cpp` fails to load:
  "module org.kde.desktop is not installed") plus `xorg-x11-server-Xvfb`.
- Quick static QML check (catches "X is not a type" without a display):

```sh
distrobox enter kde-build -- bash -lc 'qmllint -I /usr/lib64/qt6/qml -I ui ui/*.qml'
```

  Unqualified `Mgr`/`model` warnings are expected (context property / delegate
  role); a "is not a type" error is the real signal.
- True page-load check: `QT_QPA_PLATFORM=offscreen` only loads `main.qml` —
  Kirigami defers creating `pageStack.initialPage` until a real render, so type
  errors inside `ChatPage.qml`/dialogs are silent offscreen. Render on a real X
  server instead:

```sh
distrobox enter kde-build -- bash -lc 'xvfb-run -a -s "-screen 0 1280x800x24" timeout 8 ./build/grouse-desktop'
```

  Silence = all QML loaded.
- No distrobox on the machine? Neither is required — the same package set
  installs into a throwaway Fedora image and the build runs there:

```sh
podman run --rm -it --userns=keep-id -v "$PWD/../../..:/work" -w /work/clients/desktop \
  fedora:44 bash -lc 'dnf install -y qt6-qtbase-devel qt6-qtdeclarative-devel \
    kf6-kirigami-devel kf6-kcoreaddons-devel kf6-krunner-devel kf6-kservice-devel \
    kf6-ki18n-devel extra-cmake-modules kf6-qqc2-desktop-style gcc-c++ cmake \
    ninja-build xorg-x11-server-Xvfb && cmake -B build -S . && cmake --build build -j"$(nproc)"'
```

  Headless hosts need `QT_QPA_PLATFORM=offscreen` for `ctest` (the suites are
  QApplication-based; without a display they abort before the first test).
- Test suite (CTest; see Testing & QA):

```sh
distrobox enter kde-build -- bash -lc 'cd build && ctest --output-on-failure'
```
- Flatpak bundle: `./build-flatpak.sh` → `grouse-desktop.flatpak` (state in
  `$XDG_CACHE_HOME/grouse-flatpak`, env-overridable
  `STATE_DIR`/`BUILD_DIR`/`REPO_DIR`/`BUNDLE`). The script runs host-side steps
  directly but invokes `flatpak-builder` INSIDE the `kde-build` distrobox on
  purpose: the manifest's `base: io.qt.qtwebengine.BaseApp` makes the ostree
  checkout restore SELinux labels the host user context cannot set (`build-init`
  dies with `lsetxattr(security.selinux): Operation not supported`); the
  container's `spc_t` domain can, and nested bwrap works on the current flatpak
  generation (an older note about a `/oldroot/etc/resolv.conf` mount failure was
  a previous container generation). The host's own `flatpak` is still what you
  install/bundle with; only the builder needs the container. The host therefore
  needs just `flatpak` + `distrobox`; `build-flatpak.sh` checks for
  `flatpak-builder` inside `kde-build`, not on the host (atomic Fedora has no
  host `flatpak-builder`). Footgun:
  flatpak-builder can "check out
  last cache hit" and ship a bundle with STALE source (the export reports
  `Content Written: 0`). `build-flatpak.sh` now drops the app module's cached
  build before every run (`rm -rf $STATE_DIR/builder-cache/build/grouse-desktop-*`)
  so source edits always rebuild; to verify a deployed bundle is fresh, decompress
  its zstd qrc frames and grep for a marker from the newest commit (plain `strings`
  misses everything — Qt6 rcc compresses the QML). If a rebuild seems to change
  nothing else, delete the state dir (`rm -rf ~/.cache/grouse-flatpak`) and
  rebuild. The builder cache now lives under the state dir, not the repo root.
  SECOND, worse cache layer (2026-09-18): Qt's compiled-QML DISK cache
  (`~/.var/app/id.gauvin.Grouse/cache/grouse/grouse-desktop/qmlcache`) keeps
  bytecode for `qrc:/` URLs keyed on timestamps rcc fixes at the epoch — an
  in-place flatpak update NEVER invalidates it, and the app renders OLD QML
  from a byte-verified-NEW binary (symptom: "the fix isn't working" while a
  host-built binary of the same source renders correctly on the same desktop).
  `main.cpp` now sets `QML_DISABLE_DISK_CACHE=1` before QApplication (all QML
  is embedded, so the cache had no upside). If you ever see stale QML again:
  `rm -rf ~/.var/app/id.gauvin.Grouse/cache/grouse/grouse-desktop/qmlcache`
  and restart.

## Code Conventions & Common Patterns

- Match the surrounding code; comments explain *why* something is not the obvious
  thing (several protocol mines above started as an obvious wrong change). Do not
  restate code in comments.
- Prefer KDE/Qt libs over reimplementing format handling, and prefer solving a
  problem client-side over changing the goose fork — a fork carries every change
  through every rebase forever.
- camelCase everywhere: QML properties, C++ methods, `Mgr` Q_PROPERTYs.
- QML ↔ C++: read via context properties / `model.<role>`; write via Q_INVOKABLE
  calls and signals. Never two-way bindings — QQC2 TextField/TextArea break
  `text:` bindings on user edit (dialogs push one-way on `onTextChanged`/commit
  on `onClosed`; two-way `text:`/`onTextChanged` loops are a known footgun).
- ComboBox models: when the model is a JS array of `{value, name}` objects, a
  ComboBox MUST set `valueRole: "value"` — without it `currentValue` returns
  the model ITEM (a JS object) and `Mgr.setConfigOption` silently receives
  "[object V4ReferenceObject]" (provider/model switches broke this way).
  Regression-tested in `tests/qml/tst_combobox.qml`.
- Delegate rules: C++ model roles via `model.<role>` on an INLINE delegate (a
  separate-file delegate loses the `model` context for root-object property
  bindings); plain JS arrays (skills, recipes, schedules, toolGroups,
  permissionOptions…) use `modelData` + `index`. Nested Repeaters shadow
  `modelData` — capture the outer value in a property first.
- QQC2 dialogs need explicit width/height (implicit sizes collapse); a vertical
  ScrollBar overlays the ListView viewport (bubbles reserve `scrollbarReserve`);
  popups position via x/y relative to parent, not anchors; an OverlayDrawer must
  be a direct window child.
- Chat HTML arrives from the server (`model.html`); `markdownToHtml()`
  (Qt CommonMark → HTML, HTML-escapes all text) is used client-side for bubbles
  the server didn't HTML-ify.
- Dialogs: declared instances, direct window children, opened with `.open()`
  after setting args (`projectDialog.openFor(id, name)`); no
  `Component.createObject` anywhere. `PermissionDialog` has
  `closePolicy: NoAutoClose`, but `onClosed` still answers deny (Escape = deny).
- `ChartBubble.qml` renders Chart.js-style JSON specs on native Canvas 2D (no web
  engine); Canvas has no implicitHeight, so the delegate precomputes `chartH`.

## Important Files

|File|Role|
|---|---|
|`src/corebridge.h/.cpp`|The ONLY path to the wire: dlopens `libgrouse_core.so`, resolves every `grouse_*` symbol, installs the `GrouseCoreListener` table, marshals every core event from the worker thread onto the Qt main thread, and dispatches to `Manager`'s `coreOn*` handlers. The core owns the ACP connection, transcript, streaming, reconnect and the roam transport.|
|`src/manager.h/.cpp`|Thin native facade (`Mgr` context property): ~30 Q_INVOKABLEs (`sendPrompt`, `openSession`, `renameSession`, `setConfigOption`, `refreshRecipes`, `runRecipe`, `scheduleRecipe`, `saveSkill`, `setGlobalExtensionEnabled`, `respondPermission`, `pickAttachmentFiles`, `exportSessionTo`, `compactConversation`, …), writable Q_PROPERTYs (host/port/secretKey/workingDir/useTls/autoConnectEnabled), status Q_PROPERTYs (status/online/landingPage/prompting/compacting/queuedCount/contextSize/…). Every intent routes through `CoreBridge::api()`; `coreOn*` fold core events into the models.|
|`src/messagelistmodel.h/.cpp`|Transcript model, 16 roles (role/text/html/title/detail/output/status/images/usage/chartData/appHtml/calls/…); `updateDeferred` coalesces dataChanged on a 120 ms QTimer; `commitDeferred` guards the cleared-model race.|
|`src/sessionlistmodel.h/.cpp`|Sidebar model, 11 roles; flat list of group-header + session rows; sections prefixed `proj:`/`peer:` (id = `section.substring(5)`); `cwdFor()` is the source of cwd for `session/load` — never invent one; `toggleSection()`; no-op guard so unchanged lists never reset the model. The collapsed set is the user's state: Manager restores it from QSettings (`sidebar_collapsed`) at startup and persists it on `collapsedChanged` (`collapsedSections()`/`setCollapsedSections()`), so the sidebar opens the way it was left.|
|`src/markdown.h/.cpp`|`markdownToHtml()` — Qt CommonMark → HTML, escapes input.|
|`src/myday.h/.cpp`|My Day's pure half: the `on_tasks` decode (snake_case rows → the camelCase maps the page binds) and `dueLabel`/`wakeLabel`/`nextMorning`/`threadName`/`artifactsLabel`/`metaLine`. Clock and day are ARGUMENTS, so every branch is unit-tested.|
|`src/appbridge.h/.cpp`|Loopback HTTP host for MCP Apps (the flatpak runtime has no QtWebEngine): serves the app template with a host shim injected on 127.0.0.1, answers `ui/initialize`, and delivers `ui/message` back into the originating chat. Routes are scoped by per-app opaque tokens; template + shim are server-supplied and untrusted.|
|`src/notifier.h/.cpp`, `src/pushclient.h/.cpp`|Notifications: freedesktop notifications driven by the core's shared policy (this client supplies `app_visible`/window state), plus the UnifiedPush receive path (register + decode the envelope; the repo ships no sender).|
|`src/main.cpp`|QApplication (widgets needed for QFileDialog), QSettings org/app, `org.kde.desktop` style, `Mgr` context property, `manager.autoConnect()`.|
|`ui/main.qml`|Window shell: `Controls.ApplicationWindow` (NOT Kirigami's — its `contentItem` is read-only, so a sidebar+chat split can't be injected there); sidebar + ChatPage/landing page; every dialog instance and inline menu/dialog; landing provider/model ComboBoxes.|
|`ui/ChatPage.qml`|Transcript ListView with pinned-to-end scroll logic, input bar, mode menu, attachments, ChartBubble, permission routing, and the thread panel a thread chat carries above its transcript.|
|`ui/MyDayPage.qml`|My Day: the assistant card, the grouped Open list (a label gets a header only when it holds more than one row), the handed-over tasks, Snoozed, Recently done. Rows are Loaders over inline components; `entryModel` is a binding, so a refresh or a toggle rebuilds the sequence once.|
|`ui/*Dialog.qml`|ConnectDialog, SettingsDialog, ProvidersDialog, GlobalExtensionsDialog, SkillsDialog, RecipesDialog (carries the schedule controls too — no separate Scheduler screen), ProjectDialog, PermissionDialog.|
|`CMakeLists.txt`|Qt6 (Quick Qml QuickControls2 Widgets DBus Network, plus optional `WebEngineQuick` for inline MCP Apps) + ECM + KF6 (Kirigami CoreAddons); `qt_add_resources` embeds ALL `ui/*.qml`; app install rules only — the test targets live under `tests/` (wired via `include(CTest)` + `add_subdirectory(tests)`).|
|`id.gauvin.Grouse.json` / `build-flatpak.sh`|Flatpak manifest (org.kde.Platform/Sdk 6.10, `io.qt.qtwebengine.BaseApp`, cmake-ninja Release) + bundle script.|
|`.github/workflows/flatpak.yml` (repo root)|CI: `host-components` builds the KRunner plugin and `libgrouse_core.so`, then `flatpak` bundles them — no tests, no lint gates.|

## Runtime/Tooling Preferences

- Native build: Fedora distrobox `kde-build`, Makefile generator, Debug, into
  `build/`. Flatpak: cmake-ninja, Release. CI: flathub-infra kde-6.10 container.
- All QML is compiled into the binary via qrc — after changing `ui/*.qml` you
  must rebuild before seeing the change.
- No lint or test gates run in CI; `qmllint` is a manual step (see commands).
- Gitignore covers `build/`, `.cache/`, `*.user`, `*.flatpak`,
  `.flatpak-builder/`, `flatpak-repo/`. Footgun: a stray untracked build tree in
  a directory literally named `# build/` is NOT covered by the `build/` pattern —
  don't commit it.

## Testing & QA

- Automated tests live in `tests/` (CTest; `BUILD_TESTING` defaults ON for dev
  builds, OFF in the flatpak manifest). Run everything with
  `ctest --output-on-failure` from `build/`, or one suite with
  `ctest -R tst_manager --verbose`.
- Suite layout (all share the `grouse_core` static lib = `src/*` minus
  `main.cpp`):
  - `tst_markdown`, `tst_messagelistmodel`, `tst_sessionlistmodel`,
    `tst_roamlistmodel` — unit tests for the pure logic (escaping,
    deferred/coalesced model updates, grouping / collapse / cwdFor / no-op reset
    guard, peer headers + sessions).
  - `tst_appbridge` — the loopback MCP-Apps host: per-app token scoping, the
    injected host shim, and `ui/message` delivery back into the chat.
  - `tst_myday` — My Day's envelope decode (three sections, per-section errors,
    id-less rows dropped, artifacts) and the formatters: due labels in BOTH
    directions (the sign of a day difference is what a port gets wrong once),
    snooze/wake, `nextMorning`'s 05:00 rule, thread names, the meta line.
  - `tst_manager` — thin-client intent-mapping smoke: Manager Q_INVOKABLE ->
    CoreBridge routing and model invariant checks. The wire-level protocol
    (handshake, session/load, permissions, streaming, every reply parser) is
    covered by the grouse-core Rust crate (`cargo test -p grouse-core`), not
    here — there is no C++ wire client to test.
  - `tst_qml` — QtQuickTest run of `tests/qml/`: ChartBubble (chart-spec parsing
    and axis math), ComboBox `valueRole` (provider/model wiring contract),
    RecipesDialog, and MyDayPage (the thread-grouping rule plus the delegate
    tap-throughs — the one place a Loader over an inline component is exercised at
    all, so a TypeError there is a test failure and not a silently missing row).
- QML test gotcha: QuickTest resolves relative imports against neither the test
  file nor CWD. The `ui/` import URL is baked in at configure time via
  `configure_file` (template `tests/qml/tst_chartbubble.qml.in` → generated file
  in the build dir), keeping machine-specific paths out of committed files.
- A page that consumes the design tokens does `import Grouse 1.0`, and the app
  registers that singleton in `main.cpp` from the **qrc** — which a test binary
  does not have, so MyDayPage refused to load with `module "Grouse" is not
  installed` until `tst_qml` registered `ui/GrouseTheme.qml` itself
  (`GROUSE_THEME_URL`, baked in at configure time for the same
  no-machine-paths reason). A new token-consuming page inherits that for free.
- No test jobs run in CI (the flatpak workflow builds the bundle only) — run
  `ctest` locally after touching `src/` or `ui/`.
- Manual QA still applies on top: `qmllint` for static QML errors, `xvfb-run`
  under a real X server for full page load (silence = success), and a smoke test
  against a live `goose serve` for the changed path.

## Working in this checkout

- Two agents share this checkout (host + goose container). Check `git status` and
  `git diff` before starting; uncommitted changes may be the other agent's.
- Never hardcode a machine-specific path into a shared file.
- Do not commit or push unless asked.
