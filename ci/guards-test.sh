#!/usr/bin/env bash
# Exercise the real guard in disposable Git worktrees. Coordinates are built
# at runtime, so these fixtures do not exempt themselves from sanitization.
set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GUARD="${SCRIPT_DIR}/guards.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
GUARDS_TEST_GIT="$(command -v git)"
export GUARDS_TEST_GIT
. "${SCRIPT_DIR}/test-lib.sh"
CHECKS=0
REPOS=0

new_repo() {
    REPOS=$((REPOS + 1))
    REPO="${WORK}/repo-${REPOS}"
    mkdir -p "${REPO}"
    git init -q "${REPO}"
    printf 'name = "truenas-nvidia-driver-manager"\n' >"${REPO}/Cargo.toml"
    git -C "${REPO}" add -- Cargo.toml
}

expect() { # expect <rc> <description> <output fragment> [environment assignments]
    local want="$1" description="$2" fragment="$3" got=0
    shift 3
    CHECKS=$((CHECKS + 1))
    (cd "${REPO}" && env "$@" bash "${GUARD}") \
        >"${WORK}/stdout" 2>"${WORK}/stderr" || got=$?
    if [ "${got}" -eq "${want}" ] && \
        grep -qF -- "${fragment}" "${WORK}/stdout" "${WORK}/stderr"; then
        pass "${description} (rc=${got})"
    else
        fail "${description}: expected rc=${want} and '${fragment}', got rc=${got}"
        cat "${WORK}/stdout" "${WORK}/stderr" >&2
    fi
}

# Batch samples by scan rule because the guard exits at the first failed rule.
reject_text() { # reject_text <description> <text>
    SAMPLE_COUNT=$((SAMPLE_COUNT + 1))
    printf 'safe first line\n%s\n' "$2" >"${REPO}/sample-${SAMPLE_COUNT}.txt"
    SAMPLE_DESCRIPTIONS+=("$1")
}

start_samples() {
    new_repo
    SAMPLE_COUNT=0
    SAMPLE_DESCRIPTIONS=()
}

expect_samples() {
    local got=0 index=0 description
    git -C "${REPO}" add -- .
    (cd "${REPO}" && bash "${GUARD}") >"${WORK}/stdout" 2>"${WORK}/stderr" || got=$?
    for description in "${SAMPLE_DESCRIPTIONS[@]}"; do
        index=$((index + 1))
        CHECKS=$((CHECKS + 1))
        if [ "${got}" -eq 1 ] && grep -qF "sample-${index}.txt:2:" "${WORK}/stderr"; then
            pass "${description} (rc=${got})"
        else
            fail "${description}: expected rc=1 and sample-${index}.txt:2:, got rc=${got}"
            cat "${WORK}/stdout" "${WORK}/stderr" >&2
        fi
    done
}

new_repo
mkdir "${REPO}/ci"
cp "${GUARD}" "${SCRIPT_DIR}/guards-test.sh" "${REPO}/ci/"
git -C "${REPO}" add -- ci
expect 0 'clean tree including both guard scripts passes' 'OK: no non-placeholder pool path'

start_samples
for stem in 10.1.2 192.168.1 172.16.1 172.31.255; do
    reject_text "${stem} family at EOL" "${stem}.3"
    reject_text "${stem} family before a sentence period" "reach ${stem}.3."
done
stem='10.1.2'
reject_text 'IPv4 before comma' "reach ${stem}.3, next"
reject_text 'IPv4 inside parentheses' "(${stem}.3)"
reject_text 'IPv4 sentence followed by more text' "reach ${stem}.3. Next"

expect_samples
start_samples
for sep in ':' '-'; do
    reject_text "MAC with ${sep} separators" "aa${sep}bb${sep}cc${sep}dd${sep}ee${sep}ff"
    reject_text "uppercase MAC with ${sep} separators" "AA${sep}BB${sep}CC${sep}DD${sep}EE${sep}FF"
