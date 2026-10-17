#!/usr/bin/env bash
# Exercise the private workflow's gate body without running the workflow.
set -Eeuo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "${SCRIPT_DIR}/test-lib.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
cd "${SCRIPT_DIR}/.."
workflow=.gitea/workflows/ci.yml
if [ ! -f "${workflow}" ]; then
    echo 'private workflow controls: not present in the exported tree'
    exit 0
fi
awk '
    /^  gate:/ { gate=1; next }
    gate && /^  [[:alnum:]-]+:/ { gate=0 }
    gate && /      - name: judge the run/ { judge=1; next }
    judge && /^        run: \|/ { body=1; next }
    body && /^          / { sub(/^          /, ""); print; next }
    body { exit }
' "${workflow}" >"${WORK}/gate.sh"
check 'the private gate depends on the leak job' grep -qF 'needs: [changes, static, rust, e2e, image, build-script, public-leak]' "${workflow}"
check 'the private gate consumes the leak result' grep -qF 'PUBLIC_LEAK_RESULT' "${WORK}/gate.sh"
SUCCESS=(CHANGES_RESULT=success STATIC_RESULT=success RUST_RESULT=success E2E_RESULT=success IMAGE_RESULT=success BUILD_SCRIPT_RESULT=success SWITCH_RUST=true SWITCH_E2E=true SWITCH_IMAGE=true SWITCH_BUILD_KIT=true)
if env "${SUCCESS[@]}" PUBLIC_LEAK_RESULT=success bash "${WORK}/gate.sh" >"${WORK}/out" 2>"${WORK}/err"; then
    pass 'the leak job success permits the existing gate'
else
    fail 'the leak job success must permit the existing gate'
fi
for result in failure cancelled skipped ''; do
    if env "${SUCCESS[@]}" PUBLIC_LEAK_RESULT="${result}" bash "${WORK}/gate.sh" >"${WORK}/out" 2>"${WORK}/err"; then
        fail 'an unsuccessful or missing leak job must reject the gate'
    else
        pass 'an unsuccessful or missing leak job rejects the gate'
    fi
done
if node - "${workflow}" "${WORK}" <<'NODE'
const fs = require('node:fs');
const source = fs.readFileSync(process.argv[2], 'utf8');
const job = source.split('  public-leak:\n')[1]?.split('\n  static:')[0];
function requireRule(condition, message) {
    if (!condition) throw new Error(message);
}
requireRule(Boolean(job), 'missing private scan job');
const steps = job.split(/(?=^      - (?:name:|uses:))/m);
const hosts = 'PUBLIC_RELEASE_FORBIDDEN_HOSTS';
const terms = 'PUBLIC_RELEASE_FORBIDDEN_TERMS';
for (const key of [hosts, terms]) {
    const binding = key + ': ${{ secrets.' + key + ' }}';
    requireRule(source.split(binding).length === 2, 'policy must have exactly one secret binding');
    const step = steps.find(value => value.includes(binding));
    requireRule(step?.includes('bash ci/public-leak-check.sh'), 'policy must be scoped to scanner step');
    requireRule(!step.includes('bash ci/export-public.sh') && !step.includes('bash ci/public-export-drift.sh'), 'policy must not reach export or drift');
}
requireRule(!job.includes('--generic-only'), 'private scan must require both policies');
requireRule(!source.includes('PUBLIC_' + 'EXPORT_FORBIDDEN'), 'removed policy name must not remain');
requireRule(steps.some(step => step.includes('if: always()') && step.includes('rm -rf')), 'temporary scan tree must be cleaned on failure');
const exported = steps.findIndex(step => step.includes('bash ci/export-public.sh'));
const tested = steps.findIndex(step => step.includes('name: test the exported public tree'));
const scanned = steps.findIndex(step => step.includes('bash ci/public-leak-check.sh'));
requireRule(exported >= 0 && tested > exported && scanned > tested, 'test the exported tree between export and scanning');
const testStep = steps[tested];
requireRule(!testStep.includes('if:'), 'exported tree controls must not be conditional');
requireRule(testStep.includes('cd "${PUBLIC_SCAN_WORK}/tree"'), 'controls must run inside the exported tree');
for (const name of ['export-public', 'public-export-drift', 'public-leak-ci']) {
    requireRule(testStep.includes('bash ci/' + name + '-test.sh'), 'all export controls are required');
}
const body = testStep.split('        run: |\n')[1];
requireRule(Boolean(body), 'exported tree test body is required');
fs.writeFileSync(process.argv[3] + '/exported-tests.sh', body.split('\n').map(line => line.replace(/^          /, '')).join('\n'));
NODE
then
    pass 'private policy stays scoped to scanning and exported controls run before it'
