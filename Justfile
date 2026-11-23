# grouse — top-level build/test/CI entrypoints.
# Run `just` (or `just --list`) for the recipe list.

_default:
    @just --list

# Build the Rust core. (The CLI is not written yet — clients/cli is a README
# placeholder, so building it here would just fail the recipe.)
build:
    cd core && cargo build --locked

# Run the core test suite. Both recipes run from core/ so rustup reads
# core/rust-toolchain.toml; --manifest-path from the root used the host default.
test:
    cd core && cargo test --locked

# Build the Linux desktop as a Flatpak bundle.
desktop:
    cd clients/desktop && ./build-flatpak.sh

# Build the Android build image (Rust android targets + cargo-ndk + uniffi + JDK 17
# + Android SDK/NDK). Everything else android-* runs inside it.
android-image:
    ./scripts/android-env.sh --build-only

# Rebuild the Android native libs + uniffi bindings and stage them into the
# grouse-core-aar module. Run after ANY change to core/ — the committed
# libgrouse_core.so is what ships inside the APK, so skipping this ships stale code.
android-libs:
    ./scripts/android-env.sh ./scripts/build-android-libs.sh

# Verify the committed .so + bindings really are this core's output: regenerate
# into a temp dir, copy over the staged trees, and fail on any git diff. Needs
# the pinned Android toolchain on PATH; CI runs the same check in gate.yml.
android-libs-check:
    #!/usr/bin/env bash
    set -euo pipefail
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    ./scripts/build-android-libs.sh --out-dir "$tmp"
    refresh() {
      mkdir -p "$2"
      find "$2" -type f -delete
      cp -r "$1/." "$2/"
    }
    refresh "$tmp/jniLibs" clients/android/grouse-core-aar/src/main/jniLibs
    refresh "$tmp/bindings" core/grouse-core/bindings/kotlin
    refresh "$tmp/aar-kotlin" clients/android/grouse-core-aar/src/main/kotlin
    paths=(clients/android/grouse-core-aar/src/main/jniLibs \
           clients/android/grouse-core-aar/src/main/kotlin \
           core/grouse-core/bindings/kotlin)
    git diff --exit-code -- "${paths[@]}"
    test -z "$(git status --porcelain -- "${paths[@]}")"

# Build the Android debug APK.
android:
    ./scripts/android-env.sh clients/android/gradlew -p clients/android assembleDebug

# Build the signed release APK (needs the GROUSE_* signing vars in your env; without
# them gradle falls back to the debug keystore, which is not distributable).
android-release:
    ./scripts/android-env.sh clients/android/gradlew -p clients/android assembleRelease

# Verify + publish the release APK to Nextcloud. `just publish-apk <slug>`
publish-apk slug:
    ./scripts/publish-apk.sh {{slug}}

# The client parity ratchet: every surface the core exposes must be implemented
# or explicitly excluded in every client (docs/PARITY.md). Runs in CI too.
parity:
    python3 scripts/check-parity.py --self-test
    python3 scripts/check-parity.py

# Scan for committed secrets.
scan:
    gitleaks detect --source .
