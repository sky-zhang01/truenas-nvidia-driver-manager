#!/usr/bin/env bash
# changes-test.sh — controls for ci/changes.sh.
#
# Covers every rule of the path table, renames (both sides count), the
# full-run fallbacks (no base, zero base, failed diff, empty diff, non-PR
# event, unregistered path), the $GITHUB_OUTPUT contract, and — against this
# repository — that every tracked file hits a registered rule.
set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHANGES="${SCRIPT_DIR}/changes.sh"
CHANGES_TEST_GIT="$(command -v git)"
export CHANGES_TEST_GIT
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

# Keep the real diff implementation while substituting tree snapshots for
# fixture-only revision names. No test creates commits or changes any refs.
mkdir -p "${WORK}/bin"
cat >"${WORK}/bin/git" <<'GITWRAPPER'
#!/usr/bin/env bash
set -Eeuo pipefail
if [ "${1:-}" = diff ]; then
    fixture_git_dir="$("${CHANGES_TEST_GIT}" rev-parse --git-dir)"
    args=()
    for arg in "$@"; do
        case "${arg}" in
            HEAD) arg="$(cat "${fixture_git_dir}/fixture-current-tree")" ;;
            HEAD~1) arg="$(cat "${fixture_git_dir}/fixture-previous-tree")" ;;
        esac
        args+=("${arg}")
    done
    exec "${CHANGES_TEST_GIT}" "${args[@]}"
fi
exec "${CHANGES_TEST_GIT}" "$@"
GITWRAPPER
chmod +x "${WORK}/bin/git"
export PATH="${WORK}/bin:${PATH}"

FAILURES=0
PASS=0
pass() { PASS=$((PASS + 1)); echo "PASS: $1"; }
fail() { echo "FAIL: $1" >&2; FAILURES=$((FAILURES + 1)); }

# ── the path table ────────────────────────────────────────────────────────────
expect_class() { # expect_class <path> <switch...> ('-' for none)
    local path="$1" want got
    shift
    if [ "$1" = "-" ]; then want=""; else want="$*"; fi
    got="$(bash "${CHANGES}" --classify "${path}")"
    if [ "${got}" = "${want}" ]; then
        pass "classify ${path}"
    else
        fail "classify ${path}: want '${want}', got '${got}'"
    fi
}

expect_class .gitea/workflows/ci.yml rust e2e image build_kit
expect_class .github/workflows/ci.yml rust e2e image build_kit
expect_class .github/dependabot.yml rust e2e image build_kit
expect_class .github/scripts/ghcr_release.py rust e2e image build_kit
expect_class .github/scripts/test_ghcr_release.py rust e2e image build_kit
expect_class ci/guards.sh rust e2e image build_kit
expect_class src/main.rs rust e2e image
expect_class src/runner/build_env.rs rust e2e image
expect_class src/build_kit/packages.tsv rust e2e image build_kit
expect_class tests/auth.rs rust e2e
expect_class examples/fake_host_server.rs rust e2e
expect_class tools/build-kit.sh rust e2e build_kit
expect_class tools/gen-gpu-table.rs rust e2e
expect_class docker/build-driver.sh rust e2e image build_kit
expect_class docker/install-build-kit.sh rust e2e image build_kit
expect_class Cargo.toml rust e2e image locks
expect_class Cargo.lock rust e2e image locks
expect_class osv-scanner.toml locks
expect_class web/test/view.test.ts -
expect_class web/src/main.ts e2e image
expect_class web/e2e/auth.spec.ts e2e
expect_class web/playwright.config.ts e2e
expect_class web/.gitignore e2e image
expect_class web/package.json e2e image locks
expect_class web/package-lock.json e2e image locks
expect_class Dockerfile image
expect_class .dockerignore image
expect_class LICENSE image
expect_class THIRD_PARTY_NOTICES.md image
expect_class tools/third-party-licenses.py image
expect_class tools/third-party-licenses-test.py image
expect_class deploy/compose.yaml image
expect_class README.md -
expect_class README.zh-CN.md -
expect_class CHANGELOG.md -
expect_class CHANGELOG.zh-CN.md -
expect_class CONTRIBUTING.md -
expect_class CONTRIBUTING.zh-CN.md -
expect_class SECURITY.md -
expect_class SECURITY.zh-CN.md -
expect_class docs/deploy.en.md -
expect_class skills/nvidia-driver-replacement/SKILL.md -
expect_class CLAUDE.md -
expect_class renovate.json -
expect_class .gitignore -
expect_class .gitleaks.toml -
expect_class export/rules -
expect_class vendor/anything.sh unknown

