#!/usr/bin/env bash
# install-build-kit.sh — the build container's entrypoint: install the pinned
# build-environment packages, then run the build script.
#
# The program writes this script and the package list into the task's `in`
# directory (mounted read-only at `/in`) before starting the build container.
# Every list line is the absolute path of one package: packages from the offline
# folder sit in `/build-kit/`, packages this task downloaded sit in
# `/in/build-kit/`. A failed installation is reported through the existing build
# failure reason (`container_exit`), and the failing dpkg output reaches the
# task log through the container's stderr.
set -u

LIST="${NVM_BUILD_KIT_LIST:-/in/build-kit.list}"
BUILD_SCRIPT="${NVM_BUILD_SCRIPT:-/in/build-driver.sh}"
DPKG_LOG=/tmp/nvm-build-kit-dpkg.log

if [ ! -r "${LIST}" ]; then
    echo "install-build-kit: package list ${LIST} is unreadable" >&2
    exit 1
fi
files=()
while IFS= read -r line; do
    [ -n "${line}" ] || continue
    files+=("${line}")
done < "${LIST}"
if [ "${#files[@]}" -eq 0 ]; then
    echo "install-build-kit: package list ${LIST} is empty" >&2
    exit 1
fi

# One dpkg -i installs the whole list: dpkg resolves the dependencies between
# the packages itself within this single pass.
if ! dpkg -i "${files[@]}" > "${DPKG_LOG}" 2>&1; then
    echo "install-build-kit: dpkg -i failed; tail of ${DPKG_LOG}:" >&2
    tail -n 20 "${DPKG_LOG}" >&2 || true
    exit 1
fi

exec /bin/bash "${BUILD_SCRIPT}"
