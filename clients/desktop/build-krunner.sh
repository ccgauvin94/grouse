#!/usr/bin/env bash
# Build (and optionally INSTALL) the Grouse KRunner plugin for the HOST's
# KRunner. The runner is a host component: it lives outside the flatpak
# sandbox and is loaded by the host KRunner, so it is never part of the app's
# own startup path.
#
# Building is NOT an installation. With no arguments this script only compiles
# the plugin and leaves the host untouched (this is what build-flatpak.sh uses
# to stage the bundle's copy). Installing into ~/.local and writing the Plasma
# env snippet requires an explicit, user-run `./build-krunner.sh --install`.
# Nothing here is ever done on the user's behalf by the sandboxed app or by a
# build step.
#
# Install location: user-local, into ~/.local/lib/qt6/plugins — Qt6's per-user
# plugin dir. This is the right choice on atomic Fedora (Bazzite/rpm-ostree):
# /usr is read-only and system layering is discouraged, while $HOME persists
# across upgrades and KRunner scans it.
#
# Build environment: if the host has the KF6/Qt6 dev stack (pkg-config finds
# KF6Runner etc.) it builds directly; otherwise it builds inside the
# kde-build distrobox, which is the machine's sanctioned dev environment.
# The container shares $HOME, so its `cmake --install` writes the host's
# ~/.local directly.
#
# Build deps (inside kde-build if the host lacks them):
#   qt6-qtbase-devel extra-cmake-modules kf6-krunner-devel
#   kf6-kservice-devel kf6-ki18n-devel kf6-kcoreaddons-devel
set -euo pipefail
cd "$(dirname "$0")"

BUILD_DIR="${BUILD_DIR:-build-krunner-host}"
PREFIX="${HOME}/.local"
LIBDIR=lib   # Qt6 scans ~/.local/lib/qt6/plugins for user plugins

# Default: build only. --build-only is kept as an explicit spelling for older
# callers; --install is the ONLY way this script touches the host.
INSTALL=0
ASSUME_YES=0
for arg in "$@"; do
    case "$arg" in
        --build-only) ;;                       # the default, accepted for compatibility
        --install) INSTALL=1 ;;
        --yes) ASSUME_YES=1 ;;
        -h|--help)
            echo "usage: $0 [--build-only | --install [--yes]]" >&2
            exit 0 ;;
        *) echo "error: unknown option: $arg" >&2; exit 2 ;;
    esac
done

build_env=host
for pc in Qt6Core Qt6DBus KF6Runner KF6Service KF6I18n KF6CoreAddons; do
    pkg-config --exists "$pc" || { build_env=distrobox; break; }
done
if [[ ! -f /usr/share/ECM/cmake/ECMConfig.cmake && ! -f /usr/lib64/cmake/ECM/ECMConfig.cmake ]]; then
    build_env=distrobox
fi

configure=(cmake -S krunner -B "${BUILD_DIR}"
    -DCMAKE_BUILD_TYPE=Release
    -DCMAKE_INSTALL_PREFIX="${PREFIX}"
    -DCMAKE_INSTALL_LIBDIR="${LIBDIR}")

echo "== Building in: ${build_env} =="
if [[ "${build_env}" == "host" ]]; then
    "${configure[@]}"
    cmake --build "${BUILD_DIR}" -j"$(nproc)"
else
    # The container shares $HOME, so --prefix ~/.local lands on the host.
    distrobox enter kde-build -- bash -lc '
        set -e
        cd "$1"
        cmake -S krunner -B "$2" -DCMAKE_BUILD_TYPE=Release \
            -DCMAKE_INSTALL_PREFIX="$HOME/.local" -DCMAKE_INSTALL_LIBDIR=lib
        cmake --build "$2" -j"$(nproc)"
    ' _ "$(pwd)" "${BUILD_DIR}"
fi

PLUGIN="${BUILD_DIR}/krunner/kf6/krunner/grouserunner.so"
if (( ! INSTALL )); then
    echo "Built: ${PLUGIN}"
    echo "Host plugin dir NOT modified. To install it for your user, run:"
    echo "  ./build-krunner.sh --install"
    exit 0
fi

if (( ! ASSUME_YES )) && [[ -t 0 ]]; then
    read -r -p "Install the KRunner plugin into ${PREFIX} and enable it for this user? [y/N] " reply
    [[ "$reply" == [yY] || "$reply" == [yY][eE][sS] ]] || { echo "Aborted; host untouched."; exit 1; }
fi

echo "== Installing KRunner plugin for this user =="
if [[ "${build_env}" == "host" ]]; then
    cmake --install "${BUILD_DIR}"
else
    distrobox enter kde-build -- bash -lc 'cmake --install "$1"' _ "${BUILD_DIR}"
fi

# The plugin lives in Qt's user dir; Plasma needs to be told to scan it. This
# writes only the user's own plasma-workspace/env, and only after the explicit
# --install consent above — never from a build or from the sandboxed app.
ENV_DIR="${HOME}/.config/plasma-workspace/env"
mkdir -p "${ENV_DIR}"
cat > "${ENV_DIR}/grouse-krunner.sh" <<EOF
# Installed by build-krunner.sh --install — makes KRunner scan the user plugin dir.
export QT_PLUGIN_PATH="\${HOME}/.local/lib/qt6/plugins\${QT_PLUGIN_PATH:+:\${QT_PLUGIN_PATH}}"
EOF
chmod +x "${ENV_DIR}/grouse-krunner.sh"
# New user services started in this session (a freshly spawned krunner) pick the
# path up without waiting for the next login.
systemctl --user set-environment QT_PLUGIN_PATH="${PREFIX}/${LIBDIR}/qt6/plugins" 2>/dev/null || true

echo "== Restarting krunner so it picks up the plugin =="
kquitapp6 krunner 2>/dev/null || true
sleep 1

echo "Done: ${PREFIX}/${LIBDIR}/qt6/plugins/kf6/krunner/grouserunner.so"
