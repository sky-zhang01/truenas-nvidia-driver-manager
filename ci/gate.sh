#!/usr/bin/env bash
# gate.sh — every enabled check must succeed; every disabled check must skip.
set -Eeuo pipefail
failed=0
gate_fail() {
    echo "gate: $1" >&2
    failed=1
}
for job in changes guards; do
    variable="$(printf %s "${job}" | tr '[:lower:]' '[:upper:]')_RESULT"
    [ "${!variable:-}" = success ] \
        || gate_fail "the ${job} job must succeed (result '${!variable:-missing}')"
done
for job in controls web compose rust e2e image build_kit osv leak; do
    switch="SWITCH_$(printf %s "${job}" | tr '[:lower:]' '[:upper:]')"
    result="$(printf %s "${job}" | tr '[:lower:]' '[:upper:]')_RESULT"
    [ "${job}" != build_kit ] || result=BUILD_SCRIPT_RESULT
    case "${!switch:-}" in
        true) wanted=success ;;
        false) wanted=skipped ;;
        *) gate_fail "switch ${job} is '${!switch:-missing}', not true/false"; continue ;;
    esac
    [ "${!result:-}" = "${wanted}" ] \
        || gate_fail "job ${job} must be ${wanted} when its switch is ${!switch} (result '${!result:-missing}')"
done
# The workflow fetches the live base branch immediately before this step.
# Compare it with the PR head, never the synthetic GitHub merge checkout.
if [ -n "${GATE_PR_HEAD:-}" ]; then
    if [ -z "${GATE_BASE_SHA:-}" ]; then
        gate_fail 'a PR head requires the live base revision'
    elif ! git merge-base --is-ancestor "${GATE_BASE_SHA}" "${GATE_PR_HEAD}"; then
        gate_fail 'the PR head must include the live base revision'
    fi
fi
if [ "${failed}" -eq 0 ]; then
    echo 'gate: every required job succeeded and every unselected job skipped'
    exit 0
fi
exit 1
