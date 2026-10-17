#!/usr/bin/env bash
# check-red-lines-test.sh — controls for the red-line scan.
# Runs the real guard against three inputs: a clean tree (rc 0), a tree with a
# red-line literal (rc non-zero), and a `grep` that cannot read the tree
# (rc non-zero: a failed scan is not "no hit").
set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GUARD="${SCRIPT_DIR}/check-red-lines.sh"

[ -f "${GUARD}" ] || { echo "check-red-lines-test: missing ${GUARD}" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

. "${SCRIPT_DIR}/test-lib.sh"

mkdir -p "${WORK}/clean"
printf 'fn main() {\n    println!("safe");\n}\n' > "${WORK}/clean/input.rs"
if bash "${GUARD}" "${WORK}/clean" >"${WORK}/clean.out" 2>"${WORK}/clean.err"; then
    pass "clean tree passes"
else
    fail "clean tree: guard failed: $(cat "${WORK}/clean.err")"
fi

mkdir -p "${WORK}/hit"
printf 'fn main() {\n    let mode = "docker.update";\n}\n' > "${WORK}/hit/input.rs"
if bash "${GUARD}" "${WORK}/hit" >"${WORK}/hit.out" 2>"${WORK}/hit.err"; then
    fail "hit tree: guard reported success"
else
    if grep -q "red-line literal 'docker.update'" "${WORK}/hit.err"; then
        pass "hit tree fails and names the literal"
    else
        fail "hit tree: stderr does not name the literal: $(cat "${WORK}/hit.err")"
    fi
fi

mkdir -p "${WORK}/bin"
cat >"${WORK}/bin/grep" <<'GREP_SHIM'
#!/bin/sh
# Fail the recursive scan only; the self-test and other calls use the real grep.
if [ "${1:-}" = "-rnaF" ]; then
    echo "injected recursive scan read failure" >&2
    exit 2
fi
exec /usr/bin/grep "$@"
GREP_SHIM
chmod 0700 "${WORK}/bin/grep"
if PATH="${WORK}/bin:${PATH}" bash "${GUARD}" "${WORK}/clean" >"${WORK}/scan.out" 2>"${WORK}/scan.err"; then
    fail "scan failure: guard reported success (a failed scan is not 'no hit')"
else
    if grep -q "scanning" "${WORK}/scan.err" && grep -q "grep rc=2" "${WORK}/scan.err"; then
        pass "scan failure fails and names grep's status"
    else
        fail "scan failure: stderr does not name the failure: $(cat "${WORK}/scan.err")"
    fi
fi

finish_tests "check-red-lines-test: ${FAILURES} control(s) failed" \
    "check-red-lines-test: all controls passed"
