# macOS — the SwiftUI client (not started)

The plan for macOS is the same shape as Android: a thin native UI over the one Rust core,
consuming the **uniffi Swift bindings**. `clients/macos/spike/` is the de-risking step; there
is no app yet.

Read `AGENTS.md` at the repo root for the architecture and `docs/PARITY.md` for the ledger a
new client joins.

## Why a spike first

Everything protocol-side is already in `core/` (transport, TLS, roam/iroh, the session
pipeline, transcript windowing, cache policy, permissions, recipes, tasks, the advisor).
None of it is written on the Mac side yet — `clients/macos` did not exist — so the first
question is not "how big is the UI", it is **does the binding boundary work on Darwin at
all**: does uniffi's Swift output compile against this interface, do the 23 listener
callbacks arrive, do the async intents round-trip, does a real TLS WebSocket come up.

The spike answers that in an afternoon instead of a month into a UI. It is a CLI on
purpose: no settings, no storage, no AppKit. If it runs, the phases below hold.

## Run it

On the Mac (it cannot be built on the Linux host — no Apple SDK, no `lipo`, no `swift`):

```sh
cd clients/macos/spike
./build.sh                       # Rust ×3, lipo, bindgen, SwiftPM, swift build

GROUSE_HOST=goose.gauvin.id GROUSE_PORT=443 GROUSE_KEY=<secret> \
  ./.build/debug/grouse-spike
```

What it does, and what each step proves:

| step | proves |
|---|---|
| `connect` | the TLS WebSocket + `X-Secret-Key` transport works from a Rust core inside a Swift process |
| `listSessions` | an intent → reply round-trip, and that listener events (`onSessions`) cross the FFI boundary |
| `openSession` | the transcript stream: `onItem`/`Upsert`/`AppendText`/`Window` arrive with usable rows, and the cache dir is usable |
| `sendPrompt` | a full turn: prompt in, streamed reply out, `onRunEnded` |
| `onUsage` | the token accounting a context donut would show |

Flags: `--no-send` stops before prompting. Env: `GROUSE_TLS=0` for a plain-ws dev host,
`GROUSE_CWD` (must exist **inside the goose container** — `session/new` requires one),
`GROUSE_CACHE` (defaults to `~/Library/Caches/grouse-spike`).

## What the spike already found

- **The bindings generate cleanly**: 9,231 lines of `grouse_core.swift` plus 930 for roam,
  from `uniffi-bindgen-swift`. The interface is Swift-shaped as-is: `Core(listener:cacheDir:)`,
  `connect(config:)`, `openSession(sessionId:)`, `sendPrompt(prompt:expect:)`, and a
  `CoreListener` protocol for everything the core pushes.
- **Bindings must come from a DEBUG build.** The workspace's release profile sets
  `strip = true`, which removes the uniffi metadata sections bindgen reads — the Android
  script already documents this for Kotlin (`scripts/build-android-libs.sh`), and it bites
  the Swift path identically. Only the `.a` that gets linked is a release build.
- **`CoreListener` has 23 requirements and no defaults.** Every conformance would have to
  stub all of them, and each new listener event the core gains breaks that conformance.
  `Sources/GrouseCore/ListenerDefaults.swift` supplies no-op defaults via a protocol
  extension — one file, and a conformance then implements only what it uses.

## What CI checks (and what it cannot)

The Swift builds **only on a Mac**: this account has no macOS runners — a `runs-on:
macos-14` job is cancelled before it starts ("The configuration 'macos-14-arm64-us-default'
is not supported"; macOS minutes are not free for a private repo). So the first version of
`.github/workflows/macos.yml` was replaced with a job that runs on Ubuntu and guards the
two invariants that need no Darwin:

1. **The bindings still generate.** `check-bindings.py` runs `uniffi-bindgen-swift` against
   a debug host build, so a core change that breaks Swift generation is caught here and not
   on the Mac.
2. **`ListenerDefaults.swift` keeps up.** It compares the generated `CoreListener`
   protocol's requirements against the defaults and fails with the exact signature to paste
   — which is what stops a new core event from silently breaking a conformance that only
   compiles on someone's laptop.

Run it yourself: `python3 clients/macos/spike/check-bindings.py <path-to-libgrouse_core.so>`
(a debug build — see the strip note above).

## If the spike passes: the app, in phases

Estimates for one person; the repo's own rules make the middle phases bigger than they look.

| phase | work | rough |
|---|---|---|
| 0 | the spike (above) | 2–3 d |
| 1 | app shell, sidebar (sessions/projects/roam), mode choice at creation, chat surface, composer, attachments/images, provider+model picker | 1.5–2 wk |
| 2 | the three behaviours that are RULES, not code: incremental transcript updates (never republish the list), newest-60 windowing with `load_older` anchoring, and scroll pinning (pin from layout, decide on *settle*, stop when content height stops changing). SwiftUI gives none of it for free | 1 wk |
| 3 | Settings + Dev, context panel, permission/elicitation dialogs, advisor card, recipes, schedules, skills, extensions, My Day, threads | 2–3 wk |
| 4 | MCP Apps: `WKWebView` plus a loopback host (the port of `appbridge.cpp`), or the browser-handoff path only | 3–5 d |
| 5 | OS integration: notifications + "is the app frontmost" (the core already owns the policy), speech over `speak.rs` chunks, dictation, a global entry point (no KRunner here — an App Intent, or an `excluded:` row) | 3–5 d |
| 6 | Release: codesign, notarize, DMG, a `macos.yml` workflow | 1–2 d |
| 7 | The ratchet (below) | 1 d |

**~6–9 weeks for real parity; ~2 weeks to something you would use daily** (0–2 plus
settings and notifications).

## Joining the parity ratchet — deliberately last

`scripts/check-parity.py` discovers an active client from sources, but only for names in its
`CLIENT_SUFFIXES` map. `macos` is not in it, so this directory does **not** yet demand a
column — that is intentional while it is a spike. The moment it should count:

1. add `"macos": (".swift",)` to `CLIENT_SUFFIXES`,
2. add a `macos` column to `docs/PARITY.md` with a disposition for all 90-odd rows
   (`yes: path#token` anchors that the checker greps, or `excluded:` with a reason),
3. raise the pinned `excluded = N` by exactly the number of exclusions added.

Do 1 without 2 and CI fails with 90 violations at once — which is the ratchet working, and
also the reason not to register the client until the app has surfaces to point at.
