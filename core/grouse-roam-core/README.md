# grouse-roam-core

The native iroh roam transport for the Grouse Android (and later KDE) clients.

The transport protocol itself lives in the goose fork's
[`goose-roaming`](https://github.com/ccgauvin94/goose/tree/feat/acp-federate-roam/crates/goose-roaming)
crate — the same code `goose roam share` / `goose serve --roam` run. This crate is
deliberately thin: it exposes exactly the surface a native ACP client needs, through
uniffi-generated Kotlin bindings:

| Kotlin (uniffi) | What it does |
|---|---|
| `identityGenerate()` | fresh iroh secret key, base64. The APP owns the bytes (SecureStore); the core never persists anything |
| `identityPublicKey(secret)` | hex public key — what a host sees in `peers list` before accepting |
| `cardFingerprint(card)` | fingerprint of a connection card, for the pairing UI |
| `roamConnect(secret, card, label) -> RoamStream` | dial + roam handshake; returns the authorized duplex |
| `RoamStream.read/write/close` | blocking byte I/O — the app speaks ACP framing over it (same framing goose uses on stdio) |

## Layout

- `src/lib.rs` — the uniffi surface (blocking wrappers over a tokio runtime)
- `tests/core.rs` — JVM-independent unit tests (identity, card, dial-failure path)
- `tests/e2e.rs` — ignored; needs a live `goose serve --roam` + relay
- `bindings/` — generated Kotlin (build artifact, not committed)
- `android/` — the .aar packaging (roamcore library module)
- `.github/workflows/aar.yml` — build + publish the .aar to the `maven-repo` branch

## Build (host tests)

```sh
cargo test                    # unit tests need no Android, no goose
uniffi-bindgen generate --library target/debug/libgrouse_roam_core.so \
    --language kotlin --out-dir bindings/kotlin
```

Prereqs: Rust, cmake (aws-lc-rs), and `cargo install uniffi --version 0.32.0 --features cli`.

## Build (Android .aar)

```sh
rustup target add aarch64-linux-android
cargo install cargo-ndk --locked
./scripts/build-aar.sh        # needs the Android SDK + NDK + JDK 17
```

The app consumes the published artifact:
`implementation("dev.grouse:roamcore:<version>")` with the maven repo
`https://raw.githubusercontent.com/ccgauvin94/grouse-roam-core/maven-repo`.

## Pinning

`Cargo.toml` pins the fork's `feat/acp-federate-roam` commit (iroh 1.0.2 inside).
Bump the `rev` deliberately — the roam wire protocol must not drift from what
hosts run.
