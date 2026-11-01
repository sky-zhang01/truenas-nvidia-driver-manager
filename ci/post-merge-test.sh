#!/usr/bin/env bash
# Offline controls for content-addressed dev publication; no registry is contacted.
set -Eeuo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "${SCRIPT_DIR}/test-lib.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
mkdir -p "${WORK}/repo/ci/job" "${WORK}/bin"
cp "${SCRIPT_DIR}/post-merge.sh" "${SCRIPT_DIR}/release-readback.js" "${WORK}/repo/ci/"
cat >"${WORK}/repo/ci/changes.sh" <<'STUB'
#!/usr/bin/env bash
[ "$*" = '--hash image' ] || exit 99
printf '%s\n' "${CONTROL_HASH}"
STUB
cat >"${WORK}/repo/ci/job/image.sh" <<'STUB'
#!/usr/bin/env bash
printf 'verify-image\n' >>"${CONTROL_WORK}/calls"
[ "${CONTROL_BUILD_FAIL:-false}" = false ] || exit 24
printf '%s\n' "${CONTROL_ID}" >nvm-image.id
printf 'archive\n' >nvm-image.tar
STUB
cat >"${WORK}/bin/docker" <<'STUB'
#!/usr/bin/env bash
set -Eeuo pipefail
printf '%s\n' "$*" >>"${CONTROL_WORK}/calls"
[ "$*" != "${CONTROL_FAIL:-}" ] || exit 23
case "$*" in
    'login '*) cat >"${CONTROL_WORK}/login.stdin" ;;
    'logout '*|'buildx version'|'tag '*) ;;
    'push '*) touch "${CONTROL_WORK}/hash-exists" ;;
    'buildx imagetools create '*) touch "${CONTROL_WORK}/dev-exists" ;;
    'buildx imagetools inspect --raw '*)
        ref="${*: -1}"
        if [[ "$ref" = *:dev ]]; then
            if [ "${CONTROL_DEV_ERROR:-false}" = true ]; then echo "connection refused" >&2; exit 1; fi
            if [ "${CONTROL_DEV:-missing}" = same ] || [ -f "${CONTROL_WORK}/dev-exists" ]; then
                if [ "${CONTROL_BAD_DEV:-false}" = true ]; then printf '{}\n'; else cat "${CONTROL_WORK}/manifest"; fi
            else echo 'manifest unknown' >&2; exit 1; fi
        elif [ "${CONTROL_HASH_EXISTS:-false}" = true ] || [ -f "${CONTROL_WORK}/hash-exists" ]; then
            cat "${CONTROL_WORK}/manifest"
        else
            echo "${CONTROL_PROBE_ERROR:-manifest unknown}" >&2; exit 1
        fi ;;
    *) echo 'unexpected Docker command' >&2; exit 90 ;;
