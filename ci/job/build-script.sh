#!/usr/bin/env bash
# Shared build-script job body; workflows supply the runtime and checkout.
set -Eeuo pipefail

bash ci/build-script/make-fake-run-test.sh
bash ci/build-script/prepare-env.sh
bash tools/build-kit.sh check
bash ci/build-script/test.sh
