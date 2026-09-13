[![Core](https://github.com/ccgauvin94/grouse/actions/workflows/core.yml/badge.svg)](https://github.com/ccgauvin94/grouse/actions/workflows/core.yml)
[![License: AGPL v3](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)

# grouse

Native chat clients for a self-hosted [goose](https://github.com/aaif-goose/goose)
agent.

grouse is built on the official goose SDK (`agent-client-protocol`, Client role),
with the `agent-client-protocol-http` WebSocket transport. The server owns
sessions, memory, tools, and model choice. The clients render transcripts and send
prompts.

<details>
<summary>Screenshots</summary>

| Android Chat | Android Menu | Android Goose Roam |
| :---: | :---: | :---: |
| <img width="260" alt="Android Chat" src="https://github.com/user-attachments/assets/9bbeaec9-2f89-4237-8add-173bf1d0d7a4" /> | <img width="260" alt="Android Menu" src="https://github.com/user-attachments/assets/d3141077-e8fc-41b6-a450-bd6432ab99aa" /> | <img width="260" alt="Android Goose Roam" src="https://github.com/user-attachments/assets/ccbd4cc2-6881-47a0-a1e5-d83d0e4df8c8" /> |

| Linux desktop |
| :---: |
| <img width="800" alt="Linux desktop" src="https://github.com/user-attachments/assets/a03f7e10-c635-4d58-a8f9-c4f508826277" /> |

</details>

## Support

| Platform | UI | Status | Release | Download |
|---|---|---|---|---|
| Android | Kotlin + Jetpack Compose | Supported | [![Android](https://github.com/ccgauvin94/grouse/actions/workflows/android.yml/badge.svg)](https://github.com/ccgauvin94/grouse/actions/workflows/android.yml) | [APK](https://github.com/ccgauvin94/grouse/releases/latest/download/grouse-android.apk) |
| Linux | Qt 6 + KF6 Kirigami | Supported | [![Flatpak](https://github.com/ccgauvin94/grouse/actions/workflows/flatpak.yml/badge.svg)](https://github.com/ccgauvin94/grouse/actions/workflows/flatpak.yml) | [Flatpak](https://github.com/ccgauvin94/grouse/releases/latest/download/grouse-desktop.flatpak) |
| macOS | SwiftUI | Planned | | |
| iOS | SwiftUI | Not planned* | | |
| TUI | Rust | Planned | | |

<sub><i>* Lack of testing equipment</i></sub>

<details>
<summary>Architecture</summary>

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

</details>

<details>
<summary>Building</summary>

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

</details>

## Releasing

```sh
git tag v0.2 && git push origin v0.2
```

The tag runs the release workflow, which builds the signed Android APK and the
desktop Flatpak and attaches them to the GitHub release. See `AGENTS.md` for signing
and native-library requirements.

## License

AGPL-3.0. See [LICENSE](LICENSE).

