# grouse-desktop

KDE-native (Kirigami / Qt 6 / C++) desktop thin client for a self-hosted
[goose](https://github.com/block/goose) agent (`goose serve`), spoken to over
ACP (Agent Client Protocol). This is the Agentic AI Foundation client for the
desktop — the counterpart to the Android client in `ccgauvin94/grouse`.

As with the phone app, this is a **dumb pipe**: `goosed` on the server holds all
state (sessions, memory, tools, model chain). The app is an ACP JSON-RPC client
over a tailnet-only WebSocket plus a chat UI.

## Server contract (see the Android repo's `AGENTS.md` for the full notes)
- Endpoint: `wss://<host>:3284/acp` — goosed serves TLS with a self-signed cert,
  so the client is trust-all (see `AcpClient::connectTo`) and authenticated by the
  key header. The TLS/wss toggle lives in the Connect dialog.
- Auth: `X-Secret-Key: <GOOSE_SECRET_KEY>` (header on the WS upgrade)
- Transport: keep it on tailnet/LAN — goosed is RCE-capable, don't expose 3284
  to the internet (a public Caddy in front returns 403 to everything on 443).
- `session/new` needs an absolute `cwd` that exists inside the goose container
  (e.g. `/home/colin`); it has no default, so it's asked for at connect time.

## Build (Bazzite — no host dev tools, use the distrobox)

```sh
distrobox enter kde-build -- bash -lc 'cmake -B build -S . -DCMAKE_BUILD_TYPE=Debug && cmake --build build -j$(nproc)'
./build/grouse-desktop
```

The `kde-build` distrobox already has Qt 6 + Kirigami 6 + QtWebSockets.
`offscreen` only loads `main.qml` (Kirigami defers page creation), so validate
with a real X server:

```sh
distrobox enter kde-build -- bash -lc 'sudo dnf install -y xorg-x11-server-Xvfb'
xvfb-run -a -s "-screen 0 1280x800x24" timeout 8 ./build/grouse-desktop
```

## Scope (current)
- Connect (host, port, secret key, server working dir)
- Session list + resume (replay) via `session/load`
- New chat via `session/new` (filed as a `user` session so Desktop can see it)
- Streaming transcript: agent/user/thought chunks, tool calls + updates
- Tool-approval prompts (`session/request_permission`) rendered as a dialog
- Provider / model selectors via `session/set_config_option`
- Markdown→HTML rendering for agent output

Not yet: recipes, schedules, projects, skills, extensions, MCP-App
visualizations, steering, image attachments.

## Layout
| File | What lives there |
|---|---|
| `src/acpclient.h/.cpp` | The wire: ACP JSON-RPC over QtWebSockets |
| `src/manager.h/.cpp` | Process-scoped connection + chat state, exposed to QML as `Mgr` |
| `src/markdown.h/.cpp` | Lightweight markdown→HTML renderer |
| `src/main.cpp` | App setup + QML engine |
| `ui/*.qml` | Kirigami windows / pages / dialogs |

## Agents
Check `git status`/`git diff` before starting — this tree is edited from both
the host and inside the goose container (see the Android repo's AGENTS.md for
the two-agents-one-checkout hazards). Do not commit or push unless asked.
