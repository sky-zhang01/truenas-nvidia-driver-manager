#!/usr/bin/env bash
# Host safety forbids docker.update, --collect and CollectMode in src/. Test code
# spells them with concat!, so a plain fixed-string scan has no false positives.
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

scan_dir="${1:-src}"
if [ ! -d "${scan_dir}" ]; then
    echo "check-red-lines: scan directory '${scan_dir}' does not exist" >&2
    exit 1
fi

# Engine self-test: prove grep can match a known line before trusting a clean scan.
selftest="$(mktemp)"
trap 'rm -f "${selftest}"' EXIT
printf 'docker.update\n--collect\nCollectMode\n' > "${selftest}"
for needle in 'docker.update' '--collect' 'CollectMode'; do
    grep -qF -- "${needle}" "${selftest}" \
        || { echo "check-red-lines self-test failed for '${needle}'" >&2; exit 1; }
done

status=0
for needle in 'docker.update' '--collect' 'CollectMode'; do
    # Distinguish grep's exit codes: 0 = hit, 1 = no hit, anything else = the
    # scan itself failed. Treating a read failure as “no hit” would report OK
    # without an actual scan.
    if hits="$(grep -rnaF -- "${needle}" "${scan_dir}")"; then
        echo "check-red-lines: red-line literal '${needle}' found in ${scan_dir}:" >&2
        printf '%s\n' "${hits}" >&2
        status=1
    else
        rc=$?
        if [ "${rc}" -ne 1 ]; then
            echo "check-red-lines: scanning ${scan_dir} for '${needle}' failed (grep rc=${rc})" >&2
            exit 1
        fi
    fi
done
if [ "${status}" -ne 0 ]; then
    exit 1
fi
echo "check-red-lines: OK, no red-line literals in ${scan_dir}"
