#!/usr/bin/env bash
# gate.sh — the single required check of the pipeline.
#
# Inputs (environment, wired by the gate job):
#   CHANGES_RESULT, STATIC_RESULT, RUST_RESULT, E2E_RESULT, IMAGE_RESULT,
#   BUILD_SCRIPT_RESULT, PUBLIC_LEAK_RESULT — needs.<job>.result
#   SWITCH_RUST, SWITCH_E2E, SWITCH_IMAGE, SWITCH_BUILD_KIT — the changes
#   job's switch outputs
#
# Fails when: the changes job did not succeed; any job failed or was
# cancelled; a job whose switch is on was skipped; a switch is unreadable.
# A job whose switch is off may be skipped — that direction is not checked,
# so a skipped optional job can never fail the run.
set -Eeuo pipefail

CHANGES_RESULT="${CHANGES_RESULT:-}"
STATIC_RESULT="${STATIC_RESULT:-}"
RUST_RESULT="${RUST_RESULT:-}"
E2E_RESULT="${E2E_RESULT:-}"
IMAGE_RESULT="${IMAGE_RESULT:-}"
BUILD_SCRIPT_RESULT="${BUILD_SCRIPT_RESULT:-}"
PUBLIC_LEAK_RESULT="${PUBLIC_LEAK_RESULT:-}"
SWITCH_RUST="${SWITCH_RUST:-}"
SWITCH_E2E="${SWITCH_E2E:-}"
SWITCH_IMAGE="${SWITCH_IMAGE:-}"
SWITCH_BUILD_KIT="${SWITCH_BUILD_KIT:-}"

failed=0
gate_fail() {
    echo "gate: $1" >&2
    failed=1
}

# The switches must be readable booleans; an unreadable switch fails closed
# instead of guessing.
for pair in "rust:${SWITCH_RUST}" "e2e:${SWITCH_E2E}" \
            "image:${SWITCH_IMAGE}" "build_kit:${SWITCH_BUILD_KIT}"; do
    name="${pair%%:*}"
    value="${pair#*:}"
    case "${value}" in
        true|false) ;;
        *) gate_fail "switch ${name} is '${value:-missing}', not true/false" ;;
    esac
done

# The switch computation itself must have succeeded.
[ "${CHANGES_RESULT}" = success ] \
    || gate_fail "the changes job did not succeed (result '${CHANGES_RESULT:-missing}')"

# No job may have failed or been cancelled.
check_result() { # check_result <job> <result>
    case "$2" in
        success|skipped) ;;
        *) gate_fail "job $1 has result '${2:-missing}'" ;;
    esac
}
check_result static "${STATIC_RESULT}"
check_result rust "${RUST_RESULT}"
check_result e2e "${E2E_RESULT}"
check_result image "${IMAGE_RESULT}"
check_result build-script "${BUILD_SCRIPT_RESULT}"
check_result public-leak "${PUBLIC_LEAK_RESULT}"

# static runs on every event; a skipped static job means nothing was checked.
[ "${STATIC_RESULT}" = success ] \
    || gate_fail "the static job must succeed every run (result '${STATIC_RESULT:-missing}')"
[ "${PUBLIC_LEAK_RESULT}" = success ] \
    || gate_fail "the public leak job must succeed every run (result '${PUBLIC_LEAK_RESULT:-missing}')"

# A job whose switch is on must have succeeded — being skipped is a failure,
# so a routing mistake can never pass silently.
require() { # require <switch value> <job> <result>
    if [ "$1" = true ] && [ "$3" != success ]; then
        gate_fail "job $2 must succeed when its switch is on (result '${3:-missing}')"
    fi
}
require "${SWITCH_RUST}" rust "${RUST_RESULT}"
require "${SWITCH_E2E}" e2e "${E2E_RESULT}"
require "${SWITCH_IMAGE}" image "${IMAGE_RESULT}"
require "${SWITCH_BUILD_KIT}" build-script "${BUILD_SCRIPT_RESULT}"

if [ "${failed}" -eq 0 ]; then
    echo "gate: every required job succeeded"
    exit 0
fi
exit 1
