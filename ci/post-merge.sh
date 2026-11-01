#!/usr/bin/env bash
# Publish verified content under an immutable inputs tag, then move only dev.
# An absent tag is the sole build trigger, so interrupted main pushes recover.
set +x
set -Eeuo pipefail
case "${GITHUB_EVENT_NAME:-}:${GITHUB_REF:-}" in
    push:refs/heads/main|workflow_dispatch:refs/heads/main) ;;
    *) echo 'post-merge: requires main push or manual recovery on main' >&2; exit 1 ;;
esac
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"
fail() { echo "post-merge: $1" >&2; exit 1; }
host="$(printf '%s' "${REGISTRY_HOST_VARIABLE:-}" | tr '[:upper:]' '[:lower:]')"
owner="$(printf '%s' "${GITHUB_REPOSITORY_OWNER:-}" | tr '[:upper:]' '[:lower:]')"
printf '%s' "${host}" | grep -qE '^([a-z0-9]([a-z0-9.-]*[a-z0-9])?|\[[0-9a-f:]+\])(:[0-9]{1,5})?$' || fail 'invalid registry host'
for component in "${owner}" "${REGISTRY_IMAGE_NAME:-}"; do
    printf '%s' "${component}" | grep -qE '^[a-z0-9]+([._-][a-z0-9]+)*$' || fail 'invalid registry path'
done
[ -n "${REGISTRY_TOKEN:-}" ] && [ -n "${REGISTRY_USER:-}" ] || fail 'registry credentials are required'
inputs_hash="$(bash ci/changes.sh --hash image)"
printf '%s' "${inputs_hash}" | grep -qE '^[0-9a-f]{64}$' || fail 'invalid image inputs hash'
registry_ref="${host}/${owner}/${REGISTRY_IMAGE_NAME}"
content_ref="${registry_ref}:inputs-${inputs_hash}"
work="$(mktemp -d)"
# Each run owns its login state; logout cannot remove another run's session.
export DOCKER_CONFIG="${work}/docker"
mkdir "${DOCKER_CONFIG}"
cleanup() {
    docker logout "${host}" >/dev/null 2>&1 || true
    rm -rf "${work}"
}
trap cleanup EXIT
printf '%s' "${REGISTRY_TOKEN}" | docker login "${host}" --username "${REGISTRY_USER}" --password-stdin
unset REGISTRY_TOKEN
docker buildx version
probe() { # Only explicit missing-manifest responses authorize a build.
    local ref="$1" destination="$2"
    if docker buildx imagetools inspect --raw "${ref}" >"${destination}" 2>"${work}/probe.err"; then
        return 0
    fi
    if grep -qiE 'manifest unknown|name unknown|no such manifest|manifest_unknown|name_unknown|unexpected status.*404|: not found$' "${work}/probe.err"; then
        return 1
    fi
    fail 'cannot determine registry manifest existence'
}
digest() { printf 'sha256:%s' "$(sha256sum "$1" | awk '{print $1}')"; }
if probe "${content_ref}" "${work}/content.json"; then
    echo "post-merge: reusing ${content_ref}"
else
    # The job includes filesystem assertions and container tests; failures
    # terminate before any registry write, including on recovery attempts.
    bash ci/job/image.sh
    image_id="$(cat nvm-image.id)"
    docker tag "${image_id}" "${content_ref}"
    docker push "${content_ref}"
    docker buildx imagetools inspect --raw "${content_ref}" >"${work}/content.json"
    node ci/release-readback.js "${work}/content.json" "${content_ref}" "${image_id}" "$(digest "${work}/content.json")"
fi
content_digest="$(digest "${work}/content.json")"
# Validate a reused tag as well. A classic manifest identifies its config;
# an index identifies the image by the digest of the exact index bytes.
content_id="$(node - "${work}/content.json" "${content_digest}" <<'NODE'
const fs = require('node:fs');
const doc = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const id = Array.isArray(doc?.manifests) ? process.argv[3] : doc?.config?.digest;
if (typeof id !== 'string' || !/^sha256:[a-f0-9]{64}$/.test(id)) process.exit(1);
console.log(id);
NODE
)"
node ci/release-readback.js "${work}/content.json" "${content_ref}" "${content_id}" "${content_digest}"
if probe "${registry_ref}:dev" "${work}/dev.json" && [ "$(digest "${work}/dev.json")" = "${content_digest}" ]; then
    echo 'post-merge: image unchanged; dev remains unchanged'
    exit 0
fi
# --prefer-index=false preserves the source manifest bytes for a single
# image too; copying by digest never rebuilds or chooses a mutable tag.
docker buildx imagetools create --prefer-index=false --tag "${registry_ref}:dev" "${registry_ref}@${content_digest}"
docker buildx imagetools inspect --raw "${registry_ref}:dev" >"${work}/dev.json"
[ "$(digest "${work}/dev.json")" = "${content_digest}" ] || fail 'dev readback digest differs from content tag'
echo "post-merge: ${registry_ref}:dev -> ${content_digest}"