done
sep=':'
reject_text 'MAC immediately after a colon label' "hwaddr${sep}aa${sep}bb${sep}cc${sep}dd${sep}ee${sep}ff"
reject_text 'uppercase MAC immediately after a colon label' "HWADDR${sep}AA${sep}BB${sep}CC${sep}DD${sep}EE${sep}FF"
sep='-'
reject_text 'MAC immediately after a hyphen label' "nic${sep}aa${sep}bb${sep}cc${sep}dd${sep}ee${sep}ff"
reject_text 'uppercase MAC immediately after a hyphen label' "NIC${sep}AA${sep}BB${sep}CC${sep}DD${sep}EE${sep}FF"
sep='.'
reject_text 'Cisco dotted MAC' "aabb${sep}ccdd${sep}eeff"
reject_text 'uppercase dotted MAC before a sentence period' "AABB${sep}CCDD${sep}EEFF."
reject_text 'MAC immediately after a dotted label' "x${sep}aabb${sep}ccdd${sep}eeff"
reject_text 'uppercase MAC immediately after a dotted label' "X${sep}AABB${sep}CCDD${sep}EEFF"
expect_samples
start_samples
pool_root='/mnt'
reject_text 'non-placeholder pool path' "${pool_root}/probe/file"
expect_samples

new_repo
printf '%s\n' \
    'version ^10.6.0 || >=11.0.0; 1.2.3.4; 0.2.190' \
    'deadbeef 0123456789abcdef0123456789abcdef01234567 2026' \
    "${stem}.3.4; 1.${stem}.3" \
    "${pool_root}/<pool>/<dataset>; ${pool_root}/" >"${REPO}/input.txt"
git -C "${REPO}" add -- input.txt
expect 0 'versions, hashes, years, longer dotted numbers and placeholders stay clean' 'OK: no non-placeholder pool path'

new_repo
for public_stem in 172.15.1 172.32.1 192.169.1 110.1.2 11.1.2; do
    printf '%s\n' "${public_stem}.3" >>"${REPO}/input.txt"
done
git -C "${REPO}" add -- input.txt
expect 0 'addresses outside private ranges stay clean' 'OK: no non-placeholder pool path'

new_repo
for sep in ':' '-'; do
    address="aa${sep}bb${sep}cc${sep}dd${sep}ee${sep}ff"
    printf '%s\n' "f0${sep}${address}" "nic${sep}f0${sep}${address}" \
        "nic${sep}${address}${sep}00" >>"${REPO}/input.txt"
    if [ "${sep}" = ':' ]; then
        printf '%s\n' "fe80::${address}" "hwaddr:fe80::${address}" >>"${REPO}/input.txt"
    fi
done
sep='.'
address="aabb${sep}ccdd${sep}eeff"
date_stem='2026-10-04'
printf '%s\n' "0000${sep}${address}" "x${sep}0000${sep}${address}" \
    "x${sep}${address}${sep}0011" "${date_stem}-12-30-45" >>"${REPO}/input.txt"
git -C "${REPO}" add -- input.txt
expect 0 'IPv6, dates and longer hex chains stay clean, including after labels' 'OK: no non-placeholder pool path'

new_repo
printf '%s\n' "${stem}.3" >"${REPO}/untracked.txt"
expect 0 'untracked files are outside the tracked-file contract' 'OK: no private IPv4'

new_repo
printf 'safe\n%s\n' "${stem}.3" >"${REPO}/--exclude=*"
git -C "${REPO}" add -- '--exclude=*'
expect 1 'option-like tracked filename cannot suppress the scan' '--exclude=*:2:'

new_repo
mkdir -p "${REPO}/.gitea/workflows"
printf 'safe\n%s\n' "${pool_root}/probe/file" >"${REPO}/.gitea/workflows/ci.yml"
git -C "${REPO}" add -- .gitea/workflows/ci.yml
expect 1 'the workflow itself is scanned for pool paths' '.gitea/workflows/ci.yml:2:'

for script in ci/guards.sh ci/guards-test.sh; do
    new_repo
    mkdir "${REPO}/ci"
    printf 'safe\n%s\n' "${pool_root}/probe/file" >"${REPO}/${script}"
    git -C "${REPO}" add -- "${script}"
    expect 1 "${script} itself is scanned" "${script}:2:"
done

new_repo
printf 'binary\0data\n%s\n' "${stem}.3" >"${REPO}/input.bin"
git -C "${REPO}" add -- input.bin
expect 1 'NUL bytes do not hide a tracked violation' 'input.bin:2:'
printf '*.bin binary\n' >"${REPO}/.git/info/attributes"
expect 1 'binary matches retain file and line with attributes present' 'input.bin:2:'