# ── scratch repos for the diff-based switches ────────────────────────────────
new_repo() {
    REPO="$(mktemp -d "${WORK}/repo-XXXXXX")"
    git -C "${REPO}" init -q
    git -C "${REPO}" read-tree --empty
    git -C "${REPO}" write-tree >"${REPO}/.git/fixture-current-tree"
}

run_switches() { # run_switches [args...] — echo raw key=value lines
    (
        cd "${REPO}"
        env -u GITHUB_OUTPUT -u GITHUB_EVENT_NAME -u CHANGES_BASE \
            bash "${CHANGES}" "$@"
    )
}

got_rust=""
got_e2e=""
got_image=""
got_build_kit=""
got_locks=""
switches_of() { # switches_of [args...] — run and parse into got_* variables
    local out line
    out="$(run_switches "$@")"
    got_rust="" got_e2e="" got_image="" got_build_kit="" got_locks=""
    while IFS= read -r line; do
        case "${line}" in
            rust=*) got_rust="${line#rust=}" ;;
            e2e=*) got_e2e="${line#e2e=}" ;;
            image=*) got_image="${line#image=}" ;;
            build_kit=*) got_build_kit="${line#build_kit=}" ;;
            locks=*) got_locks="${line#locks=}" ;;
            *) ;;
        esac
    done <<EOF
${out}
EOF
}

expect_run() { # expect_run <description> <want rust,e2e,image,build_kit,locks>
    local desc="$1" want="$2"
    local want_rust="${want%%,*}"
    local rest="${want#*,}"
    local want_e2e="${rest%%,*}"
    rest="${rest#*,}"
    local want_image="${rest%%,*}"
    rest="${rest#*,}"
    local want_build_kit="${rest%%,*}"
    local want_locks="${rest##*,}"
    local got="rust=${got_rust:-missing} e2e=${got_e2e:-missing} image=${got_image:-missing} build_kit=${got_build_kit:-missing} locks=${got_locks:-missing}"
    local wanted="rust=${want_rust} e2e=${want_e2e} image=${want_image} build_kit=${want_build_kit} locks=${want_locks}"
    if [ "${got}" = "${wanted}" ]; then
        pass "${desc}"
    else
        fail "${desc}: want ${wanted}, got ${got}"
    fi
}

snapshot_repo() { # snapshot_repo — save index trees without creating commits
    cp "${REPO}/.git/fixture-current-tree" "${REPO}/.git/fixture-previous-tree"
    git -C "${REPO}" add -A
    git -C "${REPO}" write-tree >"${REPO}/.git/fixture-current-tree"
}

scratch_case() { # scratch_case <description> <files to create...> -- <want>
    local desc="$1"
    shift
    local -a creates=()
    while [ "$1" != "--" ]; do
        creates+=("$1")
        shift
    done
    shift
    local want="$1"
    new_repo
    local path dir
    for path in "${creates[@]}"; do
        dir="$(dirname "${path}")"
        [ -d "${REPO}/${dir}" ] || mkdir -p "${REPO}/${dir}"
        printf 'content\n' >"${REPO}/${path}"
    done
    snapshot_repo
    switches_of HEAD~1
    expect_run "${desc}" "${want}"
}