else
    fail 'private scan policy binding, exported controls or cleanup is invalid'
fi
if [ -f "${WORK}/exported-tests.sh" ]; then
    mkdir -p "${WORK}/bin" "${WORK}/export/tree/ci"
    cat >"${WORK}/bin/git" <<'SH'
#!/usr/bin/env bash
set -eu
[ "$PWD" = "${PUBLIC_SCAN_WORK}/tree" ] || exit 91
if [ "$1" = init ]; then mkdir .git; fi
SH
    chmod +x "${WORK}/bin/git"
    for name in export-public public-export-drift public-leak-ci; do
        cat >"${WORK}/export/tree/ci/${name}-test.sh" <<'SH'
#!/usr/bin/env bash
set -eu
[ "$PWD" = "${PUBLIC_SCAN_WORK}/tree" ] && [ -d .git ] || exit 92
name="$(basename "$0" -test.sh)"
echo "${name}" >>"${PUBLIC_SCAN_WORK}/calls"
[ "${name}" != "${EXPORTED_TEST_FAILURE:-}" ] || exit 23
SH
    done
    for failed in none export-public public-export-drift public-leak-ci; do
        : >"${WORK}/export/calls"
        rc=0
        PATH="${WORK}/bin:${PATH}" PUBLIC_SCAN_WORK="${WORK}/export" EXPORTED_TEST_FAILURE="${failed}" \
            bash "${WORK}/exported-tests.sh" >"${WORK}/out" 2>"${WORK}/err" || rc=$?
        want=23
        [ "${failed}" != none ] || want=0
        check "exported controls propagate ${failed} result" test "${rc}" -eq "${want}"
        check "exported controls clean Git metadata after ${failed}" test ! -e "${WORK}/export/tree/.git"
        case "${failed}" in
            export-public) count=1 ;;
            public-export-drift) count=2 ;;
            *) count=3 ;;
        esac
        check "exported controls stop at ${failed}" test "$(wc -l <"${WORK}/export/calls" | tr -d ' ')" -eq "${count}"
    done
fi
if node - "${workflow}" <<'NODE'
const fs = require('node:fs');
const {execFileSync} = require('node:child_process');
const source = fs.readFileSync(process.argv[2], 'utf8');
const job = source.split('  static:\n')[1]?.split('\n  rust:')[0];
function requireRule(condition, message) {
    if (!condition) throw new Error(message);
}
requireRule(Boolean(job), 'license generator tests must run in static');
const header = job.split('    steps:')[0];
requireRule(!/^    if:/m.test(header), 'static must run on every event');
const steps = job.split(/(?=^      - (?:name:|uses:))/m);
const tests = steps.filter(step => step.includes('python3 tools/third-party-licenses-test.py'));
requireRule(tests.length === 1 && !tests[0].includes('if:'), 'license generator tests must run unconditionally');
for (const name of ['Cargo.lock', 'web/package-lock.json', 'tools/third-party-licenses.py', 'tools/third-party-licenses-test.py']) {
    const route = execFileSync('bash', ['ci/changes.sh', '--classify', name], {encoding: 'utf8'}).trim().split(/\s+/);
    requireRule(route.includes('image'), 'license input must select the image build');
}
const e2e = source.split('  e2e:\n')[1]?.split('\n  image:')[0];
requireRule(!e2e.includes('cargo fetch'), 'e2e must not fetch sources for license generation');
requireRule(!source.includes('bash ci/check-licenses.sh'), 'workflow must not check a tracked license artifact');
NODE
then
    pass 'license generator tests always run and license inputs select image'
else
    fail 'license generator test scheduling or image routing is invalid'
fi
finish_tests 'private CI controls failed' "private CI controls: ${PASS} passed"
