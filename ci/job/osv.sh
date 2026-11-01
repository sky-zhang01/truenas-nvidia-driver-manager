#!/usr/bin/env bash
# Shared osv job body; workflows supply the runtime and checkout.
set -Eeuo pipefail

# Expiry is evaluated on the clock as well as on lock-input changes.
status=0
python3 - <<'EXPIRY' || status=1
import datetime
import pathlib
import tomllib
now = datetime.datetime.now(datetime.timezone.utc)
policy = tomllib.loads(pathlib.Path('osv-scanner.toml').read_text())
expired = []
def visit(value):
    if isinstance(value, dict):
        until = value.get('ignoreUntil')
        if until is not None and until <= now:
            expired.append(str(value.get('id', value.get('reason', 'exception'))))
        for item in value.values():
            visit(item)
    elif isinstance(value, list):
        for item in value:
            visit(item)
visit(policy)
if expired:
    raise SystemExit('expired OSV exceptions: ' + ', '.join(expired))
print('OSV exception expiry: passed')
EXPIRY
bash ci/osv-scan.sh || status=1
exit "$status"
