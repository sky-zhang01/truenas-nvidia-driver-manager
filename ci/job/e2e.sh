#!/usr/bin/env bash
# Shared e2e job body; workflows supply the runtime and checkout.
set -Eeuo pipefail

case "${1:-run}" in
    prepare)
        cargo build --locked --release --example fake_host_server
        npm --prefix web ci
        npm --prefix web run build
        (cd web
            printf '%s\n' 'Acquire::Retries "5";' 'Acquire::http::Timeout "30";' 'Acquire::https::Timeout "30";' > /etc/apt/apt.conf.d/80-download-resilience
            npx playwright install --with-deps chromium
        )
        ;;
    state) bash ci/e2e-state.sh ;;
    run) cd web; npx playwright test ;;
    *) echo "unknown e2e phase" >&2; exit 2 ;;
esac