scratch_case 'a docs-only change runs nothing heavy' README.md docs/guide.md CHANGELOG.zh-CN.md CONTRIBUTING.zh-CN.md SECURITY.zh-CN.md -- 'false,false,false,false,false'
scratch_case 'a Rust source change runs rust, e2e and image' src/main.rs -- 'true,true,true,false,false'
scratch_case 'web unit tests run no heavy jobs' web/test/view.test.ts -- 'false,false,false,false,false'
scratch_case 'browser tests run e2e only' web/e2e/auth.spec.ts -- 'false,true,false,false,false'
scratch_case 'browser configuration runs e2e only' web/playwright.config.ts -- 'false,true,false,false,false'
scratch_case 'a web change runs e2e and image' web/src/main.ts -- 'false,true,true,false,false'
scratch_case 'a web lockfile change also reports locks' web/package-lock.json -- 'false,true,true,false,true'
scratch_case 'a build script change runs rust, e2e, image and build-script' docker/build-driver.sh -- 'true,true,true,true,false'
scratch_case 'a package-closure change runs rust, e2e, image and build-script' src/build_kit/packages.tsv -- 'true,true,true,true,false'
scratch_case 'a build-kit tool change runs rust, e2e and build-script' tools/build-kit.sh -- 'true,true,false,true,false'
scratch_case 'a generator change runs rust and e2e' tools/gen-gpu-table.rs -- 'true,true,false,false,false'
scratch_case 'an example change runs rust and e2e' examples/fake_host_server.rs -- 'true,true,false,false,false'
scratch_case 'a test change runs rust and e2e' tests/auth.rs -- 'true,true,false,false,false'
scratch_case 'a Cargo manifest change reports locks' Cargo.toml -- 'true,true,true,false,true'
scratch_case 'an image-input change runs image only' Dockerfile .dockerignore LICENSE THIRD_PARTY_NOTICES.md deploy/compose.yaml -- 'false,false,true,false,false'
scratch_case 'a license generator change runs image' tools/third-party-licenses.py -- 'false,false,true,false,false'
scratch_case 'a license generator test change runs image' tools/third-party-licenses-test.py -- 'false,false,true,false,false'
scratch_case 'a CI change runs everything' ci/guards.sh -- 'true,true,true,true,false'
scratch_case 'a workflow change runs everything' .gitea/workflows/ci.yml -- 'true,true,true,true,false'
scratch_case 'a GitHub workflow change runs everything' .github/workflows/ci.yml -- 'true,true,true,true,false'
scratch_case 'a GitHub configuration change runs everything' .github/dependabot.yml -- 'true,true,true,true,false'
scratch_case 'a release helper change runs everything' .github/scripts/ghcr_release.py -- 'true,true,true,true,false'
scratch_case 'a release helper test change runs everything' .github/scripts/test_ghcr_release.py -- 'true,true,true,true,false'
scratch_case 'an unregistered path runs everything' vendor/lib.sh -- 'true,true,true,true,true'

# ── renames: the old path keeps its switches ──────────────────────────────────
new_repo
mkdir -p "${REPO}/docker"
printf 'content\n' >"${REPO}/docker/build-driver.sh"
snapshot_repo
mkdir -p "${REPO}/docs"
git -C "${REPO}" mv docker/build-driver.sh docs/build-driver.sh
snapshot_repo
switches_of HEAD~1
expect_run 'a build script renamed into docs still runs rust, e2e, image and build-script' 'true,true,true,true,false'

new_repo
mkdir -p "${REPO}/src"
printf 'content\n' >"${REPO}/src/main.rs"
snapshot_repo
mkdir -p "${REPO}/docs"
git -C "${REPO}" mv src/main.rs docs/main.rs
snapshot_repo
switches_of HEAD~1
expect_run 'a source renamed into docs still runs rust, e2e and image' 'true,true,true,false,false'

# ── full-run fallbacks ────────────────────────────────────────────────────────
new_repo
printf 'content\n' >"${REPO}/README.md"
snapshot_repo

switches_of
expect_run 'no base revision is a full run' 'true,true,true,true,true'

