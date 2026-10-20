#!/usr/bin/env bash
# prepare-env.sh — the pinned build environment of the build script tests.
#
# Pulls the official Debian image pinned by its index digest, verifies the
# local image ID (a classic image store reports the per-platform ID, a
# containerd-backed store the index digest; both name the same pinned amd64
# content), and fetches every manifest package into NVM_BUILD_KIT via
# tools/build-kit.sh.
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${repo_root}"
NVM_BUILD_KIT="${NVM_BUILD_KIT:?set NVM_BUILD_KIT}"

image_ref="debian@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f"
expected="sha256:567e3c17b188e21cd42aa5e1c55e16d8270f880ed8ea321dbd40f993467efc4c"
index_digest="sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f"
actual=""
if ! actual="$(docker image inspect -f '{{.Id}}' "${image_ref}" 2>/dev/null)"; then
    docker pull --platform linux/amd64 "${image_ref}"
    actual="$(docker image inspect -f '{{.Id}}' "${image_ref}")"
fi
case "${actual}" in
    "${expected}" | "${index_digest}") ;;
    *) { echo "unexpected local image ID ${actual}; expected ${expected}" >&2; exit 1; } ;;
esac
mkdir -p "${NVM_BUILD_KIT}"
bash tools/build-kit.sh fetch "${NVM_BUILD_KIT}"
