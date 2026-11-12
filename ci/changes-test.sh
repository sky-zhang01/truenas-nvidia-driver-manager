#!/usr/bin/env bash
# Exercise the shared routing table without changing Git refs or contacting remotes.
# Runs in the source tree and in the exported public tree (no private table there).
set -Eeuo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
PASS=0
PRIVATE=0
[ -f "${ROOT}/ci/checks-private.tsv" ] && PRIVATE=1

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { PASS=$((PASS + 1)); }
probe() { # probe DESCRIPTION CONDITION-COMMAND...
    local what=$1
    shift
    "$@" || fail "${what}"
    ok
}
eq() { [ "$1" = "$2" ] || fail "$3: expected '$2', got '$1'"; ok; }
norm() { tr ' ' '\n' <<<"$1" | grep -v '^$' | grep -vx guards | LC_ALL=C sort | tr '\n' ' ' | sed 's/ $//'; }

# Forced runs select every job except osv, which only its own inputs select.
FORCED='build_kit compose controls e2e image leak rust web'

clean_env() {
    unset GITHUB_OUTPUT CHANGES_SCOPE CHANGES_FORCE CHANGES_BASE CHANGES_HEAD TEST_MODE TEST_DIFF TEST_PATHS || true
    export GITHUB_EVENT_NAME=pull_request
}
clean_env

# A fake git supplies the diff and file list; it writes no index or refs.
FAKEBIN="${WORK}/bin"
mkdir -p "${FAKEBIN}"
cat >"${FAKEBIN}/git" <<'SH'
#!/bin/sh
case "$1" in
merge-base) [ "$TEST_MODE" != bad-base ] || exit 1; printf 'base\n' ;;
diff) [ "$TEST_MODE" != bad-diff ] || exit 1; [ "$TEST_MODE" != empty ] || exit 0; printf "$TEST_DIFF" ;;
ls-files) cat "$TEST_PATHS" ;;
*) exit 2 ;;
esac
SH
chmod +x "${FAKEBIN}/git"

# route SCRIPT DIFF... : switches for a fake diff of ":MODE MODE a b STATUS\0path\0" entries.
diff_spec() { # diff_spec STATUS PATH [OLDMODE NEWMODE]
    printf ':%s %s a b %s\\000%s\\000' "${3:-100644}" "${4:-100644}" "$1" "$2"
}
run_router() { # run_router SCRIPT [args...] with TEST_* in env
    local script=$1
    shift
    PATH="${FAKEBIN}:${PATH}" "${BASH}" "${script}" "$@"
}
sw() { grep "^$1=" <<<"$2" | cut -d= -f2; }
route() { # route DIFF -> switches on stdout
    TEST_DIFF=$1 run_router "${ROOT}/ci/changes.sh" base
}

