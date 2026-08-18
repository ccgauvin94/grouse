#!/usr/bin/env bash
# Build and install the Grouse KRunner plugin for the HOST's KRunner.
#
# The runner is a host component: it lives outside the flatpak sandbox and is
# loaded by the host KRunner, so it is never part of the flatpak. It is built
# with CMake and installed with `cmake --install` (proper install rules, no
# copying .so files by hand).
#
# Install location: user-local by default, into ~/.local/lib/qt6/plugins —
# Qt6's per-user plugin dir. This is the right choice on atomic Fedora
# (Bazzite/rpm-ostree): /usr is read-only and system layering is discouraged,
# while $HOME persists across upgrades and KRunner scans it.
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

# --build-only: stop after building (used by build-flatpak.sh to stage the
# plugin binary into the flatpak image).
BUILD_ONLY=0
if [[ "${1:-}" == "--build-only" ]]; then
    BUILD_ONLY=1
    shift
fi

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
    cmake --install "${BUILD_DIR}"
else
    # The container shares $HOME, so --prefix ~/.local lands on the host.
    distrobox enter kde-build -- bash -lc '
        set -e
        cd "$1"
        cmake -S krunner -B "$2" -DCMAKE_BUILD_TYPE=Release \
            -DCMAKE_INSTALL_PREFIX="$HOME/.local" -DCMAKE_INSTALL_LIBDIR=lib
        cmake --build "$2" -j"$(nproc)"
        cmake --install "$2"
    ' _ "$(pwd)" "${BUILD_DIR}"
fi

if (( BUILD_ONLY )); then
    echo "Built: ${BUILD_DIR}/krunner/kf6/krunner/grouserunner.so"
    exit 0
fi

echo "== Ensuring Plasma scans the user plugin dir on next login =="
ENV_DIR="${HOME}/.config/plasma-workspace/env"
mkdir -p "${ENV_DIR}"
cat > "${ENV_DIR}/grouse-krunner.sh" <<EOF
# Installed by build-krunner.sh — makes KRunner scan the user plugin dir.
export QT_PLUGIN_PATH="\${HOME}/.local/lib/qt6/plugins\${QT_PLUGIN_PATH:+:\${QT_PLUGIN_PATH}}"
EOF
chmod +x "${ENV_DIR}/grouse-krunner.sh"
# Also set it in the user manager env so newly-started user services (e.g.
# a freshly spawned krunner) pick it up without waiting for the next login.
systemctl --user set-environment QT_PLUGIN_PATH="${PREFIX}/${LIBDIR}/qt6/plugins" 2>/dev/null || true

echo "== Restarting krunner so it picks up the plugin =="
kquitapp6 krunner 2>/dev/null || true
sleep 1

echo "Done: ${PREFIX}/${LIBDIR}/qt6/plugins/kf6/krunner/grouserunner.so"