for attr in .gitattributes nested/.gitattributes; do
    new_repo
    mkdir -p "${REPO}/$(dirname "${attr}")"
    printf '*.txt binary\n' >"${REPO}/${attr}"
    git -C "${REPO}" add -- "${attr}"
    expect 1 "tracked ${attr} is rejected" "${attr}"
done

# Inject failures only at Git's boundary; every non-injected call uses real Git.
mkdir "${WORK}/bin"
cat >"${WORK}/bin/git" <<'GIT_SHIM'
#!/usr/bin/env bash
case "${GUARDS_TEST_MODE}:${1:-}:${2:-}" in
    attributes:ls-files:*) exit 2 ;;
    engine:grep:-qaF) exit 2 ;;
    canary-error:grep:--no-index) exit 2 ;;
    canary-miss:grep:--no-index) exit 1 ;;
    canary-match:grep:--no-index) exit 0 ;;
    scan:grep:-naE) exit 2 ;;
esac
exec "${GUARDS_TEST_GIT}" "$@"
GIT_SHIM
chmod 0700 "${WORK}/bin/git"
new_repo
expect 1 'attribute enumeration failure fails closed' 'ls-files failed (rc=2)' \
    "PATH=${WORK}/bin:${PATH}" GUARDS_TEST_MODE=attributes
expect 1 'tracked engine failure fails closed' 'tracked-file engine control failed' \
    "PATH=${WORK}/bin:${PATH}" GUARDS_TEST_MODE=engine
expect 1 'canaries fail when git grep errors' 'canary failed (expected rc=0, got rc=2)' \
    "PATH=${WORK}/bin:${PATH}" GUARDS_TEST_MODE=canary-error
expect 1 'positive canaries reject an engine that never matches' 'canary failed (expected rc=0, got rc=1)' \
    "PATH=${WORK}/bin:${PATH}" GUARDS_TEST_MODE=canary-miss
expect 1 'negative canaries reject an engine that always matches' 'canary failed (expected rc=1, got rc=0)' \
    "PATH=${WORK}/bin:${PATH}" GUARDS_TEST_MODE=canary-match
expect 1 'tracked scan failure is not a clean result' 'scan failed (rc=2)' \
    "PATH=${WORK}/bin:${PATH}" GUARDS_TEST_MODE=scan

# Keep workflow runtimes aligned with the image's web build stage.
CHECKS=$((CHECKS + 1))
if node - "${SCRIPT_DIR}/.." <<'NODE'
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const root = process.argv[2];
function workflowVersion(file, variable) {
    const source = fs.readFileSync(path.join(root, file), 'utf8');
    const env = source.match(/^env:\n((?:[ \t].*\n|\n)*)/m)?.[1];
    assert.ok(env, `${file} must declare a workflow env block`);
    const matches = [...env.matchAll(new RegExp(`^  ${variable}: "([0-9]+\\.[0-9]+\\.[0-9]+)"[ \t]*$`, 'gm'))];
    assert.equal(matches.length, 1, `${file} must pin exactly one ${variable}`);
    return matches[0][1];
}
const githubVersion = workflowVersion('.github/workflows/ci.yml', 'NODE_VERSION');
const dockerfile = fs.readFileSync(path.join(root, 'Dockerfile'), 'utf8');
const imageVersions = [...dockerfile.matchAll(/^FROM node:([0-9]+)(?:[.:-][^ \n]+)?(?:@[^ \n]+)? AS web[ \t]*$/gm)];
assert.equal(imageVersions.length, 1, 'Dockerfile must pin one Node web build stage');
assert.equal(githubVersion.split('.')[0], imageVersions[0][1], 'GitHub Node and Dockerfile Node major versions must match');
if (fs.existsSync(path.join(root, '.gitea/workflows/ci.yml'))) {
    const giteaVersion = workflowVersion('.gitea/workflows/ci.yml', 'NODE_BOOTSTRAP_VERSION');
    assert.equal(githubVersion, giteaVersion, 'GitHub and Gitea Node versions must match');
} else {
    console.log('SKIP: private workflow comparison; public exports exclude .gitea/');
}
NODE
then
    pass 'workflow Node versions match each other and the image major'
else
    fail 'workflow Node versions differ or cannot be read'
fi

finish_tests "guards-test: ${FAILURES}/${CHECKS} controls failed" \
    "guards-test: ${CHECKS} controls passed"
