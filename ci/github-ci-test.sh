#!/usr/bin/env bash
# Exercise public workflow contracts and its tracked-source scan without network access.
set -Eeuo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "${SCRIPT_DIR}/test-lib.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
cd "${SCRIPT_DIR}/.."

if node - "${WORK}" <<'NODE'
const fs = require('node:fs');
const source = fs.readFileSync('.github/workflows/ci.yml', 'utf8');
if (/release\.sh --dev|^  dev-image:|REGISTRY_TOKEN|REGISTRY_HOST_VARIABLE/m.test(source)) {
    throw new Error('private dev publication must stay out of public CI');
}
const job = name => source.split('\n  ' + name + ':\n')[1]?.split(/^  [a-z][a-z-]*:\s*$/m)[0];
function requireRule(condition, message) {
    if (!condition) throw new Error(message);
}
const leak = job('public-leak');
requireRule(Boolean(leak), 'public leak job is required');
requireRule(!/^    (?:if|needs):/m.test(leak), 'public scan must run on every event');
requireRule(/^    timeout-minutes: [1-9][0-9]*$/m.test(leak), 'public scan needs a timeout');
requireRule(leak.includes('    permissions:\n      contents: read\n'), 'public scan needs read-only permissions');
requireRule(!/secrets\.|FORBIDDEN_|--content-only/.test(leak), 'public scan must be complete and independent of private policy');
requireRule(leak.includes('persist-credentials: false'), 'checkout must not retain credentials');
requireRule(leak.includes('actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1'), 'reuse the pinned checkout');
requireRule(leak.includes('actions/setup-node@949feb2413d6458794dcd2491c4babbbce0c15c1 # v7.1.0'), 'reuse the pinned Node setup');
const steps = leak.split(/(?=^      - (?:name:|uses:))/m);
const index = command => steps.findIndex(step => step.includes(command));
const install = index('bash ci/install-gitleaks.sh');
const scan = index('bash ci/public-leak-check.sh');
for (const command of ['bash ci/public-leak-check-test.sh', 'bash ci/install-gitleaks-test.sh', 'bash ci/github-ci-test.sh']) {
    requireRule(index(command) >= 0 && index(command) < install, 'controls must precede installation');
}
requireRule(install >= 0 && scan > install, 'install the pinned scanner before scanning');
requireRule(steps.every(step => !/^        if:/m.test(step)), 'public scan steps must not be conditional');
requireRule(steps[scan].includes('--generic-only'), 'public scan must explicitly select generic rules');
const body = steps[scan].split('        run: |\n')[1];
requireRule(Boolean(body), 'public scan body is required');
fs.writeFileSync(process.argv[2] + '/scan.sh', body.split('\n').map(line => line.replace(/^          /, '')).join('\n'));
const gate = job('gate');
requireRule(gate.includes('needs: [changes, static, rust, e2e, image, build-script, public-leak]'), 'gate must depend on public scan');
requireRule(gate.includes('    if: always()'), 'gate must judge unsuccessful dependencies');
requireRule(gate.includes('PUBLIC_LEAK_RESULT: ${{ needs.public-leak.result }}'), 'gate must consume public scan result');
requireRule(gate.includes('run: bash ci/gate.sh'), 'gate must use the shared result checks');
const stat = job('static');
requireRule(!/^    if:/m.test(stat), 'static must run on every event');
for (const command of ['python3 tools/third-party-licenses-test.py', 'bash ci/image-assert-test.sh', 'bash ci/check-readme-compose-test.sh', 'python3 ci/check-readme-compose.py']) {
    const matches = stat.split(/(?=^      - (?:name:|uses:))/m).filter(step => step.includes(command));
    requireRule(matches.length === 1 && !/^        if:/m.test(matches[0]), 'static controls must run unconditionally');
}
const step = (body, name) => body.split(/(?=^      - (?:name:|uses:))/m)
    .find(value => value.startsWith('      - name: ' + name + '\n'));
