#!/usr/bin/env bash
# Generator and fixture-seeding controls without Docker. Temporary directories
# are registered in the shell owning the EXIT trap; seeded inputs and package
# payloads carry numeric root ownership.
set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GENERATOR="${SCRIPT_DIR}/make-fake-run.sh"

[ -f "${GENERATOR}" ] || { echo "make-fake-run-test: missing ${GENERATOR}" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

. "${SCRIPT_DIR}/../test-lib.sh"

expect_empty_scope() { # expect_empty_scope <description> <scope>
    local desc="$1" scope="$2" entries
    entries="$(ls -A "${scope}" 2>/dev/null || true)"
    if [ -z "${entries}" ]; then
        pass "${desc}: temporary scope is empty"
    else
        fail "${desc}: temporary scope still holds: ${entries}"
    fi
}

SCOPE="${WORK}/scope"
mkdir -p "${SCOPE}"

if TMPDIR="${SCOPE}" bash "${GENERATOR}" run 555 "${WORK}/ok.run" \
    >"${WORK}/run.out" 2>"${WORK}/run.err"; then
    pass "run 555 succeeds"
else
    fail "run 555 failed: $(cat "${WORK}/run.err")"
fi
if [ -x "${WORK}/ok.run" ]; then
    pass "run 555 wrote an executable wrapper"
else
    fail "run 555 did not write ${WORK}/ok.run"
fi
expect_empty_scope "run 555 success" "${SCOPE}"

# The failure happens after the temp dir exists: the output path's parent is a
# regular file, so `mkdir -p` fails.
if TMPDIR="${SCOPE}" bash "${GENERATOR}" run 555 "${WORK}/ok.run/out.run" \
    >"${WORK}/run-fail.out" 2>"${WORK}/run-fail.err"; then
    fail "run with an impossible output path reported success"
else
    pass "run with an impossible output path fails"
fi
expect_empty_scope "run 555 failure" "${SCOPE}"

if ! command -v dpkg-deb >/dev/null 2>&1; then
    # dpkg-deb comes with the CI runner (Ubuntu); without it, skip this part
    # and say so instead of reporting a false result.
    echo "UNVERIFIED: make-fake-run-test: dpkg-deb is not installed here; the debs branch was not executed" >&2
elif TMPDIR="${SCOPE}" bash "${GENERATOR}" debs "${WORK}/debs" \
    >"${WORK}/debs.out" 2>"${WORK}/debs.err"; then
    pass "debs succeeds"
else
    fail "debs failed: $(cat "${WORK}/debs.err")"
fi
if command -v dpkg-deb >/dev/null 2>&1; then
    for deb in libnvidia-container1 libnvidia-container-tools nvidia-container-toolkit-base nvidia-container-toolkit; do
        if [ -f "${WORK}/debs/${deb}.deb" ]; then
            pass "debs wrote ${deb}.deb"
            if dpkg-deb --fsys-tarfile "${WORK}/debs/${deb}.deb" \
                | python3 -c '
import sys
import tarfile
members = list(tarfile.open(fileobj=sys.stdin.buffer, mode="r|"))
assert members and all(m.uid == m.gid == 0 for m in members), "package members must be owned by numeric root:root"
'; then
                pass "${deb}.deb payload is owned by numeric root:root"
            else
                fail "${deb}.deb payload is not owned by numeric root:root"
            fi
        else
            fail "debs did not write ${deb}.deb"
        fi
    done
fi
expect_empty_scope "debs success" "${SCOPE}"

# Exercise the same tar stream used to seed volumes without contacting Docker.
awk '/^seed_root\(\) \{/ {copy=1} copy {print} copy && /^}/ {exit}' \
    "${SCRIPT_DIR}/test.sh" > "${WORK}/seed-root.sh"
if ! grep -q '^seed_root() {' "${WORK}/seed-root.sh"; then
    fail "seed_root helper is missing"
else
    . "${WORK}/seed-root.sh"
    TAR_BIN=tar
    if ! "${TAR_BIN}" --version | grep -q 'GNU tar'; then TAR_BIN=gtar; fi
    mkdir -p "${WORK}/seed/source/nested"
    printf 'fixture contents\n' > "${WORK}/seed/source/nested/file"
    chmod 0751 "${WORK}/seed/source/nested/file"
    ln -s nested/file "${WORK}/seed/source/link"
    docker() {
        [ "$#" -eq 3 ] && [ "$1" = cp ] && [ "$2" = - ] \
            && [ "$3" = seed:/host/modules/kernel ] || return 81
        cat > "${WORK}/seed/archive.tar"
    }
    if seed_root "${WORK}/seed/source" . seed:/host/modules/kernel \
        && python3 - "${WORK}/seed/archive.tar" <<'PY_CHECK'
import sys
import tarfile
with tarfile.open(sys.argv[1]) as archive:
    members = archive.getmembers()
    assert members and all(m.uid == m.gid == 0 for m in members)
    assert archive.getmember(".").isdir()
    assert archive.getmember("./nested/file").mode == 0o751
    assert archive.extractfile("./nested/file").read() == b"fixture contents\n"
    assert archive.getmember("./link").issym()
    assert archive.getmember("./link").linkname == "nested/file"
PY_CHECK
    then
        pass "seed stream preserves content, modes and symlinks with numeric root:root"
    else
        fail "seed stream does not preserve root-owned fixture metadata"
    fi
    if (set -o pipefail; seed_root "${WORK}/seed/missing" . seed:/host/modules/kernel) \
        >"${WORK}/seed/missing.out" 2>"${WORK}/seed/missing.err"; then
        fail "seed stream accepted an unreadable source"
    else
        pass "seed stream rejects an unreadable source"
    fi
    docker() { cat >/dev/null; return 82; }
    if seed_root "${WORK}/seed/source" . seed:/host/modules/kernel; then
        fail "seed stream hid a Docker copy failure"
    else
        pass "seed stream reports a Docker copy failure"
    fi
    unset -f docker
fi

finish_tests "make-fake-run-test: ${FAILURES} control(s) failed" \
    "make-fake-run-test: all controls passed"
