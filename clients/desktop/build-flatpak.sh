#!/usr/bin/env bash
# Build grouse-desktop as a Flatpak bundle.
#
# Requires flatpak and flatpak-builder, with the flathub remote added:
#   flatpak remote-add --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
set -euo pipefail
cd "$(dirname "$0")"

APP_ID="id.gauvin.Grouse"
MANIFEST="${APP_ID}.json"
# Keep build state outside the source tree so the manifest's `type: dir`
# source doesn't copy it into the sandbox.
STATE_DIR="${STATE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/grouse-flatpak}"
BUILD_DIR="${BUILD_DIR:-${STATE_DIR}/build}"
REPO_DIR="${REPO_DIR:-${STATE_DIR}/repo}"
BUNDLE="${BUNDLE:-grouse-desktop.flatpak}"

for cmd in flatpak flatpak-builder; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "error: $cmd is required" >&2
        exit 1
    fi
done

flatpak remote-add --user --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo

# Stage the KRunner plugin: the SDK has no KRunner dev headers, so the runner
# is built here (in kde-build, against the host's KF6) and shipped inside the
# flatpak; the app self-installs it to the host on first run.
echo "== Building KRunner plugin =="
./build-krunner.sh --build-only
mkdir -p krunner-prebuild
cp build-krunner-host/krunner/kf6/krunner/grouserunner.so krunner-prebuild/grouserunner.so

# Stage the roam transport: the native iroh library RoamTransport dlopens,
# bundled into /app/lib (the flatpak SDK has no Rust toolchain). It is a member
# of this repo's cargo workspace now — this used to clone grouse-roam-core from
# GitHub and check out a pinned SHA that lived on no branch.
echo "== Building grouse-roam-core =="
CORE_MANIFEST="../../core/Cargo.toml"
distrobox enter kde-build -- bash -lc \
    "cd '$PWD' && cargo build --release --manifest-path '${CORE_MANIFEST}' -p grouse-roam-core"
mkdir -p roam-prebuild
cp ../../core/target/release/libgrouse_roam_core.so roam-prebuild/

echo "== Building $APP_ID =="
flatpak-builder \
    --user \
    --install-deps-from=flathub \
    --ccache \
    --force-clean \
    --disable-rofiles-fuse \
    --state-dir="${STATE_DIR}/builder-cache" \
    --repo="${REPO_DIR}" \
    "${BUILD_DIR}" \
    "${MANIFEST}"

echo "== Exporting =="
flatpak build-export --files=files "${REPO_DIR}" "${BUILD_DIR}"

echo "== Bundling =="
flatpak build-bundle "${REPO_DIR}" "${BUNDLE}" "${APP_ID}"

echo "Done: ${BUNDLE}"
