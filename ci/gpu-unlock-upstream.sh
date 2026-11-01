#!/usr/bin/env bash
# gpu-unlock-upstream.sh — compare the upstream files the firmware generation follows
# (third_party/gpu-unlock/UPSTREAM.tsv) with the head of upstream's default branch.
#
# usage: gpu-unlock-upstream.sh [checkout]
#   Without a checkout, clone upstream. Fails, naming each file, when a recorded file differs or
#   is gone; reports the pinned and the head commit either way.
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
table="${repo_root}/third_party/gpu-unlock/UPSTREAM.tsv"
value() { awk -F '\t' -v key="$1" '$1 == key { print $2 }' "${table}"; }
checkout="${1:-}"
if [ -z "${checkout}" ]; then
    work="$(mktemp -d)"
    trap 'rm -rf "${work}"' EXIT
    checkout="${work}/upstream"
    git clone --quiet --depth 1 "https://github.com/$(value repository).git" "${checkout}"
fi
echo "gpu-unlock-upstream: pinned $(value commit), upstream head $(git -C "${checkout}" rev-parse HEAD)"
changed=0
while IFS=$'\t' read -r path sha256; do
    case "${path}" in '' | '#'* | repository | commit) continue ;; esac
    actual="$(sha256sum "${checkout}/${path}" 2>/dev/null | cut -d ' ' -f 1 || true)"
    if [ "${actual}" != "${sha256}" ]; then
        echo "gpu-unlock-upstream: ${path} changed upstream; port the change to the firmware generation" >&2
        changed=1
    fi
done < "${table}"
[ "${changed}" -ne 0 ] || echo 'gpu-unlock-upstream: every recorded file is unchanged'
exit "${changed}"
