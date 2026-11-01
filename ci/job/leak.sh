#!/usr/bin/env bash
# Shared leak job body; workflows supply the runtime and checkout.
set -Eeuo pipefail

case "${1:?select a leak phase}" in
    controls)
        bash ci/export-public-test.sh
        bash ci/public-leak-check-test.sh
        bash ci/public-export-drift-test.sh
        bash ci/install-gitleaks-test.sh
        bash ci/public-leak-ci-test.sh
        bash ci/github-ci-test.sh
        ;;
    install)
        bash ci/install-gitleaks.sh "${RUNNER_TEMP}/public-leak-tools"
        ;;
    export)
        set -Eeuo pipefail
        work="$(mktemp -d "${RUNNER_TEMP}/public-export.XXXXXXXX")"
        printf 'PUBLIC_SCAN_WORK=%s\n' "${work}" >>"${GITHUB_ENV}"
        test "$(git rev-parse HEAD)" = "${PUBLIC_EXPORT_HEAD}"
        bash ci/export-public.sh --source "${PUBLIC_EXPORT_HEAD}" --output "${work}/tree" --manifest "${work}/manifest.json"
        if [ "${GITHUB_EVENT_NAME}" = pull_request ]; then
            bash ci/public-export-drift.sh "${PUBLIC_EXPORT_BASE}" "${work}/manifest.json"
        fi
        ;;
    test)
        set -Eeuo pipefail
        cd "${PUBLIC_SCAN_WORK}/tree"
        trap 'rm -rf -- .git' EXIT
        git init --quiet --initial-branch=public-controls
        git add --all
        git -c user.name='CI Controls' -c user.email='ci@example.invalid' -c core.hooksPath=/dev/null commit --quiet --no-gpg-sign -m 'Public tree test fixture'
        bash ci/export-public-test.sh
        bash ci/public-export-drift-test.sh
        bash ci/public-leak-ci-test.sh
        ;;
    scan)
        set -Eeuo pipefail
        set +x
        PATH="${RUNNER_TEMP}/public-leak-tools:${PATH}" bash ci/public-leak-check.sh --root "${PUBLIC_SCAN_WORK}/tree"
        ;;
    cleanup)
        if [ -n "${PUBLIC_SCAN_WORK:-}" ]; then
            rm -rf -- "${PUBLIC_SCAN_WORK}"
        fi
        ;;
    *) echo "unknown leak phase" >&2; exit 2 ;;
esac
