#!/usr/bin/env bash
# Run only self-tests selected from the shared input table.
set -Eeuo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
exec python3 "${ROOT}/ci/checks.py" --controls