# Routing case table: one changed path, jobs it selects (guards always on).
case_route() { # case_route PATH 'jobs' [STATUS]
    local got
    got=$("${BASH}" "${ROOT}/ci/changes.sh" --classify "$1" "${3:-M}")
    eq "$(norm "${got}")" "$(norm "$2")" "route ${3:-M} $1"
    echo "PASS: route ${3:-M} $1"
}
case_route docs/operations.en.md 'leak'
case_route README.md 'compose leak'
case_route README.zh-CN.md 'compose leak'
case_route docs/agent.en.md 'web leak'
case_route web/test/render.test.ts 'web leak'
case_route web/e2e/auth.spec.ts 'e2e leak'
case_route web/playwright.config.ts 'e2e leak'
case_route web/src/main.ts 'web e2e image leak'
case_route web/brand/mark.svg 'web e2e image leak'
case_route web/index.html 'web e2e image leak'
case_route web/tsconfig.json 'controls web e2e image leak'
case_route web/package.json 'web e2e image leak'
case_route web/package-lock.json 'web e2e image osv leak'
case_route web/.gitignore 'leak'
case_route Cargo.toml 'rust e2e image leak'
case_route Cargo.lock 'rust e2e image osv leak'
case_route src/auth.rs 'rust web e2e image leak'
case_route src/build_kit/packages.tsv 'rust web e2e image build_kit leak'
case_route tests/auth.rs 'rust leak'
case_route tests/common/mod.rs 'rust e2e leak'
case_route examples/fake_host_server.rs 'rust e2e leak'
case_route tools/gen-gpu-table.rs 'rust leak'
case_route docker/build-driver.sh 'rust image build_kit leak'
case_route third_party/gpu-unlock/kernel/gpu_unlock/main.c 'rust image leak'
case_route third_party/gpu-unlock/UPSTREAM.tsv 'rust image leak'
case_route tools/build-kit.sh 'controls build_kit leak'
case_route tools/third-party-licenses.py 'controls image leak'
case_route tools/third-party-licenses-test.py 'controls leak'
case_route Dockerfile 'controls image leak'
case_route .dockerignore 'image leak'
case_route LICENSE 'image leak'
case_route THIRD_PARTY_NOTICES.md 'image leak'
case_route deploy/compose.yaml 'compose image leak'
case_route osv-scanner.toml 'osv leak'
case_route ci/osv-scan.sh 'controls osv leak'
case_route ci/min-rust.sh 'rust leak'
case_route ci/e2e-state.sh 'e2e leak'
case_route ci/container/test.sh 'image leak'
case_route ci/build-script/test.sh 'build_kit leak'
case_route ci/image-assert.py 'controls image leak'
case_route ci/gpu-unlock-upstream.sh 'leak'
case_route ci/github-ci-test.sh "controls leak"
case_route ci/gate-test.sh 'controls leak'
if [ "${PRIVATE}" = 1 ]; then
    # Paths that exist only in the internal repository route through the private table.
    case_route renovate.json ''
    case_route AGEN''TS.md ''
    case_route docs/release.en.md ''
    case_route tools/maintenance/local-gc.py ''
    case_route tools/release/release.py 'controls'
    case_route ci/release.sh 'controls'
    case_route ci/nas-e2e/run.py 'controls'
    case_route export/rules 'controls leak'
    case_route ci/post-merge.sh 'controls leak'
    # The leak and export tooling selects its own self-tests.
    for path in ci/export-public.sh ci/export-public-test.sh ci/public-leak-check.sh ci/public-export-drift-test.sh \
        ci/install-gitleaks.sh ci/install-gitleaks-test.sh ci/public-leak-ci-test.sh ci/public-leak-local.sh ci/public-leak-local-test.sh ci/github-ci-test.sh; do
        out=$(route "$(diff_spec M "${path}")")
        eq "$(sw control_leak_tools "${out}")" true "leak tooling self-tests for ${path}"
    done
    for path in ci/public-leak-allowlist.json src/auth.rs web/src/main.ts docs/operations.en.md ci/osv-scan.sh Cargo.lock ci/changes-test.sh; do
        out=$(route "$(diff_spec M "${path}")")
        eq "$(sw control_leak_tools "${out}")" false "no leak tooling self-tests for ${path}"
    done
fi

# Files that define what every job means force every job, whatever the head table says.
for path in ci/checks.tsv ci/checks-private.tsv ci/changes.sh ci/gate.sh ci/job/x.sh ci/job/web.sh ci/job/leak.sh ci/job/gpu-unlock.sh \
    .github/workflows/ci.yml .gitea/workflows/checks.yml .gitea/workflows/new/nested.yml unregistered/file; do
    case_route "${path}" "${FORCED}"
done
# Statuses other than M enable the controls and the change-routing control; leak never skips.
for status in A D R T; do
    case_route docs/operations.en.md 'controls leak' "${status}"