switches_of 0000000000000000000000000000000000000000
expect_run 'an all-zero base is a full run' 'true,true,true,true,true'

switches_of 0102030405060708090a0b0c0d0e0f1011121314
expect_run 'a failed diff is a full run' 'true,true,true,true,true'

switches_of HEAD
expect_run 'an empty diff is a full run' 'true,true,true,true,true'

out="$(cd "${REPO}" && env -u GITHUB_OUTPUT -u CHANGES_BASE GITHUB_EVENT_NAME=push bash "${CHANGES}" HEAD~1)"
case "${out}" in
    *'rust=true'*) pass 'a non-pull-request event is a full run' ;;
    *) fail "a non-pull-request event must be a full run, got: ${out}" ;;
esac

out="$(cd "${REPO}" && env -u GITHUB_OUTPUT GITHUB_EVENT_NAME=pull_request bash "${CHANGES}")"
case "${out}" in
    *'rust=true'*) pass 'a pull request without a base is a full run' ;;
    *) fail "a pull request without a base must be a full run, got: ${out}" ;;
esac

# ── the $GITHUB_OUTPUT contract ───────────────────────────────────────────────
new_repo
mkdir -p "${REPO}/web/src"
printf 'content\n' >"${REPO}/README.md"
printf 'content\n' >"${REPO}/web/src/main.ts"
snapshot_repo
out_file="${WORK}/github-output"
(
    cd "${REPO}"
    env -u GITHUB_EVENT_NAME -u CHANGES_BASE GITHUB_OUTPUT="${out_file}" \
        bash "${CHANGES}" HEAD~1
) >/dev/null
want='rust=false
e2e=true
image=true
build_kit=false
locks=false'
got="$(cat "${out_file}")"
if [ "${got}" = "${want}" ]; then
    pass 'switches land in the GITHUB_OUTPUT file, one key=value per line'
else
    fail "GITHUB_OUTPUT content: want ${want}, got ${got}"
fi

# The routing exemptions depend on production compilation and packaging boundaries.
if python3 - "${REPO_ROOT}" <<'PYGUARD'
import json
import pathlib
import shlex
import sys

root = pathlib.Path(sys.argv[1])
include = json.loads((root / "web/tsconfig.json").read_text())["include"]
assert include == ["src/**/*.ts"], f"unexpected TypeScript inputs: {include!r}"
lines = (root / "Dockerfile").read_text().replace("\\\n", " ").splitlines()
instructions = [shlex.split(line, comments=True) for line in lines]
instructions = [parts for parts in instructions if parts]
last_from = max(i for i, parts in enumerate(instructions) if parts[0].upper() == "FROM")
web_copies = []
for parts in instructions[last_from + 1:]:
    if parts[0].upper() != "COPY":
        continue
    operands = [part for part in parts[1:] if not part.startswith("--")]
    if "--from=web" in parts or any("web" in part for part in operands):
        web_copies.append(parts)
assert web_copies == [["COPY", "--from=web", "/build/web/dist/",
                       "/usr/local/share/truenas-nvidia-driver-manager/web/"]], web_copies
PYGUARD
then
    pass 'web compilation uses only src and runtime web assets come only from dist'
else
    fail 'web routing exemptions require src-only compilation and dist-only runtime assets'
fi

# ── every tracked file of this repository hits a registered rule ─────────────
unknown=""
while IFS= read -r path; do
    [ -e "${REPO_ROOT}/${path}" ] || continue
    if [ "$(bash "${CHANGES}" --classify "${path}")" = "unknown" ]; then
        unknown="${unknown}${path}
"
    fi
done < <(git -C "${REPO_ROOT}" ls-files)
if [ -z "${unknown}" ]; then
    pass 'every tracked file matches a registered rule'
else
    fail "unregistered tracked files (register them in ci/changes.sh):
${unknown}"
fi

echo
if [ "${FAILURES}" -ne 0 ]; then
    echo "changes-test: ${FAILURES} control(s) failed" >&2
    exit 1
fi
echo "changes-test: ${PASS} controls passed"
