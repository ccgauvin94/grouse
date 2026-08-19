# Repository Guidelines

Grouse Desktop — a KDE-native (Kirigami/Qt6/C++) client for a self-hosted goose
(`goose serve`), spoken to over ACP (JSON-RPC over WebSocket).

This lives in the grouse monorepo at `clients/desktop/`. **Read the repo-root
`AGENTS.md` first** — it owns the architecture contract, and it records this
directory as a known deviation from it: the desktop predates the Rust core and
still implements the client logic itself (`src/acpclient.*`, `src/manager.*`,
`src/*transport.*`) instead of consuming `grouse-core`. Until that migration
lands, a protocol fix here must also be made in `core/`. This file documents the
C++ side.

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

- `Manager` (QML context property `Mgr`) owns exactly one `AcpClient` plus the two
  QAbstractListModels. Everything runs on the main thread with direct connects.
- QML → `Mgr` Q_INVOKABLEs → `AcpClient` RPC → server. Server → notifications →
  `AcpClient` signals → `Manager` slots → model `insertRows`/`dataChanged` → QML
  delegates.
- Connection lifecycle: `main.cpp` calls `manager.autoConnect()` at startup
  (resumes the last chat when settings exist); `Mgr.connectToServer()` otherwise.
  The MAIN connection is rebuilt for every connect/open/new; resume
  (`session/load`) vs fresh (`session/new`) is the `setResumeSession` switch.
- **Roam peers are PARALLEL connections, not replacements.** Each `RoamPeer`
  (label → own `AcpClient` + `RoamTransport`) connects straight to a
  `goose serve --roam` host over iroh (native `libgrouse_roam_core.so`,
  dlopen'd — see the transport seam below), in browse mode (initialize →
  `session/list`, never auto-opens). The sidebar's Main|Roam tabs both stay
  live; the ROAM tab lists endpoints as drop-downs of sessions. Chat-scoped
  ops route through `Manager::activeClient()` (the peer owning the active
  session, else the main client); global catalogs (recipes/schedules/skills/
  projects/config) always hit the main client. The device iroh identity is
  generated on first view and stored in QSettings (`roam_identity`); hosts see
  its key in `peers list` and must `roam peers accept` it before the dial
  succeeds.
- **Transport seam**: `AcpClient` speaks to an `AcpTransport` (signals
  opened/textReceived/closed/error + `sendText`). `WebSocketTransport` wraps
  the old QWebSocket path; `RoamTransport` dlopens grouse-roam-core and pumps
  newline-framed ACP (see `RoamFrameCodec`, ACP's ByteStreams framing — same
  as goose on stdio). The native lib resolves via `GROUSE_ROAM_CORE`, the app
  image's `../lib`, `/app/lib`, or the system path; its C API lives in the
  crate's `src/capi.rs`. Set `GROUSE_ROAM_CORE` in tests/dev, bundle the .so
  for the flatpak.
- Streamed chunks accumulate into `MessageListModel` incrementally (insertRows +
  deferred dataChanged) — never republish a QVariantList: QML treats it as a
  brand-new model (full reset, scroll jump, quadratic slowdown).
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
- Recipe sessions that declare parameters hard-fail unless
  `clientCapabilities._meta.goose.recipeParameterRequests` is true.
- **WebSocket TLS is deliberately trust-all** (`setSslConfiguration(VerifyNone)`):
  goosed uses a self-signed cert; we are tailnet-only and authed by the
  `X-Secret-Key` header. A regenerated cert must not lock us out.
- Streaming chat: the server sends HTML; the delegate shows `Text.RichText` only
  once `model.html` is set (at finalize), `Text.PlainText` while streaming —
  RichText re-parses the whole chunk every frame (quadratic).
