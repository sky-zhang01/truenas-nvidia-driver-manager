#!/usr/bin/env bash
# osv-scan.sh — install the pinned osv-scanner and scan both lockfiles.
#
# The calling workflow supplies OSV_SCANNER_VERSION; the binary is verified
# against the release SHA256SUMS.
# To accept a finding temporarily, add an osv-scanner.toml to that change and
# extend the scan command with --config=osv-scanner.toml.
set -Eeuo pipefail

: "${OSV_SCANNER_VERSION:?set OSV_SCANNER_VERSION}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

base="https://github.com/google/osv-scanner/releases/download/${OSV_SCANNER_VERSION}"
cd "${work}"
curl -fsSL --proto '=https' --retry 5 --retry-delay 3 --retry-connrefused \
    --connect-timeout 15 --max-time 300 \
    -o osv-scanner_linux_amd64 "${base}/osv-scanner_linux_amd64"
curl -fsSL --proto '=https' --retry 5 --retry-delay 3 --retry-connrefused \
    --connect-timeout 15 --max-time 300 \
    -o osv-scanner_SHA256SUMS "${base}/osv-scanner_SHA256SUMS"
sha256sum --ignore-missing -c osv-scanner_SHA256SUMS
mkdir "${work}/bin"
install -m 0755 osv-scanner_linux_amd64 "${work}/bin/osv-scanner"

cd "${repo_root}"
"${work}/bin/osv-scanner" scan source --lockfile=Cargo.lock --lockfile=web/package-lock.json
