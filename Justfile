# grouse — top-level build/test/CI entrypoints.
# Run `just` (or `just --list`) for the recipe list.

_default:
    @just --list

# Build the Rust core. (The CLI is not written yet — clients/cli is a README
# placeholder, so building it here would just fail the recipe.)
build:
    cargo build --manifest-path core/Cargo.toml

# Run the core test suite.
test:
    cargo test --manifest-path core/Cargo.toml

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

# Scan for committed secrets.
scan:
    gitleaks detect --source .
