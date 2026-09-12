# grouse

Native chat clients for a self-hosted [goose](https://github.com/aaif-goose/goose)
agent.

grouse is built on the official goose SDK (`agent-client-protocol`, Client role),
with the `agent-client-protocol-http` WebSocket transport. The server owns
sessions, memory, tools, and model choice. The clients render transcripts and send
prompts.

[![Core](https://github.com/ccgauvin94/grouse/actions/workflows/core.yml/badge.svg)](https://github.com/ccgauvin94/grouse/actions/workflows/core.yml)
[![License: AGPL v3](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)

## Architecture

Wire protocol: ACP (JSON-RPC 2.0) over WebSocket. A second transport, roam, carries
the same protocol over an iroh connection between two peers.

Client logic lives once, in Rust, under `core/`, and is exposed through a uniffi
interface for the Kotlin app and a C ABI for the desktop app. The UIs are native and
thin; protocol behavior is not duplicated in them.

- `core/grouse-core`: ACP client built on the goose SDK. Connection, sessions,
  prompts, tools, permissions, transcript, caches.
- `core/grouse-roam-core`: roam transport (iroh).
- `core/grouse-unstable`: `_goose/unstable/*` shim for server methods not yet in the
  SDK.
- `clients/android`: Kotlin + Jetpack Compose.
- `clients/desktop`: Qt 6 + KF6 Kirigami.
- `clients/cli`: placeholder, no implementation.

## Support

| Platform | UI | Status | Release |
|---|---|---|---|
| Android | Kotlin + Jetpack Compose | Supported | [![Android](https://github.com/ccgauvin94/grouse/actions/workflows/android.yml/badge.svg)](https://github.com/ccgauvin94/grouse/actions/workflows/android.yml) |
| Linux | Qt 6 + KF6 Kirigami | Supported | [![Flatpak](https://github.com/ccgauvin94/grouse/actions/workflows/flatpak.yml/badge.svg)](https://github.com/ccgauvin94/grouse/actions/workflows/flatpak.yml) |
| macOS | SwiftUI | Planned | |
| iOS | SwiftUI | Not planned* | |
| TUI | Rust | Planned | |

<sub><i>* Lack of testing equipment</i></sub>

## Building

Rust core:

```sh
cargo test --manifest-path core/Cargo.toml
cargo clippy --manifest-path core/Cargo.toml --all-targets -- -D warnings
```

A devcontainer is available (`scripts/dev-env.sh`); see `CONTRIBUTING.md`.

Android (Android SDK/NDK, JDK 17):

```sh
cd clients/android
./gradlew assembleDebug
```

The APK includes a prebuilt `libgrouse_core.so`, so regenerate the native library and
uniffi bindings after any change under `core/`:

```sh
just android-libs
```

Desktop (Flatpak bundle):

```sh
just desktop
```

## Releasing

```sh
git tag v0.2 && git push origin v0.2
```

The tag runs the release workflow, which builds the signed Android APK and the
desktop Flatpak and attaches them to the GitHub release. See `AGENTS.md` for signing
and native-library requirements.

## License

AGPL-3.0. See [LICENSE](LICENSE).
