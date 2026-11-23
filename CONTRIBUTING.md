# Contributing to grouse

## One core first

Before changing any UI, check whether the behavior belongs in `core/`. The
principle is **one core first**: shared behavior — session state, the ACP wire,
the goose compatibility shim (`_goose/unstable/*`), roam — lives once in `core/`
and is exposed through the uniffi contract (and the C ABI for the Linux desktop).
If a change would make two UIs diverge, it belongs in the core, not in the UIs.

The doc ownership map in `AGENTS.md` says which file owns which claim: architecture
lives there, the core's uniffi interface (with `core/grouse-core/src/surface.rs` and
the generated bindings as the inventory) is the machine-readable API boundary, and
`design/` owns the shared design tokens. Link to the owner rather than restating it.

## Development environment (devcontainer)

The recommended way to work on the Rust core is the devcontainer in
`.devcontainer/`: a Rocky Linux 9 image (matching the home-server deployment OS)
with the Rust toolchain plus the core's native build deps (gcc, cmake, clang,
openssl-devel, pkg-config, perl). It is deliberately scoped to `core/` only —
the CLI under `clients/cli/` is not yet written (nothing but a README stub), and
the desktop and Android toolchains are separate per-platform setups (below), not
part of the image.

Two ways in, same image:

- **Editor** — open the repo in an editor that supports devcontainers (VS Code
  with the Dev Containers extension pointed at `podman`). The image builds on
  first open and the workspace mounts at `/workspace`.
- **CLI (on the podman host)** — `scripts/dev-env.sh` builds the image once and
  runs it with the repo bind-mounted:

  ```sh
  scripts/dev-env.sh                                             # interactive shell
  scripts/dev-env.sh cargo build --manifest-path core/Cargo.toml # run one command
  ```

Then build and test as usual:

```sh
cargo build --manifest-path core/Cargo.toml
cargo test  --manifest-path core/Cargo.toml
```

(The CLI under `clients/cli/` is not yet written, so there is no
`clients/cli/Cargo.toml` to build.)

### Per-platform extras (not in the devcontainer image)

- **Linux desktop** — Qt6 + KF6 Kirigami; built inside a `kde-build` distrobox
  (see below).
- **Android** — Android SDK + `gradlew`; see below.
- **macOS** — SwiftUI spike + first app slice under `clients/macos/`, consuming
  the uniffi Swift bindings. It builds on a Mac only (`./build.sh` there); CI
  checks binding generation and listener defaults but does not compile Swift.

## Building and testing from one clone

Everything builds from a single checkout. No secrets are committed; local
endpoint configuration is developer-specific.

### Core (Rust)

```sh
cargo build --manifest-path core/Cargo.toml
cargo test --manifest-path core/Cargo.toml
```

The CLI (`clients/cli/`) is not yet written — nothing lives there but a README
stub, so there is no `clients/cli/Cargo.toml` to build yet.

Or via Just: `just build` / `just test`.

### Linux desktop (Qt6/KF6 Kirigami)

The native build needs a distrobox container named `kde-build` with Qt6, KF6
Kirigami, and extra-cmake-modules installed:

```sh
distrobox enter kde-build -- bash -lc 'cmake -B build -S . -DCMAKE_BUILD_TYPE=Debug && cmake --build build -j$(nproc)'
```

The Flatpak bundle builds from the manifest with flatpak-builder; `just desktop`
wraps it. See `clients/desktop/AGENTS.md` for the bundle's build-script and cache
footguns.

### Android (Kotlin/Compose)

```sh
cd clients/android
./gradlew assembleDebug
```

The debug APK lands under `clients/android/app/build/outputs/apk/debug/`.

### Secrets scan

```sh
gitleaks detect --source .
```

The secrets workflow in `.github/workflows/secrets.yml` runs on pushes; a commit
must not contain secrets or personal identifiers.

## CI

The files in `.github/workflows/` are the source of truth for CI's jobs, triggers
and required checks — read them before claiming that a check exists. They define
the core, Android, desktop/Flatpak, parity, CodeQL, secrets, macOS binding-guard,
gate and release jobs; no list here can stay current. Locally, run the matching
commands from `AGENTS.md` before pushing. In particular, macOS Swift is **not**
compiled in CI.
