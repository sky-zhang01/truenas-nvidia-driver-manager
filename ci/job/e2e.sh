#!/usr/bin/env bash
# Shared e2e job body; workflows supply the runtime, checkout and caches.
#   server              build the release fake host (the one place for its build command)
#   web                 install web dependencies and build web/dist
#   browsers            install Chromium and its system libraries
#   prepare             server, web and browsers (local use)
#   state               create this run's data directory on /dev/shm
#   run [SHARD TOTAL]   run the browser suite, or only shard SHARD of TOTAL
# NVM_E2E_SERVER_BIN, when set, names a prebuilt fake host (relative to the repository
# root) that the run phase starts instead of building it.
set -Eeuo pipefail

as_root() {
    if [ "$(id -u)" -eq 0 ]; then "$@"; else sudo -- "$@"; fi
}

case "${1:-run}" in
    server) cargo build --locked --release --example fake_host_server ;;
    web)
        npm --prefix web ci
        npm --prefix web run build
        ;;
    browsers)
        if [ -d /etc/apt/apt.conf.d ]; then
            printf '%s\n' 'Acquire::Retries "5";' 'Acquire::http::Timeout "30";' 'Acquire::https::Timeout "30";' \
                | as_root tee /etc/apt/apt.conf.d/80-download-resilience >/dev/null
        fi
        (cd web && npx playwright install --with-deps chromium)
        ;;
    prepare) bash "$0" server && bash "$0" web && bash "$0" browsers ;;
    state) bash ci/e2e-state.sh ;;
    run)
        shard=()
        case "$#" in
            0|1) ;;
            3)
                [[ "$2" =~ ^[1-9][0-9]*$ && "$3" =~ ^[1-9][0-9]*$ && "$2" -le "$3" ]] \
                    || { echo "e2e: shard must be SHARD TOTAL with 1 <= SHARD <= TOTAL" >&2; exit 2; }
                shard=("--shard=$2/$3")
                ;;
            *) echo "e2e: run takes no arguments or SHARD TOTAL" >&2; exit 2 ;;
        esac
        if [ -n "${NVM_E2E_SERVER_BIN:-}" ]; then
            # Artifact transfer drops the executable bit.
            [ -f "${NVM_E2E_SERVER_BIN}" ] || { echo "e2e: ${NVM_E2E_SERVER_BIN} is missing" >&2; exit 1; }
            chmod +x "${NVM_E2E_SERVER_BIN}"
        fi
        cd web
        npx playwright test ${shard[@]+"${shard[@]}"}
        ;;
    *) echo "unknown e2e phase" >&2; exit 2 ;;
esac
