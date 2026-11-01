#!/usr/bin/env bash
# Shared web job body; workflows supply the runtime and checkout.
set -Eeuo pipefail

npm --prefix web ci
npm --prefix web run build
npm --prefix web run test:unit
