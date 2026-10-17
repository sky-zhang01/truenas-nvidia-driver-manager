# Shared reporting for shell test scripts.
FAILURES=0
PASS=0
pass() { PASS=$((PASS + 1)); echo "PASS: $1"; }
fail() { echo "FAIL: $1" >&2; FAILURES=$((FAILURES + 1)); }
check() { # check <description> <command...>
    local desc="$1"; shift
    if "$@"; then
        pass "${desc}"
    else
        fail "${desc}"
        if [ -n "${LAST_LOG:-}" ] && [ -f "${LAST_LOG}" ]; then
            echo "build output for ${desc}:" >&2
            cat "${LAST_LOG}" >&2
        fi
    fi
}

finish_tests() { # finish_tests <failure-message> <success-message>
    echo
    if [ "${FAILURES}" -ne 0 ]; then
        echo "$1" >&2
        exit 1
    fi
    echo "$2"
}
