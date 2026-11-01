#!/usr/bin/env bash
# Shared guards job body; workflows supply the runtime and checkout.
set -Eeuo pipefail

bash ci/guards-test.sh
bash ci/guards.sh
bash ci/check-red-lines-test.sh
bash ci/check-red-lines.sh
