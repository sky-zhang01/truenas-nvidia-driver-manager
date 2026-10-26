#!/usr/bin/env bash
# Check the production kernel/module input gates and non-regular source-tree
# refusals locally. Immutable input reads are stubbed for the gate matrix.
set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

awk '/^(kernel_digest|community_inputs)\(\) \{/ { copy=1 } copy { print } copy && /^}/ { copy=0 }' \
    "${REPO_ROOT}/docker/build-driver.sh" > "${WORK}/functions.sh"
. "${WORK}/functions.sh"
for name in kernel_digest community_inputs; do
    declare -F "${name}" >/dev/null \
        || { echo "community-test.sh: missing production function ${name}" >&2; exit 1; }
done

passed=0
check() {
    local title="$1"
    shift
    if "$@"; then
        printf 'PASS community: %s\n' "${title}"
        passed=$((passed + 1))
    else
        printf 'FAIL community: %s\n' "${title}" >&2
        exit 1
    fi
}
rejects() { if "$@"; then return 1; else return 0; fi; }

DRIVER_RUN="${WORK}/driver.run"
COMMUNITY_ARCHIVE="${WORK}/community.tar.xz"
NVM_COMMUNITY_PATCH=470.256.02
NVM_KERNEL_MODULE_TYPE=
REJECT_ARCHIVE=false
fixed_file() {
    printf '%s\n' "$1" >> "${WORK}/fixed-calls"
    [ "$1" != "${COMMUNITY_ARCHIVE}" ] || ! "${REJECT_ARCHIVE}"
}
KERNEL=6.12.105-production+truenas
check '6.12 passes the kernel and module gates' community_inputs
KERNEL=6.18.52-production+truenas
: > "${WORK}/fixed-calls"
check '6.18 reaches both immutable input checks' community_inputs
check '6.18 checks the archive after the installer' test "$(cat "${WORK}/fixed-calls")" = "$(printf '%s\n%s' "${DRIVER_RUN}" "${COMMUNITY_ARCHIVE}")"
for KERNEL in 6.13.1 6.180.1 6.18evil.1 6.18.x; do
    : > "${WORK}/fixed-calls"
    check "${KERNEL} is refused" rejects community_inputs
    check "${KERNEL} stops before immutable input reads" test ! -s "${WORK}/fixed-calls"
done
KERNEL=6.18.52-production+truenas
NVM_KERNEL_MODULE_TYPE=proprietary
check 'an explicit module type is refused' rejects community_inputs
NVM_KERNEL_MODULE_TYPE=
REJECT_ARCHIVE=true
check 'an archive mismatch is refused' rejects community_inputs
REJECT_ARCHIVE=false
NVM_COMMUNITY_PATCH=470.999.01
check 'an unknown patch marker is refused' rejects community_inputs
NVM_COMMUNITY_PATCH=470.256.02
PROBE_DIR="${WORK}/probe"
mkdir -p "${PROBE_DIR}/kernel/sub"
printf one > "${PROBE_DIR}/kernel/a first"
printf two > "${PROBE_DIR}/kernel/sub/z-last"
ln -s 'a first' "${PROBE_DIR}/kernel/link"
check 'kernel digest refuses a symlink member' rejects kernel_digest
rm "${PROBE_DIR}/kernel/link"
mkfifo "${PROBE_DIR}/kernel/pipe"
check 'kernel digest refuses a special member' rejects kernel_digest
rm "${PROBE_DIR}/kernel/pipe"
mv "${PROBE_DIR}/kernel" "${PROBE_DIR}/real-kernel"
ln -s real-kernel "${PROBE_DIR}/kernel"
check 'kernel digest refuses a symlink root' rejects kernel_digest
rm "${PROBE_DIR}/kernel"
check 'kernel digest refuses a missing root' rejects kernel_digest

printf 'community-test.sh: %s local checks passed\n' "${passed}"
