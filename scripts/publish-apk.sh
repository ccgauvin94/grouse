#!/usr/bin/env bash
#
# Publish a built APK to Nextcloud, refusing anything that is not signed with the
# real release key or that would install as the same/older build.
#
# The two checks exist because both failures are silent on the phone:
#   - a debug-signed APK installs fine (Android accepts it over the debug app) and
#     then nobody can be updated to a properly signed one without an uninstall;
#   - a re-used versionCode is a "successful" install that Android may skip, so a
#     fix looks broken and gets debugged all over again.
#
# Usage:
#   scripts/publish-apk.sh <slug> [apk-path]
#     <slug>      short name for the change, e.g. roam-queued-send
#     [apk-path]  defaults to the release build output
#
# Env:
#   GROUSE_PUBLISH_DIR   drop directory (default /mnt/data/nextcloud/colin/files)
#   GROUSE_EXPECT_SHA256 signer fingerprint that must match (default: the release key)
#
# The APK is verified INSIDE the build image (it is the thing with apksigner/aapt2),
# then copied on the host and indexed with `occ files:scan` so it appears at once —
# nextcloud-cron is cron, not a file watcher.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"

SLUG="${1:?usage: publish-apk.sh <slug> [apk-path]}"
APK="${2:-${REPO_ROOT}/clients/android/app/build/outputs/apk/release/app-release.apk}"
PUBLISH_DIR="${GROUSE_PUBLISH_DIR:-/mnt/data/nextcloud/colin/files}"
# CN=Grouse, O=Grouse, C=US — the one key that has ever signed a shipped build.
EXPECT_SHA256="${GROUSE_EXPECT_SHA256:-95b51fd4e9b7a8298a3c362da07ba4bc908f3b2ce8bcaa0820f87b34dd84474e}"
IMAGE_NAME="${GrouseAndroid_IMAGE:-grouse-android}"

[[ -f "${APK}" ]] || { echo "publish-apk: no APK at ${APK}" >&2; exit 1; }
[[ -d "${PUBLISH_DIR}" ]] || { echo "publish-apk: no publish dir at ${PUBLISH_DIR}" >&2; exit 1; }
podman image exists "${IMAGE_NAME}" || {
  echo "publish-apk: image ${IMAGE_NAME} is missing; run scripts/android-env.sh --build" >&2
  exit 1
}

# Relative to the repo, so the same path resolves inside the container.
rel_apk="${APK#${REPO_ROOT}/}"
[[ "${rel_apk}" != "${APK}" ]] || { echo "publish-apk: APK must live under ${REPO_ROOT}" >&2; exit 1; }

NAME="$(podman run --rm --security-opt label=disable --userns=keep-id \
  --user "$(id -u):$(id -g)" \
  --volume "${REPO_ROOT}:/workspace:ro" \
  --volume "${PUBLISH_DIR}:/publish:ro" \
  --env EXPECT_SHA256="${EXPECT_SHA256}" \
  --env SLUG="${SLUG}" \
  --env REL_APK="${rel_apk}" \
  --entrypoint /bin/bash "${IMAGE_NAME}" -s <<'IN_CONTAINER'
set -euo pipefail
apksigner="$(ls /opt/android-sdk/build-tools/*/apksigner | head -1)"
aapt2="$(ls /opt/android-sdk/build-tools/*/aapt2 | head -1)"
apk="/workspace/${REL_APK}"

# 1. Signed with the release key, and not debug-signed.
got="$("${apksigner}" verify --print-certs "${apk}" 2>/dev/null \
        | awk -F': ' '/^Signer #1 certificate SHA-256 digest/{print $2; exit}')"
if [[ "${got}" != "${EXPECT_SHA256}" ]]; then
  echo "REFUSING: signer is ${got:-<none>}, expected ${EXPECT_SHA256}" >&2
  echo "          (a debug-signed APK installs over the debug app and locks out updates)" >&2
  exit 2
fi

# 2. versionCode strictly ahead of everything already published.
badging="$("${aapt2}" dump badging "${apk}")"
vcode="$(sed -n "s/.*versionCode='\([0-9]*\)'.*/\1/p" <<<"${badging}" | head -1)"
vname="$(sed -n "s/.*versionName='\([^']*\)'.*/\1/p" <<<"${badging}" | head -1)"
max=0
for f in /publish/grouse-*.apk; do
  [[ -e "${f}" ]] || continue
  c="$("${aapt2}" dump badging "${f}" 2>/dev/null | sed -n "s/.*versionCode='\([0-9]*\)'.*/\1/p" | head -1)"
  [[ -n "${c}" && "${c}" -gt "${max}" ]] && max="${c}"
fi
if [[ "${vcode}" -le "${max}" ]]; then
  echo "REFUSING: versionCode ${vcode} is not newer than ${max} already published" >&2
  echo "          bump versionCode/versionName in app/build.gradle.kts first" >&2
  exit 3
fi

printf '%s\n' "${vname}-${SLUG}"
IN_CONTAINER
)"

DEST="${PUBLISH_DIR}/grouse-${NAME}.apk"
[[ -e "${DEST}" ]] && { echo "publish-apk: ${DEST} already exists" >&2; exit 4; }
cp "${APK}" "${DEST}"
chmod 644 "${DEST}"

# Index it now: the file watcher does not exist, and an unscanned file is 404 in
# the web UI even though it is on disk. Match the container's data-dir view.
podman exec -u www-data nextcloud php occ files:scan --path="/colin/files" >/dev/null
echo "published: ${DEST}"
echo "nextcloud: https://cloud.gauvin.id/remote.php/dav/files/colin/$(basename "${DEST}")"
