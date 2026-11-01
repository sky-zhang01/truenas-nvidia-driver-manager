#!/usr/bin/env bash
# Shared compose job body; workflows supply the runtime and checkout.
set -Eeuo pipefail

bash ci/check-compose-test.sh
bash ci/check-compose.sh
bash ci/check-readme-compose-test.sh
python3 ci/check-readme-compose.py