done
out=$(route "$(diff_spec A docs/operations.en.md)")
eq "$(sw control_changes "${out}")" true 'status A enables control_changes'
out=$(route "$(diff_spec M docs/operations.en.md)")
eq "$(sw control_changes "${out}")" false 'status M leaves control_changes off'
# Mode changes and symlinks are type changes.
out=$(route "$(diff_spec M docs/operations.en.md 100644 100755)")
eq "$(sw leak "${out}")$(sw control_changes "${out}")" truetrue 'mode change is a type change'
out=$(route "$(diff_spec M docs/operations.en.md 120000 120000)")
eq "$(sw leak "${out}")$(sw control_changes "${out}")" truetrue 'symlink edit is a type change'
# A path may contain a newline; later paths must still be read (fail closed).
out=$(route "$(diff_spec M "$(printf 'README.md\nzz')")$(diff_spec M web/src/main.ts)")
eq "$(sw web "${out}")$(sw rust "${out}")$(sw e2e "${out}")" truetruetrue 'a newline in a path neither hides later paths nor narrows the run'
if [ "${PRIVATE}" = 1 ]; then
    for status in A D R T; do
        case_route tools/maintenance/local-gc.py 'controls leak' "${status}"
    done
    out=$(route "$(diff_spec M export/rules)")
    eq "$(sw control_release "${out}")" true 'export/rules selects the release control'
    out=$(route "$(diff_spec M ci/release-readback.js)$(diff_spec M ci/test-lib.sh)")
    eq "$(sw control_post_merge "${out}")" true 'post-merge control reads its inputs'
    out=$(route "$(diff_spec M renovate.json)")
    eq "$(sw leak "${out}")" false 'a modify-only edit of a leak-skip path leaves leak off'
    out=$(route "$(diff_spec M renovate.json)$(diff_spec M docs/operations.en.md)")
    eq "$(sw leak "${out}")" true 'one non-exempt path in the set selects leak'
fi
echo 'PASS: statuses and leak-skip'

# Several paths union their inputs.
out=$(route "$(diff_spec M ci/gate-test.sh)$(diff_spec M web/test/render.test.ts)")
eq "$(sw controls "${out}")$(sw web "${out}")$(sw leak "${out}")" truetruetrue 'second path unions inputs'
out=$(route "$(diff_spec M ci/test-lib.sh)")
eq "$(sw control_github "${out}")" true 'test-lib selects the github control'
for path in Dockerfile web/tsconfig.json; do
    out=$(route "$(diff_spec M "${path}")")
    eq "$(sw control_changes "${out}")" true "${path} selects the packaging-boundary control"
done

# osv follows time, not edits: forced runs on a pull request select it only for its own inputs.
out=$(route "$(diff_spec M ci/gate.sh)")
eq "$(sw osv "${out}")$(sw locks "${out}")$(sw rust "${out}")" falsefalsetrue 'CI-definition change leaves osv off'
out=$(route "$(diff_spec M unregistered/file)")
eq "$(sw osv "${out}")$(sw locks "${out}")" falsefalse 'unregistered path leaves osv off'
out=$(route "$(diff_spec M ci/gate.sh)$(diff_spec M Cargo.lock)")
eq "$(sw osv "${out}")$(sw locks "${out}")$(sw rust "${out}")" truetruetrue 'forced run keeps osv when its inputs changed'
out=$(route "$(diff_spec M osv-scanner.toml)")
eq "$(sw osv "${out}")$(sw locks "${out}")$(sw rust "${out}")" truetruefalse 'osv input selects osv alone'
out=$(GITHUB_EVENT_NAME=push route "$(diff_spec M ci/gate.sh)")
eq "$(sw osv "${out}")$(sw locks "${out}")" truetrue 'non-pull-request events select osv'
echo 'PASS: osv selection'

# Command-boundary failures and non-pull-request events select everything.
for mode in bad-base bad-diff empty; do
    out=$(TEST_MODE=${mode} route "$(diff_spec M docs/operations.en.md)" 2>/dev/null)
    eq "$(sw rust "${out}")$(sw osv "${out}")$(sw image "${out}")" truetruetrue "${mode} selects everything"
