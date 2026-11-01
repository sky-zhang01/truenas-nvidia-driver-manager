#!/usr/bin/env bash
# Shared body of the scheduled gpu-unlock jobs; the workflow supplies the runtime and checkout.
#   upstream — the upstream files the firmware generation follows are unchanged
#   firmware — the pinned NVIDIA installer alone gives exactly the fixed firmware
set -Eeuo pipefail

case "${1:-}" in
    upstream)
        bash ci/gpu-unlock-upstream.sh
        ;;
    firmware)
        work="$(mktemp -d)"
        trap 'rm -rf "${work}"' EXIT
        # The test checks the file against the pinned size and SHA-256 before it uses it.
        curl -fsSL --proto '=https' --retry 5 --retry-connrefused --connect-timeout 20 --max-time 1800 \
            -o "${work}/installer.run" \
            https://download.nvidia.com/XFree86/Linux-x86_64/610.43.03/NVIDIA-Linux-x86_64-610.43.03-no-compat32.run
        NVM_GPU_UNLOCK_INSTALLER="${work}/installer.run" \
            cargo test --locked --release --test gpu_unlock_firmware -- --ignored --nocapture
        ;;
    *)
        echo 'usage: gpu-unlock.sh upstream|firmware' >&2
        exit 2
        ;;
esac
