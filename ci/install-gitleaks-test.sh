#!/usr/bin/env bash
# Offline installer controls use a synthetic archive, never a downloaded tool.
set -Eeuo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "${SCRIPT_DIR}/test-lib.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
mkdir -p "${WORK}/archive" "${WORK}/bin"
cat >"${WORK}/archive/gitleaks" <<'SH'
#!/usr/bin/env bash
[ "${1:-}" = version ] || exit 2
echo 8.30.1
SH
chmod +x "${WORK}/archive/gitleaks"
tar -czf "${WORK}/tool.tar.gz" -C "${WORK}/archive" gitleaks
digest="$(node -e 'const fs=require("node:fs"),c=require("node:crypto"); console.log(c.createHash("sha256").update(fs.readFileSync(process.argv[1])).digest("hex"))' "${WORK}/tool.tar.gz")"
if GITLEAKS_ARCHIVE="${WORK}/tool.tar.gz" GITLEAKS_ARCHIVE_SHA256="${digest}" \
    bash "${SCRIPT_DIR}/install-gitleaks.sh" "${WORK}/bin" >"${WORK}/out" 2>"${WORK}/err" \
    && [ "$("${WORK}/bin/gitleaks" version)" = 8.30.1 ]; then
    pass 'a matching offline archive installs the pinned version'
else
    fail 'a matching offline archive must install'
fi
for digest in '' not-a-digest "$(printf '%064d' 0)"; do
    if GITLEAKS_ARCHIVE="${WORK}/tool.tar.gz" GITLEAKS_ARCHIVE_SHA256="${digest}" \
        bash "${SCRIPT_DIR}/install-gitleaks.sh" "${WORK}/rejected" >"${WORK}/out" 2>"${WORK}/err"; then
        fail 'a missing, invalid or mismatched pin must fail'
    else
        pass 'a missing, invalid or mismatched pin fails'
    fi
done
check 'rejected archives never create the tool destination' test ! -e "${WORK}/rejected"
finish_tests 'installer controls failed' "installer controls: ${PASS} passed"