done
out=$(route "$(diff_spec M docs/operations.en.md)")
eq "$(sw rust "${out}")$(sw leak "${out}")$(sw guards "${out}")" falsetruetrue 'normal diff stays narrow'
out=$(route ':100644 100644 a b\000docs/operations.en.md\000' 2>/dev/null)
eq "$(sw rust "${out}")" true 'unreadable metadata selects everything'
out=$(route ':100644 100644 a b M\000docs/operations.en.md\000orphan' 2>/dev/null)
eq "$(sw rust "${out}")" true 'a dangling diff entry selects everything'
out=$(GITHUB_EVENT_NAME=push route "$(diff_spec M docs/operations.en.md)")
eq "$(sw rust "${out}")$(sw osv "${out}")" truetrue 'push selects everything'
echo 'PASS: fail-closed selection'

# CHANGES_SCOPE selects one job (and guards) or everything.
for scope in all osv web build-script public-leak leak controls; do
    out=$(CHANGES_SCOPE=${scope} "${BASH}" "${ROOT}/ci/changes.sh")
    eq "$(sw guards "${out}")" true "scope ${scope} keeps guards"
    eq "$(sw scope "${out}")" "${scope}" "scope ${scope} is echoed"
    want=$scope
    case "${scope}" in all) want=rust ;; build-script) want=build_kit ;; public-leak) want=leak ;; esac
    eq "$(sw "${want}" "${out}")" true "scope ${scope} selects ${want}"
done
out=$(CHANGES_SCOPE=web "${BASH}" "${ROOT}/ci/changes.sh")
eq "$(sw rust "${out}")$(sw osv "${out}")$(sw locks "${out}")" falsefalsefalse 'a one-job scope selects nothing else'
out=$(CHANGES_SCOPE=osv "${BASH}" "${ROOT}/ci/changes.sh")
eq "$(sw locks "${out}")$(sw web "${out}")" truefalse 'scope osv selects osv and locks'
out=$(CHANGES_SCOPE=all "${BASH}" "${ROOT}/ci/changes.sh")
eq "$(sw osv "${out}")$(sw locks "${out}")" truetrue 'scope all includes osv'
out=$(CHANGES_SCOPE=controls "${BASH}" "${ROOT}/ci/changes.sh")
eq "$(sw control_gate "${out}")$(sw control_changes "${out}")$(sw web "${out}")" truetruefalse 'scope controls selects every control'
if CHANGES_SCOPE=bogus "${BASH}" "${ROOT}/ci/changes.sh" >/dev/null 2>&1; then fail 'unknown scope must fail'; fi
ok
out=$(GITHUB_OUTPUT="${WORK}/outputs" CHANGES_SCOPE=web "${BASH}" "${ROOT}/ci/changes.sh")
eq "${out}" '' 'switches go to GITHUB_OUTPUT when set'
grep -qx 'web=true' "${WORK}/outputs" || fail 'GITHUB_OUTPUT carries web=true'
ok
echo 'PASS: CHANGES_SCOPE'

# --classify usage, --validate on the real tree.
if "${BASH}" "${ROOT}/ci/changes.sh" --classify >/dev/null 2>&1; then fail '--classify needs a path'; fi
ok
if "${BASH}" "${ROOT}/ci/changes.sh" --bogus >/dev/null 2>&1; then fail 'unknown option must fail'; fi
ok
(cd "${ROOT}" && "${BASH}" ci/changes.sh --validate) || fail 'every repository path registered and every glob matches'
ok
echo 'PASS: every repository path registered and every glob matches'

# Fixture trees: a copy of the router with its own tables.
new_fixture() { # new_fixture NAME -> directory with ci/changes.sh
    local dir="${WORK}/$1"
    mkdir -p "${dir}/ci/job"
    cp "${ROOT}/ci/changes.sh" "${dir}/ci/changes.sh"
    cp "${ROOT}/ci/job/controls.sh" "${dir}/ci/job/controls.sh"
    echo "${dir}"
}

# A table without its rows cannot judge its own edit narrowly: the fixed list decides.
fx=$(new_fixture weak)
printf 'rust\tsrc/**\t-\tOnly rust.\tall\n' >"${fx}/ci/checks.tsv"
for path in ci/checks.tsv ci/changes.sh ci/gate.sh ci/job/rust.sh .github/workflows/ci.yml; do
    out=$(TEST_DIFF=$(diff_spec M "${path}") run_router "${fx}/ci/changes.sh" base)
    eq "$(sw web "${out}")$(sw leak "${out}")$(sw e2e "${out}")" truetruetrue "fixed list forces ${path} with a weakened table"
