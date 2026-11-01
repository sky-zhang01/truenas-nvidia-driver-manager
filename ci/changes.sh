#!/usr/bin/env bash
# changes.sh — select the union of checks that read the changed inputs.
set -Eeuo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "${SCRIPT_DIR}/checks.py" "$@"