- **DBus exports of nested containers are opaque**: a `Q_SCRIPTABLE` slot
  returning `QVariantList`/`QVariantMap` of maps crosses the bus as unreadable
  `QDBusArgument` objects (the outer shell demarshals, inner maps don't).
  `DbusAdapter::ListSessions` therefore returns a JSON string; the KRunner
  plugin parses it. Keep any new DBus payloads flat (`as`/`s`).

## Key Directories

|Dir|What lives there|
|---|---|
|`src/`|C++ core: `acpclient.{h,cpp}` (the wire), `manager.{h,cpp}` (connection + chat state, `Mgr`), `messagelistmodel`, `sessionlistmodel`, `markdown.{h,cpp}`, `dbusadapter.{h,cpp}` (session-bus service for KRunner), `main.cpp`|
|`krunner/`|Host-side KRunner plugin (searches/opens sessions via the DBus service); standalone-buildable CMake, excluded from the flatpak (`BUILD_KRUNNER=OFF`)|
|`ui/`|All QML, embedded into the binary via `qt_add_resources` (PREFIX `/`): `main.qml` (window root, sidebar, all dialog instances), `ChatPage.qml` (transcript/input/attachments/charts), feature dialogs|
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
  qt6-qtwebsockets-devel kf6-kirigami-devel extra-cmake-modules
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
- Test suite (CTest; see Testing & QA):

```sh
distrobox enter kde-build -- bash -lc 'cd build && ctest --output-on-failure'
```
- Flatpak bundle: `./build-flatpak.sh` → `grouse-desktop.flatpak` (state in
  `$XDG_CACHE_HOME/grouse-flatpak`, env-overridable
  `STATE_DIR`/`BUILD_DIR`/`REPO_DIR`/`BUNDLE`). Run it on the HOST, not inside
  the `kde-build` distrobox: nested flatpak-builder there breaks with a
  bubblewrap `/oldroot/etc/resolv.conf` mount error. The host has no
  `flatpak-builder` — copy it from the container first:
  `distrobox enter kde-build -- bash -lc 'cp /usr/bin/flatpak-builder /var/home/colin/flatpak-builder-host'`
  then `ln -s /var/home/colin/flatpak-builder-host ~/.local/bin-shim/flatpak-builder`
  and put `~/.local/bin-shim` on PATH. Footgun: flatpak-builder can "check out
  last cache hit" and ship a bundle with STALE source (the export reports
  `Content Written: 0`). If a rebuild seems to change nothing, delete the state
  dir (`rm -rf ~/.cache/grouse-flatpak`) and rebuild. The builder cache now
  lives under the state dir, not the repo root.

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
|`src/acpclient.h/.cpp`|The wire: initialize handshake (clientCapabilities), `session/new` vs `session/load`, response/notification/serverRequest dispatch, goose extensions (`session/request_permission`, `session/set_config_option`, `_goose/unstable/session/steer`, recipes/schedules/skills/projects/global-extensions). ~30 outbound RPC methods + ~30 signals; `friend class Manager`.|
|`src/manager.h/.cpp`|Process-scoped connection + chat state; `Mgr` context property: ~30 Q_INVOKABLEs (`sendPrompt`, `openSession`, `renameSession`, `setConfigOption`, `refreshRecipes`, `runRecipe`, `scheduleRecipe`, `saveSkill`, `setGlobalExtensionEnabled`, `respondPermission`, `pickAttachmentFiles`, `exportSessionTo`, `compactConversation`, …), writable Q_PROPERTYs (host/port/secretKey/workingDir/useTls/autoConnectEnabled), status Q_PROPERTYs (status/online/landingPage/prompting/compacting/queuedCount/contextSize/…).|
|`src/messagelistmodel.h/.cpp`|Transcript model, 16 roles (role/text/html/title/detail/output/status/images/usage/chartData/appHtml/calls/…); `updateDeferred` coalesces dataChanged on a 120 ms QTimer; `commitDeferred` guards the cleared-model race.|
|`src/sessionlistmodel.h/.cpp`|Sidebar model, 11 roles; flat list of group-header + session rows; sections prefixed `proj:`/`peer:` (id = `section.substring(5)`); `cwdFor()` is the source of cwd for `session/load` — never invent one; `toggleSection()`; no-op guard so unchanged lists never reset the model.|
|`src/markdown.h/.cpp`|`markdownToHtml()` — Qt CommonMark → HTML, escapes input.|
|`src/main.cpp`|QApplication (widgets needed for QFileDialog), QSettings org/app, `org.kde.desktop` style, `Mgr` context property, `manager.autoConnect()`.|
|`ui/main.qml`|Window shell: `Controls.ApplicationWindow` (NOT Kirigami's — its `contentItem` is read-only, so a sidebar+chat split can't be injected there); sidebar + ChatPage/landing page; every dialog instance and inline menu/dialog; landing provider/model ComboBoxes.|
|`ui/ChatPage.qml`|Transcript ListView with pinned-to-end scroll logic, input bar, mode menu, attachments, ChartBubble, permission routing.|
|`ui/*Dialog.qml`|ConnectDialog, SettingsDialog, ProvidersDialog, GlobalExtensionsDialog, SkillsDialog, RecipesDialog, SchedulerDialog, ProjectDialog, PermissionDialog.|
|`CMakeLists.txt`|Qt6 (Quick Qml QuickControls2 WebSockets Widgets) + ECM + KF6 (Kirigami CoreAddons); `qt_add_resources` embeds ALL `ui/*.qml`; app install rules only — the test targets live under `tests/` (wired via `include(CTest)` + `add_subdirectory(tests)`).|
|`id.gauvin.Grouse.json` / `build-flatpak.sh`|Flatpak manifest (org.kde.Platform/Sdk 6.10, cmake-ninja Release) + bundle script.|
|`.github/workflows/flatpak.yml`|CI: builds and uploads the Flatpak bundle only — no native build, no tests, no lint gates.|

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
  `ctest -R tst_acpclient --verbose`.
- Suite layout (all share the `grouse_core` static lib = `src/*` minus
  `main.cpp`):
  - `tst_markdown`, `tst_messagelistmodel`, `tst_sessionlistmodel` — unit tests
    for the pure logic (escaping, deferred/coalesced model updates, grouping /
    collapse / cwdFor / no-op reset guard).
  - `tst_acpclient` — 30 wire tests against an in-process fake goose
    (`tests/fakeserver.h`, a scriptable QWebSocketServer): handshake
    capabilities, `_meta.client`, cwd, load-failure fallback, steer, permissions,
    streaming, every reply parser, `toExtensionDto`. This is where the protocol
    gotchas are pinned down — change the wire, expect a failure here.
  - `tst_manager` — integration end to end: connect → open → prompt → streamed
    chunks → model rows, permissions, queue/steer, error bubbles. QSettings and
    caches are redirected via XDG_CONFIG_HOME/XDG_CACHE_HOME so tests never
    touch real user config.
  - `tst_qml` — QtQuickTest run of `tests/qml/`: ChartBubble (chart-spec parsing
    and axis math) and ComboBox `valueRole` (provider/model wiring contract).
- QML test gotcha: QuickTest resolves relative imports against neither the test
  file nor CWD. The `ui/` import URL is baked in at configure time via
  `configure_file` (template `tests/qml/tst_chartbubble.qml.in` → generated file
  in the build dir), keeping machine-specific paths out of committed files.
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
