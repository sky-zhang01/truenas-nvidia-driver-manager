#!/usr/bin/env bash
# Exercise the private leak contract and shared exported-tree test body.
set -Eeuo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "${SCRIPT_DIR}/test-lib.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
cd "${SCRIPT_DIR}/.."
workflow=.gitea/workflows/checks.yml
if [ ! -f "${workflow}" ]; then
    echo 'private workflow controls: not present in the exported tree'
    exit 0
fi
if node - "${WORK}" <<'NODE'
const fs = require('node:fs');
function requireRule(condition, message) {
    if (!condition) throw new Error(message);
}
const job = (source, name) => source.split('\n  ' + name + ':\n')[1]?.split(/^  [a-z][a-z0-9-]*:\s*$/m)[0];
const body = fs.readFileSync('ci/job/leak.sh', 'utf8');
const phase = name => body.split('    ' + name + ')\n')[1]?.split('\n        ;;')[0];
requireRule(Boolean(phase('scan')) && phase('scan').includes('bash ci/public-leak-check.sh'), 'scan phase must run the real scanner');
requireRule(!body.includes('--generic-only'), 'private scan must require both policies');
requireRule(phase('cleanup')?.includes('rm -rf'), 'cleanup phase must remove the temporary tree');
requireRule(phase('export')?.includes('bash ci/export-public.sh') && phase('export').includes('bash ci/public-export-drift.sh'), 'export must retain drift reporting');
requireRule(phase('export').includes('git rev-parse HEAD') && phase('export').includes('PUBLIC_EXPORT_HEAD'), 'export must verify the exact source');
for (const file of ['checks.yml', 'release-tags.yml', 'scheduled.yml']) {
    const source = fs.readFileSync('.gitea/workflows/' + file, 'utf8');
    const leak = job(source, 'leak'), gate = job(source, 'gate');
    requireRule(Boolean(leak), file + ' needs leak');
    requireRule(leak.includes('    needs: changes\n') && leak.includes("    if: needs.changes.outputs.leak == 'true'\n"), 'leak must consume its switch');
    requireRule(gate?.includes('    if: always()\n'), 'gate must evaluate unsuccessful dependencies on check runs');
    requireRule(gate.includes('LEAK_RESULT: ${{ needs.leak.result }}') && gate.includes('SWITCH_LEAK: ${{ needs.changes.outputs.leak }}'), 'gate must consume leak expectation and result');
    const steps = leak.split(/(?=^      - (?:name:|uses:))/m);
    for (const key of ['PUBLIC_RELEASE_FORBIDDEN_HOSTS', 'PUBLIC_RELEASE_FORBIDDEN_TERMS']) {
        const binding = key + ': ${{ secrets.' + key + ' }}';
        requireRule(source.split(binding).length === 2, 'policy must have exactly one binding in each workflow');
        const step = steps.find(value => value.includes(binding));
        requireRule(step?.includes('run: bash ci/job/leak.sh scan'), 'policy must be scoped to the scan phase');
        requireRule(!step.includes('leak.sh export') && !step.includes('leak.sh test'), 'policy must not reach export or tests');
    }
    requireRule(steps.some(step => step.includes('if: always()') && step.includes('leak.sh cleanup')), 'cleanup must run after failure');
    const index = phase => steps.findIndex(step => step.includes('run: bash ci/job/leak.sh ' + phase));
    requireRule(index('export') >= 0 && index('test') > index('export') && index('scan') > index('test'), 'exported tests must run between export and scan');
    requireRule(!steps[index('test')].includes('if:'), 'exported tests must be unconditional inside the selected job');
    for (const name of ['rust', 'e2e', 'image', 'build-script', 'web', 'compose', 'controls', 'osv']) {
        const selected = job(source, name);
        requireRule(selected?.includes('    needs: changes\n'), name + ' must depend only on changes');
        requireRule(!/^    needs: \[/m.test(selected), 'heavy jobs must not depend on one another');
    }
    requireRule(!source.includes('bash ci/check-licenses.sh'), 'workflow must not check a tracked license artifact');
}
const source = fs.readFileSync('.gitea/workflows/checks.yml', 'utf8');
requireRule(/^name: checks$/m.test(source), 'the required check workflow name must remain stable');
requireRule(source.includes('  pull_request:\n    branches: [main]'), 'checks must run on main pull requests');
requireRule(!/^  (?:push|schedule|workflow_dispatch):/m.test(source) && !/^    paths:/m.test(source), 'PR checks must report without workflow path filtering');
const tested = phase('test');
requireRule(tested?.includes('cd "${PUBLIC_SCAN_WORK}/tree"'), 'controls must run inside the exported tree');
for (const name of ['export-public', 'public-export-drift', 'public-leak-ci']) {
    requireRule(tested.includes('bash ci/' + name + '-test.sh'), 'all export controls are required');
}
fs.writeFileSync(process.argv[2] + '/exported-tests.sh', tested.split('\n').map(line => line.replace(/^        /, '')).join('\n'));
NODE
then
    pass 'private policies, exact export, test ordering, cleanup and dependency shape are preserved'
else
    fail 'private workflow contract is invalid'
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
if node - <<'NODE'
const fs = require('node:fs');
const {execFileSync} = require('node:child_process');
for (const name of ['Cargo.lock', 'web/package-lock.json', 'tools/third-party-licenses.py']) {
    const route = execFileSync('bash', ['ci/changes.sh', '--classify', name], {encoding: 'utf8'}).trim().split(/\s+/);
    if (!route.includes('image')) throw new Error('license input must select image: ' + name);
}
const controlRoute = execFileSync('bash', ['ci/changes.sh', '--classify', 'tools/third-party-licenses-test.py'], {encoding: 'utf8'}).trim().split(/\s+/);
if (!controlRoute.includes('controls')) throw new Error('license self-test must select controls');
const e2e = fs.readFileSync('ci/job/e2e.sh', 'utf8');
if (e2e.includes('cargo fetch')) throw new Error('e2e must not fetch license sources');
NODE
then
    pass 'license inputs select image and browser tests do not fetch license sources'
else
    fail 'license input routing is invalid'
fi
finish_tests 'private CI controls failed' "private CI controls: ${PASS} passed"
