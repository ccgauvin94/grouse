# grouse — top-level build/test/CI entrypoints.
# Run `just` (or `just --list`) for the recipe list.

_default:
    @just --list

# Build the Rust core and the CLI.
build:
    cargo build --manifest-path core/Cargo.toml
    cargo build --manifest-path clients/cli/Cargo.toml

# Run the core test suite.
test:
    cargo test --manifest-path core/Cargo.toml

# Build the Linux desktop as a Flatpak bundle.
desktop:
    cd clients/desktop && ./build-flatpak.sh

# Rebuild the Android native libs + uniffi bindings and stage them into the
# grouse-core-aar module. Run after any change to core/.
android-libs:
    ./scripts/build-android-libs.sh

# Build the Android debug APK.
android:
    cd clients/android && ./gradlew assembleDebug

# Scan for committed secrets.
scan:
    gitleaks detect --source .
