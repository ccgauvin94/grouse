# Contributing to grouse

## One core first

Before changing any UI, check whether the behavior belongs in `core/`. The
principle is **one core first**: shared behavior — session state, the ACP wire,
the goose compatibility shim (`_goose/unstable/*`), roam — lives once in `core/`
and is exposed through the uniffi contract (and the C ABI for the Linux desktop).
If a change would make two UIs diverge, it belongs in the core, not in the UIs.

The contract lives in three places:

- `AGENTS.md` — the architecture and the protocol notes that matter.
- the core's uniffi interface — the machine-readable API boundary every UI consumes.
- `design/` — the shared design tokens (owned by the design-language workstream).

## Development environment (devcontainer)

The recommended way to work on the Rust core and CLI is the devcontainer in
`.devcontainer/`: a Rocky Linux 9 image (matching the home-server deployment OS)
with the Rust toolchain plus the core's native build deps (gcc, cmake, clang,
openssl-devel, pkg-config, perl). It is deliberately scoped to `core/` +
`clients/cli/` only — the desktop and Android toolchains are separate
per-platform setups (below), not part of the image.

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
cargo build --manifest-path clients/cli/Cargo.toml
```

### Per-platform extras (not in the devcontainer image)

- **Linux desktop** — Qt6 + KF6 Kirigami; built inside a `kde-build` distrobox
  (see below).
- **Android** — Android SDK + `gradlew`; see below.

## Building and testing from one clone

Everything builds from a single checkout. No secrets are committed; local
endpoint configuration is developer-specific.

### Core (Rust) and CLI (Rust)

```sh
cargo build --manifest-path core/Cargo.toml
cargo test --manifest-path core/Cargo.toml
cargo build --manifest-path clients/cli/Cargo.toml
```

Or via Just: `just build` / `just test`.

### Linux desktop (Qt6/KF6 Kirigami)

The native build needs a distrobox container named `kde-build` with Qt6, KF6
Kirigami, and extra-cmake-modules installed:

```sh
distrobox enter kde-build -- bash -lc 'cmake -B build -S . -DCMAKE_BUILD_TYPE=Debug && cmake --build build -j$(nproc)'
```

The Flatpak bundle builds from the manifest with flatpak-builder (see
`clients/desktop/` once the desktop lands in the monorepo); `just desktop` wraps
it.

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

CI runs this on every push; a commit must not contain secrets or personal
identifiers.

## CI

- Linux desktop → Flatpak bundle only.
- Android → APK only.
- Core → `cargo test` + `clippy`.
- Every push → gitleaks secrets scan.
