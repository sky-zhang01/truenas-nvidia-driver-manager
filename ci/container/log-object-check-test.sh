#!/usr/bin/env bash
# Container controls run without a daemon through the existing CI entry point.
# The log predicate is fed the same shapes it must accept and reject.
set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="${SCRIPT_DIR}/log-object-check.py"

command -v python3 >/dev/null 2>&1 || { echo "log-object-check-test: python3 is required" >&2; exit 1; }
[ -f "${CHECK}" ] || { echo "log-object-check-test: missing ${CHECK}" >&2; exit 1; }

. "${SCRIPT_DIR}/../test-lib.sh"

expect() { # expect <rc> <description> <input>
    local want="$1" desc="$2" input="$3" got=0
    printf '%b' "${input}" | python3 "${CHECK}" >/dev/null 2>&1 || got=$?
    if [ "${got}" -eq "${want}" ]; then
        pass "${desc} (rc=${got})"
    else
        fail "${desc}: want rc=${want}, got rc=${got}"
    fi
}

expect 0 "an object passes" '{"event":"starting","version":"0.1.0"}\n'
expect 0 "blank lines are skipped" '\n{"event":"x"}\n\n'
expect 0 "an ASCII object with CRLF passes" '{"event":"x"}\r\n'
expect 1 "bad JSON is rejected" 'not-json\n'
expect 1 "a scalar is rejected" '123\n'
expect 1 "an array is rejected" '[]\n'
expect 1 "a non-ASCII line is rejected" '{"event":"启动"}\n'
expect 1 "an object after a bad line is rejected" '{"event":"x"}\n123\n'
# U+00A0 (NBSP) is bytes c2 a0: a raw non-ASCII line and a trailing NBSP, which
# a `strip()` before the ASCII check would have removed.
NBSP=$(printf '\302\240')
expect 1 "a line of NBSP only is rejected" "${NBSP}\n"
expect 1 "an object followed by NBSP is rejected" "{\"event\":\"x\"}${NBSP}\n"
expect 1 "an object followed by a CJK space is rejected" '{"event":"x"}。\n'

check 'network lifecycle controls' bash "${SCRIPT_DIR}/network-test.sh"
check 'stale resource cleanup controls' bash "${SCRIPT_DIR}/stale-cleanup-test.sh"

finish_tests "container controls: ${FAILURES} control group(s) failed" \
    "container controls: all controls passed"