esac
STUB
chmod +x "${WORK}/bin/docker"
export CONTROL_WORK="${WORK}" CONTROL_ID="sha256:$(printf 'a%.0s' {1..64})" CONTROL_HASH="$(printf 'b%.0s' {1..64})"
printf '{"config":{"digest":"%s"}}\n' "${CONTROL_ID}" >"${WORK}/manifest"
run_publish() {
    local want="$1" desc="$2" rc=0
    shift 2
    : >"${WORK}/calls"
    rm -f "${WORK}/hash-exists" "${WORK}/dev-exists" "${WORK}/login.stdin"
    env PATH="${WORK}/bin:${PATH}" GITHUB_EVENT_NAME=push GITHUB_REF=refs/heads/main \
        REGISTRY_IMAGE_NAME=example-image REGISTRY_HOST_VARIABLE=registry.example.invalid \
        GITHUB_REPOSITORY_OWNER=Example REGISTRY_USER=fixture-user REGISTRY_TOKEN=fixture-stdin-only \
        CONTROL_HASH_EXISTS=false CONTROL_DEV=missing CONTROL_PROBE_ERROR='manifest unknown' \
        CONTROL_BUILD_FAIL=false CONTROL_BAD_DEV=false CONTROL_DEV_ERROR=false CONTROL_FAIL= "$@" \
        bash -x "${WORK}/repo/ci/post-merge.sh" >"${WORK}/out" 2>"${WORK}/err" || rc=$?
    check "${desc}" test "${rc}" -eq "${want}"
    if grep -qF fixture-stdin-only "${WORK}/out" "${WORK}/err" "${WORK}/calls"; then fail 'token leaked'; else pass 'token stays on stdin'; fi
}
no_write() { check "$1" test "$(grep -Ec '^(push|buildx imagetools create) ' "${WORK}/calls" || true)" -eq 0; }
run_publish 0 'missing content tag builds, verifies, then publishes hash and dev'
check 'verified image precedes every registry write' node - "${WORK}/calls" <<'NODE'
const rows = require('fs').readFileSync(process.argv[2], 'utf8').trim().split('\n');
const verify = rows.indexOf('verify-image'), push = rows.findIndex(x => x.startsWith('push ')), dev = rows.findIndex(x => x.startsWith('buildx imagetools create '));
if (!(verify >= 0 && verify < push && push < dev)) process.exit(1);
if (rows.some(x => /:latest|:v[0-9]/.test(x))) process.exit(1);
NODE
check 'login receives token through stdin' test "$(cat "${WORK}/login.stdin")" = fixture-stdin-only
check 'registry logout is attempted' grep -qxF 'logout registry.example.invalid' "${WORK}/calls"
run_publish 0 'existing content is promoted without rebuilding' CONTROL_HASH_EXISTS=true
check 'existing content is never rebuilt' test "$(grep -c '^verify-image$' "${WORK}/calls" || true)" -eq 0
check 'existing content is never pushed again' test "$(grep -c '^push ' "${WORK}/calls" || true)" -eq 0
run_publish 0 'dev already points to the content digest' CONTROL_HASH_EXISTS=true CONTROL_DEV=same
no_write 'equal dev is a no-op'
run_publish 0 'manual recovery on main is allowed' GITHUB_EVENT_NAME=workflow_dispatch CONTROL_HASH_EXISTS=true
for event in pull_request schedule ''; do run_publish 1 'untrusted event is rejected' "GITHUB_EVENT_NAME=${event}"; no_write 'untrusted event has no writes'; done
run_publish 1 'non-main dispatch is rejected' GITHUB_EVENT_NAME=workflow_dispatch GITHUB_REF=refs/heads/topic
no_write 'non-main cannot publish'
for pair in REGISTRY_TOKEN= REGISTRY_USER= REGISTRY_HOST_VARIABLE= REGISTRY_HOST_VARIABLE=https://invalid.example.invalid GITHUB_REPOSITORY_OWNER=../invalid REGISTRY_IMAGE_NAME=../invalid CONTROL_HASH=wrong; do
    run_publish 1 'invalid input fails closed' "${pair}"; no_write 'invalid input has no writes'
done
for error in unauthorized 'connection refused' 'unexpected status 500'; do
    run_publish 1 'ambiguous registry failure is not absence' "CONTROL_PROBE_ERROR=${error}"
    no_write 'unknown registry state has no writes'
done
run_publish 24 'failed image verification prevents publication' CONTROL_BUILD_FAIL=true
no_write 'failed checks have no writes'
run_publish 23 'failed content push propagates' "CONTROL_FAIL=push registry.example.invalid/example/example-image:inputs-${CONTROL_HASH}"
check 'failed content push never moves dev' test "$(grep -c '^buildx imagetools create ' "${WORK}/calls" || true)" -eq 0
run_publish 1 'dev digest mismatch fails readback' CONTROL_BAD_DEV=true
run_publish 1 'dev query transport errors prevent promotion' CONTROL_HASH_EXISTS=true CONTROL_DEV_ERROR=true
no_write 'unreadable dev has no writes'
printf '{}\n' >"${WORK}/manifest"
run_publish 1 'existing content tag with an invalid manifest fails' CONTROL_HASH_EXISTS=true
no_write 'invalid existing content has no writes'
printf '{"config":{"digest":"sha256:wrong"}}\n' >"${WORK}/manifest"
run_publish 1 'new content readback must match the verified image'
check 'invalid new content never moves dev' test "$(grep -c '^buildx imagetools create ' "${WORK}/calls" || true)" -eq 0
finish_tests "post-merge-test: ${FAILURES} controls failed" "post-merge-test: ${PASS} controls passed"