function writeRun(value, file) {
    const body = value?.split('        run: |\n')[1];
    requireRule(Boolean(body), file + ' needs a shell body');
    fs.writeFileSync(process.argv[2] + '/' + file, body.split('\n').map(line => line.replace(/^          /, '')).join('\n'));
}
const locks = step(job('rust'), 'test host locks across Linux processes');
requireRule(locks?.includes('cargo test --locked --test lock_process --no-run --message-format=json'), 'compile locks as the runner user');
requireRule(!locks.includes('sudo cargo'), 'root must not compile into target');
writeRun(locks, 'locks.sh');
const image = job('image'), record = step(image, 'retain the native build record');
requireRule(record?.includes('BUILT_IMAGE_ID: ${{ steps.build.outputs.imageid }}'), 'compare the build output image ID');
requireRule(record.includes("<<'NVM_BUILD_METADATA_JSON'"), 'metadata needs a quoted heredoc');
requireRule(record.includes('${{ toJSON(fromJSON(steps.build.outputs.metadata)) }}'), 'normalize metadata JSON before script interpolation');
requireRule(!image.includes('BUILD_METADATA:'), 'metadata must not enter the process environment');
requireRule((image.match(/steps\.build\.outputs\.metadata/g) || []).length === 1, 'metadata has one file-based consumer');
writeRun(record, 'record.sh');
const container = step(image, 'container-level tests');
requireRule(container?.includes('export NVM_CURL_IMAGE'), 'container probes need an exported curl image');
requireRule(container.includes("' Dockerfile)"), 'read the curl image from Dockerfile');
requireRule(!/rust:1-bookworm@sha256:[a-f0-9]{64}/.test(container), 'do not duplicate the Dockerfile digest');
writeRun(container, 'container.sh');
const upload = step(image, 'upload the image for the release job');
for (const file of ['nvm-build-metadata.json', 'nvm-image.id']) requireRule(upload?.includes('            ' + file + '\n'), 'retain ' + file);
const dependabot = fs.readFileSync('.github/dependabot.yml', 'utf8');
const docker = dependabot.split('  - package-ecosystem: docker\n')[1]?.split(/^  - /m)[0];
requireRule(/      - dependency-name: node\n        update-types: \["version-update:semver-major"\]/.test(docker || ''), 'coordinate Node major upgrades across Docker and workflows');
NODE
then
    pass 'public scan, gate and static are wired unconditionally without private policy'
else
    fail 'public workflow contracts are invalid'
fi

if [ -f "${WORK}/locks.sh" ] && [ -f "${WORK}/record.sh" ]; then
    mkdir -p "${WORK}/bin" "${WORK}/lock-runner" "${WORK}/record"
    export RUNNER_TEMP="${WORK}/lock-runner" CI_CONTROL_WORK="${WORK}"
    cat >"${WORK}/bin/cargo" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >"${CI_CONTROL_WORK}/cargo.args"
cat "${CI_CONTROL_WORK}/cargo.json"
exit "${CARGO_CONTROL_RC:-0}"
STUB
    cat >"${WORK}/bin/sudo" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$@" >"${CI_CONTROL_WORK}/sudo.args"
exit "${SUDO_CONTROL_RC:-0}"
STUB
    cat >"${WORK}/bin/docker" <<'STUB'
#!/usr/bin/env bash
[ "$*" = "image inspect -f {{.Id}} nvm-ci:candidate" ] || exit 90
printf '%s\n' "${INSPECT_CONTROL_ID:-sha256:fixture}"
exit "${INSPECT_CONTROL_RC:-0}"
STUB
    chmod 0700 "${WORK}/bin/"*
    lock_control() { PATH="${WORK}/bin:${PATH}" /bin/bash -e -o pipefail "${WORK}/locks.sh"; }
    printf '%s\n' '{"reason":"compiler-artifact","target":{"name":"lock_process"},"profile":{"test":true},"executable":"/runner/target/lock process"}' >"${WORK}/cargo.json"
    check 'lock probes run after an unprivileged Cargo build' lock_control
    check 'Cargo only compiles the selected test' grep -qxF 'test --locked --test lock_process --no-run --message-format=json' "${WORK}/cargo.args"
    printf '%s\n' -- '/runner/target/lock process' --ignored --test-threads=1 --nocapture >"${WORK}/sudo.expected"
    check 'sudo receives only the exact test executable and test flags' cmp -s "${WORK}/sudo.expected" "${WORK}/sudo.args"
    rc=0; CARGO_CONTROL_RC=23 lock_control >/dev/null 2>&1 || rc=$?
    check 'Cargo failures reject the job' test "${rc}" -eq 23
    rc=0; SUDO_CONTROL_RC=24 lock_control >/dev/null 2>&1 || rc=$?
    check 'root probe failures reject the job' test "${rc}" -eq 24
    cp "${WORK}/cargo.json" "${WORK}/cargo.good"
    for mode in missing multiple malformed; do
        case "${mode}" in
            missing) printf '%s\n' '{"reason":"build-finished","success":true}' >"${WORK}/cargo.json" ;;
            multiple) cat "${WORK}/cargo.good" >"${WORK}/cargo.json"; sed 's|/runner/target/lock process|/runner/target/other|' "${WORK}/cargo.good" >>"${WORK}/cargo.json" ;;
            malformed) printf '%s\n' '{' >"${WORK}/cargo.json" ;;
        esac
        rm -f "${WORK}/sudo.args"
        if lock_control >/dev/null 2>&1; then fail "${mode} Cargo output must fail"; else pass "${mode} Cargo output fails"; fi
        check "${mode} Cargo output never invokes sudo" test ! -e "${WORK}/sudo.args"
    done
    node - "${WORK}" <<'NODE'
