#!/usr/bin/env bash
# Build grouse-desktop as a Flatpak bundle.
#
# Host prerequisites: flatpak and distrobox, with the flathub remote added (the
# script adds it). flatpak-builder is NOT a host package on atomic Fedora
# (immutable /usr, dnf blocked) — it lives in the kde-build distrobox, which is
# where the builder runs (see the note before its invocation). Run this on the
# HOST, not from inside the container.
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

for cmd in flatpak distrobox; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "error: $cmd is required" >&2
        exit 1
    fi
done

# flatpak-builder is provided by the kde-build container, not the host. Check it
# where it is actually used so an atomic-Fedora host does not abort here.
if ! distrobox enter kde-build -- bash -lc 'command -v flatpak-builder' >/dev/null 2>&1; then
    echo "error: flatpak-builder is required inside the kde-build distrobox" >&2
    exit 1
fi

flatpak remote-add --user --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo

# Stage the KRunner plugin: the SDK has no KRunner dev headers, so the runner
# is built here (in kde-build, against the host's KF6) and shipped inside the
# flatpak; the app self-installs it to the host on first run.
echo "== Building KRunner plugin =="
./build-krunner.sh --build-only
mkdir -p krunner-prebuild
cp build-krunner-host/krunner/kf6/krunner/grouserunner.so krunner-prebuild/grouserunner.so

# Stage the core: the whole thin-client engine the app dlopens (transport,
# session state machine, and roam all live in libgrouse_core.so), bundled into
# /app/lib (the flatpak SDK has no Rust toolchain). It is a member of this
# repo's cargo workspace, so we build the in-tree crate.
echo "== Building grouse-core =="
CORE_MANIFEST="../../core/Cargo.toml"
distrobox enter kde-build -- bash -lc \
    "cd '$PWD' && cargo build --release --manifest-path '${CORE_MANIFEST}' -p grouse-core"
mkdir -p grouse-core-prebuild
# A STALE core here ships a desktop with an old engine behind a new UI, and
# nothing in the app says so. Measured 2026-11-02: the lib on disk was from
# 2026-10-29 07:41 — 7h before the outbound image policy landed — so it lacked
# both `grouse_set_image_rescale` and `grouse_send_prompt_with_paths`, and any
# bundle built from it silently had no image rescaling and no file attachments.
# A current core (built above) satisfies this check; if the build above no-ops
# because the toolchain is missing, this is what stops the bundle.
CORE_LIB=../../core/target/release/libgrouse_core.so
for sym in grouse_set_image_rescale grouse_send_prompt_with_paths grouse_abi_version; do
    if ! nm -D "$CORE_LIB" 2>/dev/null | grep -q " T $sym$"; then
        echo "ERROR: $CORE_LIB does not export $sym — it predates the current core." >&2
        echo "       rebuild: distrobox enter kde-build -- bash -lc \"cargo build --release --manifest-path core/Cargo.toml -p grouse-core\"" >&2
        exit 1
    fi
done
# And it must be newer than every core source file, or cargo never ran.
STALE=$(find ../../core -name '*.rs' -newer "$CORE_LIB" -not -path '*/target/*' | head -1)
if [ -n "$STALE" ]; then
    echo "ERROR: $CORE_LIB is older than $STALE — the bundle would ship a stale core." >&2
    exit 1
fi
cp "$CORE_LIB" grouse-core-prebuild/

echo "== Building $APP_ID =="
# The builder's module cache can MISS changes to the `type: dir` source (observed:
# an edit to src/manager.cpp + ui/main.qml rebuilt nothing, and build-export's
# "Content Written: 0" shipped a stale bundle under a fresh commit hash). Drop just
# the app module's cached build so it always re-runs; the base/runtime layers stay
# cached, so this costs ~1 min of cmake/ninja, not the SDK download.
rm -rf "${STATE_DIR}/builder-cache/build/grouse-desktop-"*
# Run the builder INSIDE the kde-build container: the manifest now has
# base: io.qt.qtwebengine.BaseApp, and flatpak's ostree checkout of that base
# restores SELinux labels the host's unprivileged context cannot set
# (build-init dies with "lsetxattr(security.selinux): Operation not
# supported"). The container runs in the permissive spc_t domain, and nested
# bwrap works on this flatpak generation.
distrobox enter kde-build -- bash -lc \
    "cd '$PWD' && flatpak-builder \
        --user \
        --install-deps-from=flathub \
        --ccache \
        --force-clean \
        --disable-rofiles-fuse \
        --state-dir='${STATE_DIR}/builder-cache' \
        --repo='${REPO_DIR}' \
        '${BUILD_DIR}' \
        '${MANIFEST}' \
     && flatpak build-export --files=files '${REPO_DIR}' '${BUILD_DIR}' \
     && flatpak build-bundle '${REPO_DIR}' '${BUNDLE}' '${APP_ID}'"

echo "Done: ${BUNDLE}"
