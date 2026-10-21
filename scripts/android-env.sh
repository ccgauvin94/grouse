#!/usr/bin/env bash
#
# Build and run the grouse ANDROID build environment on a podman host.
#
# Same shape as scripts/dev-env.sh (rootless-friendly: `--userns=keep-id` plus an
# explicit --user keeps the invoking uid/gid, so the repo's target/ and the staged
# .so files stay owned by you).
#
# This is where `just android-libs` and the APK build belong when the host has no
# NDK: the committed libgrouse_core.so is what ships inside the APK, so shipping a
# core change means regenerating it, and that needs the NDK + cargo-ndk.
#
# Usage:
#   scripts/android-env.sh                              # interactive shell
#   scripts/android-env.sh ./scripts/build-android-libs.sh
#   scripts/android-env.sh ./gradlew assembleRelease
#   scripts/android-env.sh --build                      # force an image rebuild
#   scripts/android-env.sh --build-only                 # (re)build the image, then exit
#   scripts/android-env.sh --no-build <command>
#
# Signing: the four GROUSE_* variables (GROUSE_KEYSTORE, GROUSE_STORE_PASSWORD,
# GROUSE_KEY_ALIAS, GROUSE_KEY_PASSWORD) are forwarded from YOUR environment when
# set, and the keystore's directory is mounted read-only at the same path. Nothing
# is written into this script or into the image, and the values never appear in
# the podman command line (--env NAME reads them from the caller's env).
#
# With none of them set, `assembleRelease` still works — gradle falls back to the
# debug keystore (see app/build.gradle.kts), which installs over a debug build but
# is NOT a distributable signature.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
CONTAINERFILE="${REPO_ROOT}/.devcontainer/android/Containerfile"

IMAGE_NAME="${GrouseAndroid_IMAGE:-grouse-android}"
# Gradle's caches live OUTSIDE the repo so a build can never confuse the working
# tree, and so an image rebuild does not throw away ~1G of downloaded artifacts.
GRADLE_HOME="${GrouseAndroid_GRADLE_HOME:-${XDG_CACHE_HOME:-$HOME/.cache}/grouse/gradle}"
# Cargo's registry/git caches, likewise outside the repo. Mounted as SUBPATHS of
# the image's CARGO_HOME so the toolchain binaries installed there (cargo-ndk,
# uniffi-bindgen) stay visible.
CARGO_CACHE="${GrouseAndroid_CARGO_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/grouse/cargo}"

FORCE_BUILD=0
SKIP_BUILD=0
BUILD_ONLY=0
args=()
while (($#)); do
  case "$1" in
    --build)      FORCE_BUILD=1; shift ;;
    --build-only) FORCE_BUILD=1; BUILD_ONLY=1; shift ;;
    --no-build)   SKIP_BUILD=1;  shift ;;
    --)         shift; args+=("$@"); break ;;
    *)          args+=("$1"); shift ;;
  esac
done

if [[ "${SKIP_BUILD}" -ne 1 ]]; then
  if [[ "${FORCE_BUILD}" -eq 1 ]] || ! podman image exists "${IMAGE_NAME}"; then
    podman build \
      --build-arg UID="$(id -u)" \
      --build-arg GID="$(id -g)" \
      --file "${CONTAINERFILE}" \
      --tag "${IMAGE_NAME}" \
      "$(dirname -- "${CONTAINERFILE}")"
  fi
fi

[[ "${BUILD_ONLY}" -eq 1 ]] && { echo "image ready: ${IMAGE_NAME}"; exit 0; }

mkdir -p "${GRADLE_HOME}" "${CARGO_CACHE}/registry" "${CARGO_CACHE}/git"

# Default command is an interactive shell when none is supplied.
if [[ ${#args[@]} -eq 0 ]]; then
  args=(/bin/bash)
fi

# The keystore directory is mounted read-only at its own host path, so the
# absolute GROUSE_KEYSTORE value keeps working inside the container.
mounts=(--volume "${REPO_ROOT}:/workspace:Z"
        --volume "${GRADLE_HOME}:/home/dev/.gradle"
        --volume "${CARGO_CACHE}/registry:/home/dev/.cargo/registry"
        --volume "${CARGO_CACHE}/git:/home/dev/.cargo/git")
signing_env=()
for var in GROUSE_KEYSTORE GROUSE_STORE_PASSWORD GROUSE_KEY_ALIAS GROUSE_KEY_PASSWORD; do
  if [[ -n "${!var:-}" ]]; then
    signing_env+=(--env "${var}")
  fi
done
if [[ -n "${GROUSE_KEYSTORE:-}" && -e "${GROUSE_KEYSTORE}" ]]; then
  ks_dir="$(cd -- "$(dirname -- "${GROUSE_KEYSTORE}")" && pwd)"
  mounts+=(--volume "${ks_dir}:${ks_dir}:ro")
fi

exec podman run --rm -it \
  --userns=keep-id \
  --user "$(id -u):$(id -g)" \
  --env HOME=/home/dev \
  --env GRADLE_USER_HOME=/home/dev/.gradle \
  "${mounts[@]}" \
  "${signing_env[@]}" \
  --workdir /workspace \
  "${IMAGE_NAME}" \
  "${args[@]}"
