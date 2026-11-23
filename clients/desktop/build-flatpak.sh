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
# flatpak as a convenience for users who choose to install it themselves.
# --build-only never writes to ~/.local or to the Plasma env: host integration
# is opt-in via `./build-krunner.sh --install`, run by the user on the host.
echo "== Building KRunner plugin =="
./build-krunner.sh --build-only
mkdir -p krunner-prebuild
cp build-krunner-host/krunner/kf6/krunner/grouserunner.so krunner-prebuild/grouserunner.so
KRUNNER_TOOLCHAIN=$(distrobox enter kde-build -- bash -lc '
    . /etc/os-release
    printf "%s %s; Qt %s; KF6 %s" "$NAME" "$VERSION_ID" \
        "$(pkg-config --modversion Qt6Core 2>/dev/null || echo unknown)" \
        "$(pkg-config --modversion KF6Runner 2>/dev/null || echo unknown)"')

# Stage the core: the whole thin-client engine the app dlopens (transport,
# session state machine, and roam all live in libgrouse_core.so), bundled into
# /app/lib (the flatpak SDK has no Rust toolchain). It is a member of this
# repo's cargo workspace, so we build the in-tree crate.
echo "== Building grouse-core =="
CORE_DIR="$(cd ../../core && pwd)"
# rustup resolves rust-toolchain.toml from CWD, NOT from --manifest-path: build
# from core/ so the pinned 1.97.1 is actually used, and prove it before staging.
CORE_TOOLCHAIN=$(distrobox enter kde-build -- bash -lc 'cd "$1" && rustc --version' _ "$CORE_DIR")
if [[ "$CORE_TOOLCHAIN" != "rustc 1.97.1"* ]]; then
    echo "ERROR: core toolchain is '$CORE_TOOLCHAIN', expected rustc 1.97.1." >&2
    echo "       install the pinned toolchain (rustup toolchain install 1.97.1) and rebuild." >&2
    exit 1
fi
distrobox enter kde-build -- bash -lc \
    'cd "$1" && cargo build --release -p grouse-core' _ "$CORE_DIR"
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
    # NOT `grep -q`: it exits after the first match, `nm` gets SIGPIPE, and with
    # `set -o pipefail` the pipeline then reports 141 — so this check failed on a
    # perfectly good core (measured: exit 141 for `grep -q`, exit 0 for plain grep
    # over the same 802-line symbol table). Redirect instead of asking grep to stop.
    if ! nm -D "$CORE_LIB" 2>/dev/null | grep " T $sym$" >/dev/null; then
        echo "ERROR: $CORE_LIB does not export $sym — it predates the current core." >&2
        echo "       rebuild: distrobox enter kde-build -- bash -lc \"cargo build --release --manifest-path core/Cargo.toml -p grouse-core\"" >&2
        exit 1
    fi
done
# And it must be newer than every core source file, or cargo never ran. `-print
# -quit` rather than `| head -1` for the same reason: head would close the pipe and
# pipefail would fail the assignment (and `set -e` would kill the script).
STALE=$(find ../../core -name '*.rs' -newer "$CORE_LIB" -not -path '*/target/*' -print -quit)
if [ -n "$STALE" ]; then
    echo "ERROR: $CORE_LIB is older than $STALE — the bundle would ship a stale core." >&2
    exit 1
fi
cp "$CORE_LIB" grouse-core-prebuild/

# The committed manifest is toolchain-agnostic so CI can stage the blobs itself;
# local builds pin both blobs by digest and record the toolchains that produced
# them in the generated manifest. The provenance module copies the digests and
# toolchain strings into /app/share/grouse-desktop in the installed bundle.
RUNNER_SHA=$(sha256sum krunner-prebuild/grouserunner.so | awk '{print $1}')
CORE_SHA=$(sha256sum grouse-core-prebuild/libgrouse_core.so | awk '{print $1}')
GENERATED_MANIFEST="${STATE_DIR}/${APP_ID}.generated.json"
MANIFEST_SRC="$MANIFEST" MANIFEST_OUT="$GENERATED_MANIFEST" \
RUNNER_SHA="$RUNNER_SHA" CORE_SHA="$CORE_SHA" \
RUNNER_TOOLCHAIN="$KRUNNER_TOOLCHAIN" CORE_TOOLCHAIN="$CORE_TOOLCHAIN" \
python3 - <<'PY'
import json, os
with open(os.environ['MANIFEST_SRC'], encoding='utf-8') as f:
    manifest = json.load(f)
digests = {
    'krunner-prebuild/grouserunner.so': os.environ['RUNNER_SHA'],
    'grouse-core-prebuild/libgrouse_core.so': os.environ['CORE_SHA'],
}
manifest_dir = os.path.dirname(os.path.abspath(os.environ['MANIFEST_SRC']))
for module in manifest['modules']:
    for source in module.get('sources', []):
        if source.get('path') in digests:
            source['sha256'] = digests[source['path']]
        # The generated manifest lives in STATE_DIR, so relative source paths
        # must be resolved against the committed manifest's directory.
        if 'path' in source and not os.path.isabs(source['path']):
            source['path'] = os.path.normpath(
                os.path.join(manifest_dir, source['path']))
    if module.get('name') == 'build-provenance':
        commands = []
        for command in module['build-commands']:
            command = command.replace(
                'KRunner plugin toolchain: Fedora 44 KF6/Qt6 host build',
                'KRunner plugin toolchain: ' + os.environ['RUNNER_TOOLCHAIN'])
            command = command.replace(
                'grouse-core toolchain: rustc 1.97.1 (core/rust-toolchain.toml)',
                'grouse-core toolchain: ' + os.environ['CORE_TOOLCHAIN']
                + ' (core/rust-toolchain.toml)')
            commands.append(command)
        module['build-commands'] = commands
with open(os.environ['MANIFEST_OUT'], 'w', encoding='utf-8') as f:
    json.dump(manifest, f, indent=2)
    f.write('\n')
PY
echo "Digest-pinned manifest: ${GENERATED_MANIFEST}"
MANIFEST="$GENERATED_MANIFEST"

echo "== Building $APP_ID =="
# The builder's module cache can MISS changes to the `type: dir` source (observed:
# an edit to src/manager.cpp + ui/main.qml rebuilt nothing, and build-export's
# "Content Written: 0" shipped a stale bundle under a fresh commit hash). Drop just
# the app module's cached build so it always re-runs; the base/runtime layers stay
# cached, so this costs ~1 min of cmake/ninja, not the SDK download.
# CI's flatpak-builder action sets `cache: true` and does NOT do this: the
# coordinator must clear the app module cache there (or set cache: false) or a
# source-only change can ship a stale bundle. Local builds always clear it;
# GROUSE_KEEP_APP_CACHE=1 is only for debugging cache behaviour.
if [ "${GROUSE_KEEP_APP_CACHE:-0}" != "1" ]; then
    rm -rf "${STATE_DIR}/builder-cache/build/grouse-desktop-"*
fi
# A local install does not need the single-file bundle: flatpak-builder has
# already written the ref into ${REPO_DIR}, which `flatpak install` takes
# directly. Producing the file is 252s of single-threaded compression (measured:
# 276s of a 375s rebuild) that the next command decompresses straight back.
# Publishing still needs the file, so this is opt-in and the default is unchanged.
FINISH="flatpak build-export --files=files '${REPO_DIR}' '${BUILD_DIR}'"
if [ "${GROUSE_NO_BUNDLE:-0}" != "1" ]; then
    FINISH="${FINISH} && flatpak build-bundle '${REPO_DIR}' '${BUNDLE}' '${APP_ID}'"
fi
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
     && ${FINISH}"

if [ "${GROUSE_NO_BUNDLE:-0}" = "1" ]; then
    echo "Done: exported to ${REPO_DIR} (no bundle) — install with:"
    echo "  flatpak --user install --reinstall grouse-local ${APP_ID}"
else
    echo "Done: ${BUNDLE}"
fi
