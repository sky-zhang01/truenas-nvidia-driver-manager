#!/usr/bin/env bash
# gate-test.sh — controls for ci/gate.sh.
set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="${SCRIPT_DIR}/gate.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
. "${SCRIPT_DIR}/test-lib.sh"

expect_gate() { # expect_gate <rc> <description> <message fragment or '-'> [ENV=VALUE...]
    local want_rc="$1" desc="$2" want_msg="$3"
    shift 3
    local rc=0
    env -u CHANGES_RESULT -u STATIC_RESULT -u RUST_RESULT -u E2E_RESULT \
        -u IMAGE_RESULT -u BUILD_SCRIPT_RESULT -u PUBLIC_LEAK_RESULT \
        -u SWITCH_RUST -u SWITCH_E2E -u SWITCH_IMAGE -u SWITCH_BUILD_KIT \
        "$@" bash "${GATE}" >"${WORK}/out" 2>"${WORK}/err" || rc=$?
    if [ "${rc}" -ne "${want_rc}" ]; then
        fail "${desc}: want rc ${want_rc}, got ${rc}; output: $(cat "${WORK}/err")"
    elif [ "${want_msg}" != '-' ] && ! grep -qF -- "${want_msg}" "${WORK}/err"; then
        fail "${desc}: message '${want_msg}' not in: $(cat "${WORK}/err")"
    else
        pass "${desc}"
    fi
}

ALL_ON=(SWITCH_RUST=true SWITCH_E2E=true SWITCH_IMAGE=true SWITCH_BUILD_KIT=true)
ALL_OFF=(SWITCH_RUST=false SWITCH_E2E=false SWITCH_IMAGE=false SWITCH_BUILD_KIT=false)
SUCCESS=(CHANGES_RESULT=success STATIC_RESULT=success RUST_RESULT=success E2E_RESULT=success
         IMAGE_RESULT=success BUILD_SCRIPT_RESULT=success PUBLIC_LEAK_RESULT=success)
expect_gate 0 'all switches on, every job succeeded' - "${SUCCESS[@]}" "${ALL_ON[@]}"
expect_gate 0 'all switches off, heavy jobs skipped' - \
    CHANGES_RESULT=success STATIC_RESULT=success RUST_RESULT=skipped E2E_RESULT=skipped \
    IMAGE_RESULT=skipped BUILD_SCRIPT_RESULT=skipped PUBLIC_LEAK_RESULT=success "${ALL_OFF[@]}"
expect_gate 0 'e2e runs independently of Rust' - "${SUCCESS[@]}" "${ALL_OFF[@]}" \
    SWITCH_E2E=true RUST_RESULT=skipped
expect_gate 0 'Rust runs independently of e2e' - "${SUCCESS[@]}" "${ALL_OFF[@]}" \
    SWITCH_RUST=true E2E_RESULT=skipped

# Every upstream result must be present; failures and timeouts fail even with switches off.
for pair in changes:CHANGES_RESULT static:STATIC_RESULT rust:RUST_RESULT \
            e2e:E2E_RESULT image:IMAGE_RESULT build-script:BUILD_SCRIPT_RESULT \
            public-leak:PUBLIC_LEAK_RESULT; do
    job="${pair%%:*}"
    variable="${pair#*:}"
    for result in failure cancelled ''; do
        if [ "${job}" = changes ]; then
            message='the changes job did not succeed'
        else
            message="job ${job} has result '${result:-missing}'"
        fi
        expect_gate 1 "${job} result '${result:-missing}' fails the gate" "${message}" \
            "${SUCCESS[@]}" "${ALL_OFF[@]}" "${variable}=${result}"
    done
done
expect_gate 1 'skipped changes fails the gate' 'the changes job did not succeed' \
    "${SUCCESS[@]}" "${ALL_OFF[@]}" CHANGES_RESULT=skipped
expect_gate 1 'skipped static fails the gate' 'the static job must succeed every run' \
    "${SUCCESS[@]}" "${ALL_OFF[@]}" STATIC_RESULT=skipped
expect_gate 1 'skipped public leak scan fails the gate' 'the public leak job must succeed every run' \
    "${SUCCESS[@]}" "${ALL_OFF[@]}" PUBLIC_LEAK_RESULT=skipped

for triple in rust:SWITCH_RUST:RUST_RESULT e2e:SWITCH_E2E:E2E_RESULT \
              image:SWITCH_IMAGE:IMAGE_RESULT build-script:SWITCH_BUILD_KIT:BUILD_SCRIPT_RESULT; do
    job="${triple%%:*}"
    rest="${triple#*:}"
    switch="${rest%%:*}"
    result="${rest#*:}"
    expect_gate 1 "required ${job} skipped fails the gate" \
        "job ${job} must succeed when its switch is on" \
        "${SUCCESS[@]}" "${ALL_OFF[@]}" "${switch}=true" "${result}=skipped"
    for value in '' on; do
        expect_gate 1 "${switch} '${value:-missing}' fails closed" \
            "not true/false" "${SUCCESS[@]}" "${ALL_ON[@]}" "${switch}=${value}"
    done
done

expect_gate 1 'multiple failures fail the gate' 'the changes job did not succeed' \
    CHANGES_RESULT=failure STATIC_RESULT=failure RUST_RESULT=skipped E2E_RESULT=skipped \
    IMAGE_RESULT=skipped BUILD_SCRIPT_RESULT=skipped PUBLIC_LEAK_RESULT=failure "${ALL_ON[@]}"
if grep -qF "job static has result 'failure'" "${WORK}/err" \
    && grep -qF 'job rust must succeed when its switch is on' "${WORK}/err" \
    && grep -qF 'job e2e must succeed when its switch is on' "${WORK}/err" \
    && grep -qF 'job image must succeed when its switch is on' "${WORK}/err" \
    && grep -qF 'job build-script must succeed when its switch is on' "${WORK}/err" \
    && grep -qF "job public-leak has result 'failure'" "${WORK}/err"; then
    pass 'a failing gate reports every broken rule'
else
    fail "a failing gate must report every broken rule, got: $(cat "${WORK}/err")"
fi
# Both pipelines route the README copy check through the always-required static job.
if node - "${SCRIPT_DIR}/.." <<'NODE'
const fs = require('node:fs'), path = require('node:path');
for (const name of ['.gitea/workflows/ci.yml', '.github/workflows/ci.yml']) {
    const file = path.join(process.argv[2], name);
    if (name.startsWith('.gitea/') && !fs.existsSync(file)) {
        console.log('SKIP: private workflow is absent from the public export');
        continue;
    }
    const source = fs.readFileSync(file, 'utf8');
    const body = source.split('\n  static:\n')[1]?.split(/^  [a-z][a-z-]*:\s*$/m)[0];
    if (!body || /^    if:/m.test(body)) throw new Error(name + ': static must run unconditionally');
    for (const command of ['bash ci/check-readme-compose-test.sh', 'python3 ci/check-readme-compose.py']) {
        const steps = body.split(/(?=^      - (?:name:|uses:))/m).filter(step => step.includes(command));
        if (steps.length !== 1 || /^        if:/m.test(steps[0])) throw new Error(name + ': missing unconditional ' + command);
    }
}
NODE
then pass 'both static jobs require README compose controls and the real check'
else fail 'README compose checks must run in both required static jobs'
fi
finish_tests "gate-test: ${FAILURES} control(s) failed" "gate-test: ${PASS} controls passed"
