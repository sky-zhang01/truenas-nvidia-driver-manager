#!/usr/bin/env bash
# Controls for the inline README installation YAML; no Docker or network needed.
set -Eeuo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
. "${SCRIPT_DIR}/test-lib.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
mkdir -p "${WORK}/deploy"
reset_fixture() {
    cp "${ROOT}/deploy/compose.yaml" "${WORK}/deploy/compose.yaml"
    cp "${ROOT}/README.md" "${WORK}/README.md"
    cp "${ROOT}/README.zh-CN.md" "${WORK}/README.zh-CN.md"
}
expect_check() {
    local want="$1" description="$2" rc=0
    python3 "${SCRIPT_DIR}/check-readme-compose.py" --root "${WORK}" >"${WORK}/out" 2>"${WORK}/err" || rc=$?
    if [ "${rc}" -eq "${want}" ]; then pass "${description}"; else
        fail "${description}: expected ${want}, got ${rc}: $(cat "${WORK}/err")"
    fi
}
reset_fixture
expect_check 0 'both current README blocks match the template'
for name in README.md README.zh-CN.md; do
    reset_fixture
    python3 - "${WORK}/${name}" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1]); p.write_text(p.read_text().replace('runtime: runc', 'runtime: nvidia'))
PY
    expect_check 1 "${name}: changed runtime is rejected"
    reset_fixture
    python3 - "${WORK}/${name}" <<'PY'
from pathlib import Path
import re, sys
p = Path(sys.argv[1]); text = p.read_text(); block = re.search(r'```yaml\n.*?\n```', text, re.S)[0]
p.write_text(text.replace(block, '<!--\n' + block + '\n-->'))
PY
    expect_check 1 "${name}: YAML hidden in a comment cannot satisfy the check"
    reset_fixture
    python3 - "${WORK}/${name}" <<'PY'
from pathlib import Path
import re, sys
p = Path(sys.argv[1]); text = p.read_text(); block = re.search(r'```yaml\n.*?\n```', text, re.S)[0]
p.write_text(text + '\n' + block + '\n')
PY
    expect_check 1 "${name}: duplicate YAML is rejected"
done
reset_fixture
python3 - "${WORK}" <<'PY'
from pathlib import Path
import sys
root = Path(sys.argv[1])
for name in ['deploy/compose.yaml', 'README.md', 'README.zh-CN.md']:
    p = root / name; text = p.read_text().replace('runtime: runc', 'runtime: runc # retained comment')
    if name.startswith('README'):
        text = text.replace('services:\n', '# ignored full line\nservices:\n\n')
        text = text.replace('```yaml', '~~~yml').replace('8199:8199"\n```', '8199:8199"\n~~~')
    p.write_text(text)
PY
expect_check 0 'full-line, inline comments, blank lines and tilde yml fences are accepted'
reset_fixture
python3 - "${WORK}/README.md" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1]); p.write_text(p.read_text().replace('runtime: runc', 'runtime: r<!-- injected -->unc'))
PY
expect_check 1 'HTML comments inside fenced YAML are literal and cannot hide drift'
reset_fixture
python3 - "${WORK}/README.md" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1]); p.write_text(p.read_text().replace('    runtime:', '  runtime:'))
PY
expect_check 1 'indentation drift is rejected'
reset_fixture
python3 - "${WORK}/README.md" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1]); p.write_text(p.read_text().replace('    runtime: runc\n', ''))
PY
expect_check 1 'a missing required line is rejected'
reset_fixture
python3 - "${WORK}" <<'PY'
from pathlib import Path
import sys
root = Path(sys.argv[1])
for name in ['deploy/compose.yaml', 'README.md', 'README.zh-CN.md']:
    p = root / name
    p.write_text(p.read_text().replace('runtime: runc', '''runtime: "runc # quoted \\"value\\"" # comment
    label: 'it''s # quoted' # comment'''))
PY
expect_check 0 'quoted hashes and escaped quote characters survive comment stripping'
python3 - "${WORK}/README.zh-CN.md" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1]); p.write_text(p.read_text().replace('it\'\'s # quoted', 'it\'\'s # changed'))
PY
expect_check 1 'a changed hash inside quotes is rejected'
reset_fixture
python3 - "${WORK}/README.md" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1]); p.write_text(p.read_text().replace('\n```\n\n> [!WARNING]', '\n\n> [!WARNING]', 1))
PY
expect_check 1 'an unclosed YAML fence is rejected'
reset_fixture
rm "${WORK}/README.md"
expect_check 1 'a missing README is rejected'
finish_tests "README compose controls: ${FAILURES} failed" "README compose controls: ${PASS} passed"