const fs = require('node:fs'), work = process.argv[2];
const metadata = {provenance: 'x'.repeat(512 * 1024), shell: '$(touch injected)\nNVM_BUILD_METADATA_JSON\n`touch injected`\nquotes: \' " \\'};
fs.writeFileSync(work + '/expected.json', JSON.stringify(metadata));
const source = fs.readFileSync(work + '/record.sh', 'utf8');
fs.writeFileSync(work + '/record-rendered.sh', source.replace('${{ toJSON(fromJSON(steps.build.outputs.metadata)) }}', JSON.stringify(metadata, null, 2)));
NODE
    record_control() { (cd "${WORK}/record" && PATH="${WORK}/bin:${PATH}" BUILT_IMAGE_ID=sha256:fixture /bin/bash -e -o pipefail "${WORK}/record-rendered.sh"); }
    check 'large metadata is written through the script file' record_control
    check 'metadata JSON survives quoting and delimiter text unchanged' node -e 'const fs=require("fs"); if (JSON.stringify(JSON.parse(fs.readFileSync(process.argv[1]))) !== JSON.stringify(JSON.parse(fs.readFileSync(process.argv[2])))) process.exit(1)' "${WORK}/expected.json" "${WORK}/record/nvm-build-metadata.json"
    check 'metadata cannot execute embedded shell text' test ! -e "${WORK}/record/injected"
    check 'the retained image ID matches the inspected build output' grep -qxF sha256:fixture "${WORK}/record/nvm-image.id"
    for mode in mismatch failure; do
        rm -f "${WORK}/record/nvm-image.id"
        rc=0
        if [ "${mode}" = mismatch ]; then INSPECT_CONTROL_ID=sha256:other record_control >/dev/null 2>&1 || rc=$?
        else INSPECT_CONTROL_RC=25 record_control >/dev/null 2>&1 || rc=$?; fi
        check "inspect ${mode} rejects the image" test "${rc}" -ne 0
        check "inspect ${mode} never retains an accepted image ID" test ! -e "${WORK}/record/nvm-image.id"
    done
fi

if [ -f "${WORK}/container.sh" ]; then
    mkdir -p "${WORK}/container/bin"
    cp Dockerfile "${WORK}/container/Dockerfile"
    printf '%s\n' sha256:fixture >"${WORK}/container/nvm-image.id"
    cat >"${WORK}/container/bin/docker" <<'STUB'
#!/bin/bash
[ "$#" -eq 2 ] && [ "$1" = pull ] || exit 90
printf '%s\n' "pull $2" >>"${CI_CONTROL_WORK}/container.calls"
exit "${PULL_CONTROL_RC:-0}"
STUB
    cat >"${WORK}/container/bin/bash" <<'STUB'
