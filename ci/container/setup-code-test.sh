#!/usr/bin/env bash
# Exercise the same parser used by the real image test, without Docker.
set -Eeuo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "${SCRIPT_DIR}/../test-lib.sh"
parse() { printf '%s\n' "$1" | python3 "${SCRIPT_DIR}/setup-code.py"; }
first='{"event":"setup_code_issued","msg":"SETUP CODE: 7KQ2-M9XD-4TWB-HE6R. Open the manager page.","valid_hours":24}'
second='{"event":"setup_code_issued","msg":"SETUP CODE: 1111-2222-3333-4444. Open the manager page.","valid_hours":24}'
check 'the code is read from the message' test "$(parse "${first}")" = 7KQ2-M9XD-4TWB-HE6R
check 'the newest issued code wins' test "$(parse "${first}"$'\n'"${second}")" = 1111-2222-3333-4444
for input in '' '{"event":"listening","msg":"SETUP CODE: 7KQ2-M9XD-4TWB-HE6R. "}' \
    '{"event":"setup_code_issued","setup_code":"7KQ2-M9XD-4TWB-HE6R","valid_hours":24}' \
    '{"event":"setup_code_issued","msg":"SETUP CODE: short. ","valid_hours":24}' \
    '{"event":"setup_code_issued","msg":"SETUP CODE: 7KQ2-M9XD-4TWB-HE6R. ","valid_hours":1}'; do
    if parse "${input}" >/dev/null 2>&1; then fail 'missing or malformed code was accepted';
    else pass 'missing or malformed code is refused'; fi
done
finish_tests 'setup-code controls failed' 'setup-code controls passed'
