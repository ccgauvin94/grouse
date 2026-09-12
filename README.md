# grouse

Native chat clients for a [goose](https://github.com/aaif-goose/goose) agent you
run yourself.

goose runs on a server (`goose serve`); grouse is what talks to it. Right now
that's an Android app and a KDE desktop app, with a terminal client planned. The
server owns sessions, memory, tools, and model choice. The clients render the
transcript and send your prompts.

[![Core](https://github.com/ccgauvin94/grouse/actions/workflows/core.yml/badge.svg)](https://github.com/ccgauvin94/grouse/actions/workflows/core.yml)
[![Android](https://github.com/ccgauvin94/grouse/actions/workflows/android.yml/badge.svg)](https://github.com/ccgauvin94/grouse/actions/workflows/android.yml)
[![Flatpak](https://github.com/ccgauvin94/grouse/actions/workflows/flatpak.yml/badge.svg)](https://github.com/ccgauvin94/grouse/actions/workflows/flatpak.yml)
[![Secrets scan](https://github.com/ccgauvin94/grouse/actions/workflows/secrets.yml/badge.svg)](https://github.com/ccgauvin94/grouse/actions/workflows/secrets.yml)
[![License: AGPL v3](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)

## How it's put together

The wire is ACP (JSON-RPC 2.0) over WebSocket. There's a second transport called
roam that carries the same protocol over an iroh connection between two peers,
for when the server isn't directly reachable.

The client logic lives once, in Rust, under `core/`. It's exposed through a
uniffi interface that the Kotlin app consumes, and a C ABI that the desktop app
uses. The UIs are native and thin. They don't reimplement protocol behavior.

- `core/grouse-core` is the ACP client: connection, sessions, prompts, tools,
  permissions, the transcript, and the caches.
- `core/grouse-roam-core` is the roam transport.
- `core/grouse-unstable` is the `_goose/unstable/*` shim. goose has a few server
  methods the official SDK doesn't cover yet, so they're isolated here until it does.
- `clients/android` is Kotlin and Jetpack Compose.
- `clients/desktop` is Qt 6 and KF6 Kirigami.
- `clients/cli` isn't written yet. It's just a README at the moment.

## Building

The Rust core is the main development loop:

```sh
cargo test --manifest-path core/Cargo.toml
cargo clippy --manifest-path core/Cargo.toml --all-targets -- -D warnings
```

The devcontainer (`scripts/dev-env.sh`) is the easy way to do that without
installing anything. See `CONTRIBUTING.md`.

Android, with the Android SDK/NDK and JDK 17 set up:

```sh
cd clients/android
./gradlew assembleDebug
```

If you changed anything in `core/`, rebuild the native library and the uniffi
bindings before building the app, because the APK ships a prebuilt `.so`:

```sh
just android-libs
```

Desktop:

```sh
just desktop
```

That wraps `clients/desktop/build-flatpak.sh`, which builds the Flatpak bundle.

## Releasing

Push a tag:

```sh
git tag v0.2 && git push origin v0.2
```

GitHub Actions then builds the signed Android APK and the desktop Flatpak and
attaches both to the release. More detail is in `AGENTS.md`.

## License

AGPL-3.0. See [LICENSE](LICENSE).