done
out=$(TEST_DIFF=$(diff_spec M src/a.rs) run_router "${fx}/ci/changes.sh" base)
eq "$(sw web "${out}")$(sw rust "${out}")" falsetrue 'a registered path stays narrow'
# An 'all' row in a table forces every job as well.
printf 'all\tspecial/**\t-\tForce.\tall\n' >>"${fx}/ci/checks.tsv"
out=$(TEST_DIFF=$(diff_spec M special/x) run_router "${fx}/ci/changes.sh" base)
eq "$(sw web "${out}")$(sw osv "${out}")" truefalse 'an all row forces every job except osv'
echo 'PASS: fail-closed CI-definition list and all rows'

# Context: private rows load only with export/rules; a bad row is refused.
fx=$(new_fixture context)
cat >"${fx}/ci/checks.tsv" <<'TSV'
rust	src/**	-	Public row.	all
TSV
cat >"${fx}/ci/checks-private.tsv" <<'TSV'
web	secret/**	-	Private row.	private
TSV
out=$(TEST_DIFF=$(diff_spec M secret/x) run_router "${fx}/ci/changes.sh" base 2>/dev/null)
eq "$(sw rust "${out}")" true 'private row ignored without export/rules (path unregistered, selects all)'
echo marker >"${fx}/export-marker"
mkdir -p "${fx}/export" && echo rules >"${fx}/export/rules"
out=$(TEST_DIFF=$(diff_spec M secret/x) run_router "${fx}/ci/changes.sh" base)
eq "$(sw web "${out}")$(sw rust "${out}")" truefalse 'private row read with export/rules'
printf 'rust\tsrc/**\t-\tonly four columns\n' >"${fx}/ci/checks.tsv"
if TEST_DIFF=$(diff_spec M src/a) run_router "${fx}/ci/changes.sh" base >/dev/null 2>&1; then fail 'a malformed row must fail'; fi
ok
printf 'nonsense\tsrc/**\t-\tUnknown check.\tall\n' >"${fx}/ci/checks.tsv"
if TEST_DIFF=$(diff_spec M src/a) run_router "${fx}/ci/changes.sh" base >/dev/null 2>&1; then fail 'an unknown check must fail'; fi
ok
echo 'PASS: private and public context'

# --validate rejects an unregistered path and a glob matching nothing.
fx=$(new_fixture validate)
touch "${fx}/a" "${fx}/b"
printf 'rust\ta\t-\tOne.\tall\n' >"${fx}/ci/checks.tsv"
printf 'a\0b\0' >"${WORK}/paths-two"
(cd "${fx}" && TEST_PATHS="${WORK}/paths-two" PATH="${FAKEBIN}:${PATH}" "${BASH}" ci/changes.sh --validate 2>"${WORK}/err") && fail 'unregistered path must fail validation'
grep -q 'unregistered path: b' "${WORK}/err" || fail 'validation names the unregistered path'
ok
printf 'rust\ta;ghost*\t-\tOne.\tall\n' >"${fx}/ci/checks.tsv"
printf 'a\0' >"${WORK}/paths-one"
(cd "${fx}" && TEST_PATHS="${WORK}/paths-one" PATH="${FAKEBIN}:${PATH}" "${BASH}" ci/changes.sh --validate 2>"${WORK}/err") && fail 'dead glob must fail validation'
grep -q 'glob matches no repository path: ghost\*' "${WORK}/err" || fail 'validation names the dead glob'
ok
echo 'PASS: validation fails on unregistered paths and dead globs'

# Content identities include only the selected inputs, their names and modes.
fx=$(new_fixture hash)
printf 'image\truntime*;helper\t-\tFixture image inputs.\tall\n' >"${fx}/ci/checks.tsv"
printf 'runtime bytes' >"${fx}/runtime"
printf 'test helper' >"${fx}/helper"
printf 'documentation' >"${fx}/unrelated"
printf 'runtime\0helper\0unrelated\0' >"${fx}/paths"
identity() { (cd "${fx}" && TEST_PATHS="${fx}/paths" PATH="${FAKEBIN}:${PATH}" "${BASH}" ci/changes.sh --hash image); }
original=$(identity)
eq "${#original}" 64 'identity is a sha256'
printf 'changed documentation' >"${fx}/unrelated"
eq "$(identity)" "${original}" 'unrelated files do not change the identity'
printf 'changed helper' >"${fx}/helper"
changed=$(identity)
probe 'input content changes the identity' [ "${changed}" != "${original}" ]
chmod 755 "${fx}/helper"
mode_hash=$(identity)
probe 'the executable bit changes the identity' [ "${mode_hash}" != "${changed}" ]
mv "${fx}/runtime" "${fx}/runtime-renamed"
printf 'runtime-renamed\0helper\0unrelated\0' >"${fx}/paths"
renamed=$(identity)
probe 'a rename changes the identity' [ "${renamed}" != "${mode_hash}" ]
printf 'image\thelper\t-\tNarrowed fixture image inputs.\tall\n' >"${fx}/ci/checks.tsv"
probe 'membership changes the identity' [ "$(identity)" != "${renamed}" ]
ln -s helper "${fx}/link"
printf 'helper\0link\0' >"${fx}/paths"
printf 'image\thelper;link\t-\tFixture with a link.\tall\n' >"${fx}/ci/checks.tsv"
linked=$(identity)
ln -sf unrelated "${fx}/link"
probe 'a symlink target changes the identity' [ "$(identity)" != "${linked}" ]
# A trailing newline in the target is part of the target.
rm -f "${fx}/link"
ln -s unrelated "${fx}/link"
plain=$(identity)
rm -f "${fx}/link"; ln -s "unrelated
" "${fx}/link"
probe 'a trailing newline in a symlink target changes the identity' [ "$(identity)" != "${plain}" ]
# A link that cannot be read must fail the hash, not hash an empty target.
mkdir -p "${WORK}/badlink"
printf '#!/bin/sh\nexit 42\n' >"${WORK}/badlink/readlink"
chmod +x "${WORK}/badlink/readlink"
if (cd "${fx}" && TEST_PATHS="${fx}/paths" PATH="${WORK}/badlink:${FAKEBIN}:${PATH}" "${BASH}" ci/changes.sh --hash image >/dev/null 2>&1); then
    fail 'an unreadable link must fail the hash'
fi
ok
if (cd "${fx}" && TEST_PATHS="${fx}/paths" PATH="${FAKEBIN}:${PATH}" "${BASH}" ci/changes.sh --hash osv >/dev/null 2>&1); then fail 'a job without inputs must fail'; fi
ok
echo 'PASS: image hash covers content, modes, names and selected membership only'

# Controls: only the selected commands run, failures propagate, removed rows do not run.
fx=$(new_fixture controls)
cat >"${fx}/ci/checks.tsv" <<'TSV'
controls:alpha	alpha	bash ci/record.sh alpha	Selected public control.	all
controls:beta	beta	bash ci/record.sh beta	Unselected public control.	all
controls:private	alpha	bash ci/record.sh private	Private-only control.	private
TSV
cat >"${fx}/ci/record.sh" <<'SH'
#!/bin/sh
printf '%s\n' "$1" >>"$CONTROL_LOG"
[ "$1" != alpha ] || exit "${CONTROL_FAIL:-0}"
SH
LOG="${fx}/executed"
run_controls() { (cd "${fx}" && CONTROL_LOG="${LOG}" CHANGES_BASE=base TEST_DIFF="$(diff_spec M alpha)" PATH="${FAKEBIN}:${PATH}" "$@" "${BASH}" ci/job/controls.sh); }
: >"${LOG}"
run_controls env >"${WORK}/out" 2>&1 || fail 'selected control must pass'
eq "$(tr '\n' ' ' <"${LOG}")" 'alpha ' 'the actual controls entry runs only the selected public command'
echo 'PASS: actual controls entry executes only the selected public command'
mkdir -p "${fx}/export" && echo rules >"${fx}/export/rules"
: >"${LOG}"
run_controls env >"${WORK}/out" 2>&1 || fail 'private control must pass'
eq "$(tr '\n' ' ' <"${LOG}")" 'alpha private ' 'private commands run only with the private context'
echo 'PASS: private commands execute only when the private context is present'
: >"${LOG}"
if run_controls env CONTROL_FAIL=17 >"${WORK}/out" 2>"${WORK}/err"; then fail 'a failing control must fail the run'; fi
grep -q 17 "${WORK}/err" || fail 'the failure names the exit status'
eq "$(tr '\n' ' ' <"${LOG}")" 'alpha ' 'a failed control stops later selected commands'
echo 'PASS: control failure propagates and stops later selected commands'
# A scope runs a single control family: controls selects them all.
: >"${LOG}"
(cd "${fx}" && CONTROL_LOG="${LOG}" CHANGES_SCOPE=controls "${BASH}" ci/changes.sh --controls >/dev/null 2>&1) || fail 'scope controls must pass'
eq "$(LC_ALL=C sort "${LOG}" | tr '\n' ' ')" 'alpha beta private ' 'scope controls runs every registered control'
# A row removed from the table is not run; a registered control whose file is gone fails.
printf 'controls:beta\tbeta\tbash ci/record.sh beta\tOnly beta.\tall\n\talpha\t-\tNo control.\tall\n' >"${fx}/ci/checks.tsv"
: >"${LOG}"
run_controls env >"${WORK}/out" 2>&1 || true
eq "$(cat "${LOG}")" '' 'a removed control row is not run'
printf 'controls:alpha\talpha\tbash ci/missing.sh\tFile deleted.\tall\n' >"${fx}/ci/checks.tsv"
if run_controls env >"${WORK}/out" 2>&1; then fail 'a registered control whose file is deleted must fail'; fi
ok
printf 'controls:alpha\talpha\tbash ci/a.sh\tA.\tall\ncontrols:alpha\talpha\tbash ci/b.sh\tB.\tall\n' >"${fx}/ci/checks.tsv"
if run_controls env >"${WORK}/out" 2>&1; then fail 'conflicting commands must fail'; fi
ok
echo 'PASS: removed, missing and conflicting controls'

# The packaging boundaries that justify web test-only routing.
node -e "
const t = JSON.parse(require('fs').readFileSync('${ROOT}/web/tsconfig.json', 'utf8'));
if (JSON.stringify(t.include) !== JSON.stringify(['src/**/*.ts'])) process.exit(1);
" || fail 'web/tsconfig.json include'
ok
# Instructions may be indented and use any case; compare normalized words.
dockerfile_copies() { # dockerfile_copies FILE: web COPY instructions of the last stage
    local last
    last=$(grep -n -i -E '^[[:space:]]*FROM[[:space:]]' "$1" | tail -1 | cut -d: -f1)
    tail -n +"$((last + 1))" "$1" | sed -e ':a' -e '/\\$/N; s/\\\n//; ta' \
        | sed -E 's/^[[:space:]]+//; s/[[:space:]]+/ /g; s/ $//' | grep -i '^COPY ' | grep web || true
}
copies=$(dockerfile_copies "${ROOT}/Dockerfile")
eq "${copies}" 'COPY --from=web /build/web/dist/ /usr/local/share/truenas-nvidia-driver-manager/web/' 'the runtime stage copies only built web files'
{ cat "${ROOT}/Dockerfile"; printf ' copy web/test/ /test-fixtures/\n'; } >"${WORK}/Dockerfile.indented"
copies=$(dockerfile_copies "${WORK}/Dockerfile.indented")
[ "${copies}" != 'COPY --from=web /build/web/dist/ /usr/local/share/truenas-nvidia-driver-manager/web/' ] || fail 'an indented lower-case COPY of web files must be caught'
ok

echo "changes-test: ${PASS} assertions passed"
