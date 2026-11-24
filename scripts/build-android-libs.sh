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
# Usage: scripts/build-android-libs.sh [--out-dir DIR]
#   --out-dir DIR  write jniLibs/, bindings/ and aar-kotlin/ into DIR instead of
#                  replacing the staged copies in the repo. gate.yml uses this
#                  to regenerate into a temp dir and `git diff --exit-code`.
#
# Prereqs (asserted below): rustc 1.97.1 (from core/rust-toolchain.toml),
# cargo-ndk 3.1.2, NDK 27.0.12077973, uniffi-bindgen 0.32.1 installed with
# `cargo install uniffi --version 0.32.1 --features cli --locked`. A different
# generator or NDK produces different bytes and fails the CI freshness check.
#
# LANDMINE — this repo is written by TWO different podman containers, and their
# SELinux MCS categories do not agree. `scripts/dev-env.sh` (the core's cargo
# build) and the Android toolchain image both mount the tree, so whichever ran
# last leaves `core/target/` and the staged artifacts labelled with ITS category
# pair. The other container then fails on the files it does not own — as
# `EPERM` ("Operation not permitted"), never `EACCES`, which reads like a broken
# toolchain rather than a label:
#
#   error: unable to open output file '.../ring-.../out/*.o': Operation not permitted
#   rm: cannot remove '.../libgrouse_core.so': Permission denied
#
# The staged artifacts are also owned by a SUBUID (host uid 100000), so the host
# user cannot remove them either. Recovery, in this order:
#
#   rm -rf core/target/{aarch64-linux-android,x86_64-linux-android}   # host, as colin
#   podman unshare rm -rf \
#     clients/android/grouse-core-aar/src/main/jniLibs/{arm64-v8a,x86_64} \
#     core/grouse-core/bindings/kotlin/uniffi \
#     clients/android/grouse-core-aar/src/main/kotlin/uniffi
#   ./scripts/build-android-libs.sh
#
# `podman unshare` is what removes the subuid-owned copies: it enters the userns
# where 100000 is mapped, so no sudo (and there is no passwordless sudo here).
set -euo pipefail

OUT_DIR=""
while (($#)); do
  case "$1" in
    --out-dir)
      [[ $# -ge 2 ]] || { echo "usage: $0 [--out-dir DIR]" >&2; exit 2; }
      OUT_DIR="$2"; shift 2 ;;
    *) echo "usage: $0 [--out-dir DIR]" >&2; exit 2 ;;
  esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# The committed .so must be byte-identical when the pinned toolchain rebuilds it
# anywhere, not just in the developer's Android image. Rust embeds absolute
# source paths in panic/tracing strings even in a stripped release cdylib, so
# without remapping, a build under /home/dev/.cargo differs from CI's
# /home/runner/.cargo and the freshness gate can never pass. Remap both the
# cargo home (crate sources) and the checkout root to stable placeholders.
CARGO_HOME_DIR="${CARGO_HOME:-${HOME}/.cargo}"
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=${CARGO_HOME_DIR}=/cargo --remap-path-prefix=${ROOT}=/grouse"

AAR="$ROOT/clients/android/grouse-core-aar/src/main"
BINDINGS="$ROOT/core/grouse-core/bindings/kotlin"
if [[ -n "$OUT_DIR" ]]; then
  mkdir -p "$OUT_DIR"
  DEST_JNILIBS="$OUT_DIR/jniLibs"
  DEST_BINDINGS="$OUT_DIR/bindings"
  DEST_AAR_KOTLIN="$OUT_DIR/aar-kotlin"
else
  DEST_JNILIBS="$AAR/jniLibs"
  DEST_BINDINGS="$BINDINGS"
  DEST_AAR_KOTLIN="$AAR/kotlin"
fi
# Build into a scratch dir and swap only on success, so a failed run never
# leaves the module without its staged artifacts.
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

# Toolchain assertions: rustup resolves core/rust-toolchain.toml from the CWD,
# not from --manifest-path, so ask from inside the workspace. A mismatch here
# produces a byte-different .so and fails CI's freshness diff later.
RUSTC_VERSION="$(cd "$ROOT/core" && rustc --version)"
case "$RUSTC_VERSION" in
  "rustc 1.97.1 "*) ;;
  *) echo "build-android-libs: expected rustc 1.97.1, got $RUSTC_VERSION" >&2; exit 1 ;;
esac
UNIFFI_VERSION="$(uniffi-bindgen --version 2>&1)"
case "$UNIFFI_VERSION" in
  *0.32.1*) ;;
  *) echo "build-android-libs: expected uniffi-bindgen 0.32.1, got $UNIFFI_VERSION" >&2; exit 1 ;;
esac

# 1. Host debug build for bindgen. The release profile strips the uniffi
#    metadata sections library-mode bindgen reads, so bindings come from an
#    unstripped host build (the metadata is identical across profiles).
#    CWD core/ (not --manifest-path from the root) pins the compiler.
(cd core && cargo build --locked -p grouse-core)

# 2. Android cdylibs, one per ABI: arm64-v8a for devices, x86_64 for emulators.
#    cargo-ndk resolves the workspace from the CWD and ignores --manifest-path
#    for that lookup, so it runs inside core/. We copy the artifacts ourselves
#    instead of using `-o`, which also drags along libraries we do not ship:
#    ONLY libgrouse_core.so ships — iroh's cdylibs and libgrouse_roam_core.so
#    are linked statically into it, so shipping them too is dead weight.
declare -A ABIS=( [arm64-v8a]=aarch64-linux-android [x86_64]=x86_64-linux-android )
for abi in "${!ABIS[@]}"; do
    (cd core && cargo ndk -t "$abi" build --release --locked -p grouse-core)
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
#    aar module compiles a staged copy alongside the .so files. --out-dir
#    skips the swap so CI can diff the generated tree against the committed one.
if [[ -z "$OUT_DIR" ]]; then
  rm -rf "$BINDINGS" "$AAR/jniLibs" "$AAR/kotlin"
fi
mkdir -p "$DEST_BINDINGS" "$DEST_JNILIBS" "$DEST_AAR_KOTLIN"
cp -r "$STAGE/bindings/." "$DEST_BINDINGS/"
cp -r "$STAGE/jniLibs/." "$DEST_JNILIBS/"
cp -r "$STAGE/bindings/uniffi/." "$DEST_AAR_KOTLIN/"

if [[ -n "$OUT_DIR" ]]; then
  echo "generated into $OUT_DIR:"
  find "$OUT_DIR" -type f | sort | sed "s|^$ROOT/|  |"
else
  echo "staged:"
  find "$AAR/jniLibs" "$AAR/kotlin" -type f | sort | sed "s|^$ROOT/|  |"
fi
