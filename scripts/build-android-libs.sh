#!/usr/bin/env bash
#
# Rebuild the Android native library + uniffi Kotlin bindings and stage them
# into the grouse-core-aar module. This is the step the aar module's build
# script refers to; run it (`just android-libs`) after any change to core/.
#
# ONE cdylib ships: libgrouse_core.so. grouse-roam-core is an rlib dependency of
# grouse-core, so the roam transport links statically into it and its uniffi
# symbols are exported from there — library-mode bindgen emits the
# uniffi.grouse_roam_core package pointing at "grouse_core". A separate
# libgrouse_roam_core.so would be ~22 MB of never-loaded duplicate; don't add one.
#
# Prereqs: rustup targets aarch64-linux-android + x86_64-linux-android,
# cargo-ndk, uniffi-bindgen (cargo install uniffi --features cli), the Android NDK.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

AAR="$ROOT/clients/android/grouse-core-aar/src/main"
BINDINGS="$ROOT/core/grouse-core/bindings/kotlin"
# Build into a scratch dir and swap only on success, so a failed run never
# leaves the module without its staged artifacts.
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

# 1. Host debug build for bindgen. The release profile strips the uniffi
#    metadata sections library-mode bindgen reads, so bindings come from an
#    unstripped host build (the metadata is identical across profiles).
cargo build --manifest-path core/Cargo.toml -p grouse-core

# 2. Android cdylibs, one per ABI: arm64-v8a for devices, x86_64 for emulators.
#    cargo-ndk resolves the workspace from the CWD and ignores --manifest-path
#    for that lookup, so it runs inside core/. We copy the artifacts ourselves
#    instead of using `-o`, which also drags along libraries we do not ship:
#    ONLY libgrouse_core.so ships — iroh's cdylibs and libgrouse_roam_core.so
#    are linked statically into it, so shipping them too is dead weight.
declare -A ABIS=( [arm64-v8a]=aarch64-linux-android [x86_64]=x86_64-linux-android )
for abi in "${!ABIS[@]}"; do
    (cd core && cargo ndk -t "$abi" build --release -p grouse-core)
    mkdir -p "$STAGE/jniLibs/$abi"
    cp "core/target/${ABIS[$abi]}/release/libgrouse_core.so" "$STAGE/jniLibs/$abi/"
done

# 3. Kotlin bindings from the HOST debug library — emits BOTH the
#    uniffi.grouse_core and uniffi.grouse_roam_core packages (the roam package
#    resolves to "grouse_core", which is why no second .so is needed).
#    uniffi-bindgen shells out to `cargo metadata`, so it too must run from
#    inside the workspace or it fails before generating anything.
(cd core && uniffi-bindgen generate --library target/debug/libgrouse_core.so \
    --language kotlin --out-dir "$STAGE/bindings" --no-format)

# 4. Swap into place: the generated bindings live next to the crate, and the
#    aar module compiles a staged copy alongside the .so files.
rm -rf "$BINDINGS" "$AAR/jniLibs" "$AAR/kotlin"
mkdir -p "$BINDINGS" "$AAR/kotlin"
cp -r "$STAGE/bindings/." "$BINDINGS/"
cp -r "$STAGE/jniLibs" "$AAR/jniLibs"
cp -r "$STAGE/bindings/uniffi" "$AAR/kotlin/"

echo "staged:"
find "$AAR/jniLibs" "$AAR/kotlin" -type f | sort | sed "s|^$ROOT/|  |"
