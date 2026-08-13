#!/usr/bin/env bash
#
# Build and run the grouse core + CLI devcontainer on a podman host.
# Rootless-friendly: `--userns=keep-id` keeps the invoking user's uid/gid stable
# inside the container, so files written to the bind-mounted repo (target/, etc.)
# stay owned by you on the host.
#
# Usage:
#   scripts/dev-env.sh                                  # build (if needed), drop into a shell
#   scripts/dev-env.sh cargo test --manifest-path core/Cargo.toml   # run one command
#   scripts/dev-env.sh --build                           # force a rebuild
#   scripts/dev-env.sh --no-build                        # skip the build step
#
# Options:
#   --build       rebuild the image even if it already exists
#   --no-build    skip the build and run an existing image
#   --            treat the remaining args as the container command
#   anything else is passed through as the container command (default: /bin/bash)
set -euo pipefail

# Resolve the repo root (works whether invoked from the repo root or scripts/).
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
CONTAINERFILE="${REPO_ROOT}/.devcontainer/Containerfile"

IMAGE_NAME="${GrouseDev_IMAGE:-grouse-dev}"

FORCE_BUILD=0
SKIP_BUILD=0
args=()
while (($#)); do
  case "$1" in
    --build)    FORCE_BUILD=1; shift ;;
    --no-build) SKIP_BUILD=1;  shift ;;
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

# Default command is an interactive shell when none is supplied.
if [[ ${#args[@]} -eq 0 ]]; then
  args=(/bin/bash)
fi

exec podman run --rm -it \
  --userns=keep-id \
  --user "$(id -u):$(id -g)" \
  --volume "${REPO_ROOT}:/workspace:Z" \
  --workdir /workspace \
  "${IMAGE_NAME}" \
  "${args[@]}"
