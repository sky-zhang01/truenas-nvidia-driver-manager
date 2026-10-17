#!/usr/bin/env bash
# e2e-state.sh — this run's e2e data directory on the job container's /dev/shm.
#
# The fake host maps every virtual path (credentials, upload staging, task
# records) under the server process's temp directory, and the credentials
# rewrites publish atomically with several fsyncs each — on the runner's
# shared storage those queue behind disk-bound jobs. /dev/shm must therefore
# really be a tmpfs of at least 32000 KiB (the suite peaks around 10 MiB;
# the threshold keeps a 3x margin); anything else fails instead of silently
# falling back. Publishes NVM_E2E_DATA_DIR and NVM_E2E_SERVER_LOG for the
# playwright steps via $GITHUB_ENV.
set -Eeuo pipefail

fs_type="$(findmnt -no FSTYPE /dev/shm)"
size_kib="$(df -Pk /dev/shm | awk 'NR==2 {print $2}')"
[ "${fs_type}" = "tmpfs" ] || { echo "e2e: /dev/shm is '${fs_type}', not tmpfs" >&2; exit 1; }
[ "${size_kib}" -ge 32000 ] || { echo "e2e: /dev/shm holds only ${size_kib} KiB" >&2; exit 1; }
data_dir="$(mktemp -d -p /dev/shm nvm-e2e-XXXXXX)"
if [ -n "${GITHUB_ENV:-}" ]; then
    printf 'NVM_E2E_DATA_DIR=%s\n' "${data_dir}" >>"${GITHUB_ENV}"
    printf 'NVM_E2E_SERVER_LOG=%s\n' "${data_dir}/fake-host-server.log" >>"${GITHUB_ENV}"
fi
echo "e2e: data dir ${data_dir} on ${fs_type} of ${size_kib} KiB"