#!/bin/bash
[ "$#" -eq 1 ] && [ "$1" = ci/container/test.sh ] || exit 90
printf '%s\n' "test ${NVM_CURL_IMAGE:-missing} ${NVM_IMAGE:-missing}" >>"${CI_CONTROL_WORK}/container.calls"
exit "${CONTAINER_CONTROL_RC:-0}"
STUB
    chmod 0700 "${WORK}/container/bin/"*
    container_control() {
        (cd "${WORK}/container" && PATH="${WORK}/container/bin:${PATH}" CI_CONTROL_WORK="${WORK}" /bin/bash -e -o pipefail "${WORK}/container.sh")
    }
    check 'the current Dockerfile supplies a valid curl image' container_control
    rm -f "${WORK}/container.calls"
    # Use a different valid digest to prove the workflow reads the file, rather
    # than obtaining the current production reference from another source.
    curl_fixture="rust:1-bookworm@sha256:$(printf '%064d' 0)"
    printf 'FROM %s AS rust\n' "${curl_fixture}" >"${WORK}/container/Dockerfile"
    check 'container probes accept the Dockerfile curl image' container_control
    printf '%s\n' "pull ${curl_fixture}" "test ${curl_fixture} sha256:fixture" >"${WORK}/container.expected"
    check 'pull precedes the test with both image references exported' cmp -s "${WORK}/container.expected" "${WORK}/container.calls"
    rc=0; CONTAINER_CONTROL_RC=27 container_control >/dev/null 2>&1 || rc=$?
    check 'container test failures reject the job' test "${rc}" -eq 27
    rm -f "${WORK}/container.calls"
    rc=0; PULL_CONTROL_RC=26 container_control >/dev/null 2>&1 || rc=$?
    check 'curl image pull failures reject the job' test "${rc}" -eq 26
    printf '%s\n' "pull ${curl_fixture}" >"${WORK}/container.expected"
    check 'failed pulls never start the container tests' cmp -s "${WORK}/container.expected" "${WORK}/container.calls"
    for mode in missing duplicate unpinned malformed wrong-image; do
        case "${mode}" in
            missing) printf '%s\n' 'FROM scratch' >"${WORK}/container/Dockerfile" ;;
            duplicate) printf 'FROM %s AS rust\nFROM %s AS rust\n' "${curl_fixture}" "${curl_fixture}" >"${WORK}/container/Dockerfile" ;;
            unpinned) printf '%s\n' 'FROM rust:1-bookworm AS rust' >"${WORK}/container/Dockerfile" ;;
            malformed) printf '%s\n' 'FROM rust:1-bookworm@sha256:abc AS rust' >"${WORK}/container/Dockerfile" ;;
            wrong-image) printf 'FROM %s AS rust\n' "${curl_fixture/rust:/node:}" >"${WORK}/container/Dockerfile" ;;
        esac
        rm -f "${WORK}/container.calls"
        if container_control >/dev/null 2>&1; then fail "${mode} Dockerfile must fail"; else pass "${mode} Dockerfile fails"; fi
        check "${mode} Dockerfile never pulls or runs tests" test ! -e "${WORK}/container.calls"
    done
fi

if [ -f "${WORK}/scan.sh" ]; then
    git ls-tree -r --name-only HEAD >"${WORK}/tracked"
    git show HEAD:README.md >"${WORK}/readme"
    cat >"${WORK}/inspect.js" <<'NODE'
const fs = require('node:fs'), path = require('node:path');
const args = process.argv.slice(2);
if (args.length !== 4 || args[0] !== 'ci/public-leak-check.sh' || args[1] !== '--root' || args[3] !== '--generic-only') process.exit(2);
const root = args[2], names = [];
function walk(dir, prefix = '') {
    for (const name of fs.readdirSync(dir)) {
        const rel = prefix + name, full = path.join(dir, name);
        if (fs.lstatSync(full).isDirectory()) walk(full, rel + '/');
        else names.push(rel);
    }
}
walk(root);
const expected = fs.readFileSync(process.env.SCAN_CONTROL_WORK + '/tracked', 'utf8').trim().split('\n');
if (JSON.stringify(names.sort()) !== JSON.stringify(expected.sort()) || fs.existsSync(root + '/.git')) process.exit(3);
if (!fs.readFileSync(root + '/README.md').equals(fs.readFileSync(process.env.SCAN_CONTROL_WORK + '/readme'))) process.exit(4);
process.exit(Number(process.env.SCAN_CONTROL_RC || 0));
NODE
    export SCAN_CONTROL_WORK="${WORK}" RUNNER_TEMP="${WORK}/runner"
    mkdir "${RUNNER_TEMP}"
    bash() { node "${SCAN_CONTROL_WORK}/inspect.js" "$@"; }
    export -f bash
    check 'scan body selects exactly HEAD files and committed bytes' /bin/bash "${WORK}/scan.sh"
    rc=0
    SCAN_CONTROL_RC=19 /bin/bash "${WORK}/scan.sh" || rc=$?
    check 'scanner failure rejects the job' test "${rc}" -eq 19
    check 'temporary snapshots are removed after success and failure' test -z "$(ls -A "${RUNNER_TEMP}")"
fi
finish_tests 'public CI controls failed' "public CI controls: ${PASS} passed"
