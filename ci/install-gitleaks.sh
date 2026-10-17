#!/usr/bin/env bash
# Install a fixed release only after checking its pinned archive digest (the
# SHA-256 published in the release's checksums file, verified independently).
set -Eeuo pipefail
set +x

if [ "$#" -ne 1 ]; then
    echo "usage: install-gitleaks.sh <tool-directory>" >&2
    exit 2
fi
version=8.30.1
archive_sha256=551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb
# The control tests point GITLEAKS_ARCHIVE at a synthetic archive and pass its digest.
expected="${GITLEAKS_ARCHIVE_SHA256:-${archive_sha256}}"
if ! [[ "${expected}" =~ ^[a-f0-9]{64}$ ]]; then
    echo "gitleaks: the archive digest must be 64 lowercase hex characters" >&2
    exit 1
fi
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT
archive="${GITLEAKS_ARCHIVE:-${work}/gitleaks.tar.gz}"
if [ -z "${GITLEAKS_ARCHIVE:-}" ]; then
    curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
        --retry 3 --connect-timeout 15 --max-time 120 \
        "https://github.com/gitleaks/gitleaks/releases/download/v${version}/gitleaks_${version}_linux_x64.tar.gz" \
        --output "${archive}"
fi
node - "${archive}" "${expected}" <<'NODE'
const fs = require('node:fs');
const crypto = require('node:crypto');
try {
    const actual = crypto.createHash('sha256').update(fs.readFileSync(process.argv[2])).digest('hex');
    if (actual !== process.argv[3]) throw new Error();
} catch {
    console.error('gitleaks: archive integrity check failed');
    process.exit(1);
}
NODE
tar -xzf "${archive}" -C "${work}" gitleaks
[ -f "${work}/gitleaks" ] && [ ! -L "${work}/gitleaks" ] || {
    echo "gitleaks: archive does not contain a regular executable" >&2
    exit 1
}
chmod 755 "${work}/gitleaks"
[ "$("${work}/gitleaks" version)" = "${version}" ] || {
    echo "gitleaks: executable version differs from the pinned release" >&2
    exit 1
}
mkdir -p "$1"
install -m 755 "${work}/gitleaks" "$1/gitleaks"
echo "gitleaks: verified release ${version} installed"
