#!/usr/bin/env bash
# Compare real local history without creating commits or changing refs.
set -Eeuo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "${SCRIPT_DIR}/test-lib.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
REPO="$(git -C "${SCRIPT_DIR}" rev-parse --show-toplevel)"
SOURCE="$(git -C "${REPO}" rev-parse --verify HEAD)"
COMMITTED_RULES="$(git -C "${REPO}" ls-tree --name-only "${SOURCE}" -- export/rules)"
HAS_PRIVATE_RULES=false
if [ -n "${COMMITTED_RULES}" ] || [ -f "${REPO}/.gitea/workflows/ci.yml" ]; then
    HAS_PRIVATE_RULES=true
else
    echo 'successful drift comparison controls: private export/rules is not present in the exported tree'
fi
node - >"${WORK}/manifest.json" <<'NODE'
console.log(JSON.stringify({files:[{path:'new-public.txt'}]}));
NODE
added_hash="$(node -e "console.log(require('node:crypto').createHash('sha256').update('new-public.txt').digest('hex'))")"
removed_hash="$(node -e "console.log(require('node:crypto').createHash('sha256').update('Cargo.toml').digest('hex'))")"
# Keep checking output redaction when the private comparison policy is absent.
comparison_succeeded=false
if bash "${SCRIPT_DIR}/public-export-drift.sh" HEAD "${WORK}/manifest.json" >"${WORK}/out" 2>"${WORK}/err"; then
    comparison_succeeded=true
fi
if "${HAS_PRIVATE_RULES}"; then
    if "${comparison_succeeded}" \
        && grep -qF "+ [path sha256:${added_hash}]" "${WORK}/out" && grep -qF -- "- [path sha256:${removed_hash}]" "${WORK}/out"; then
        pass 'membership differences identify added and removed paths by digest'
    else
        fail 'membership differences must be identified by digest'
    fi
fi
if ! grep -qF 'new-public' "${WORK}/out" && ! grep -qF 'Cargo.toml' "${WORK}/out"; then
    pass 'path values are never logged without requiring private policy'
else
    fail 'path values must be hidden without private policy'
fi
node - >"${WORK}/manifest.json" <<'NODE'
console.log(JSON.stringify({files:[{path:'sample-identity.example/Personal_ID/name.txt'}]}));
NODE
if "${HAS_PRIVATE_RULES}"; then
    if bash "${SCRIPT_DIR}/public-export-drift.sh" HEAD "${WORK}/manifest.json" >"${WORK}/out" 2>"${WORK}/err" \
        && ! grep -qiE 'sample-identity|Personal_ID|name.txt' "${WORK}/out" "${WORK}/err"; then
        pass 'synthetic private host and term path values are hidden'
    else
        fail 'synthetic private path values must be hidden'
    fi
fi
if bash "${SCRIPT_DIR}/public-export-drift.sh" invalid-commit "${WORK}/manifest.json" >"${WORK}/out" 2>"${WORK}/err"; then
    fail 'a missing base must fail'
else
    pass 'a missing base fails'
fi
finish_tests 'drift controls failed' "drift controls: ${PASS} passed"
