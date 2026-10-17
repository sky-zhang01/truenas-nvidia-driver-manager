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

image_ref="debian@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587"
expected="sha256:d4c45f66efc59f89b0597d343a9bd72808493c0eacecde5285961743252cee23"
index_digest="sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587"
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
