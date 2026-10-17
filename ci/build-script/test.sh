#!/usr/bin/env bash
# Exercise the build script in the pinned Debian environment. The first build
# installs the manifest packages through the production entrypoint. Subsequent
# builds start fresh containers from a separately installed helper image.
# Containers, volumes and the helper image are registered for proven cleanup.
#
# Inputs:
#   NVM_BUILD_ENV_IMAGE     Pinned official image reference.
#   NVM_BUILD_KIT           Verified package directory; required.
#   NVM_BUILD_ENV_PLATFORM  Target platform, default linux/amd64.
set -Eeuo pipefail

IMAGE_REF="${NVM_BUILD_ENV_IMAGE:-debian@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587}"
IMAGE_ID_EXPECTED="sha256:d4c45f66efc59f89b0597d343a9bd72808493c0eacecde5285961743252cee23"
IMAGE_ID_INDEX="sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587"
PLATFORM="${NVM_BUILD_ENV_PLATFORM:-linux/amd64}"
KIT="${NVM_BUILD_KIT:-}"
KERNEL="$(uname -r)"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
MANIFEST="${REPO_ROOT}/src/build_kit/packages.tsv"

# Fixture archive creation requires GNU tar, available as gtar on macOS.
TAR_BIN=tar
if ! "${TAR_BIN}" --version 2>/dev/null | grep -q 'GNU tar'; then
    TAR_BIN=gtar
    "${TAR_BIN}" --version 2>/dev/null | grep -q 'GNU tar' \
        || { echo "test.sh: GNU tar is required (install gtar on macOS)" >&2; exit 1; }
fi

# Match production root ownership, including directory roots and symlinks.
seed_root() { # seed_root <source directory> <member> <container:destination directory>
    "${TAR_BIN}" --numeric-owner --owner=0 --group=0 -C "$1" -cf - "$2" \
        | docker cp - "$3" >/dev/null
}

command -v docker >/dev/null 2>&1 \
    || { echo "test.sh: docker is not installed; cannot run the build script tests" >&2; exit 1; }

file_sha256() { # file_sha256 <path>
    sha256sum "$1" 2>/dev/null | cut -d' ' -f1 || shasum -a 256 "$1" | cut -d' ' -f1
}

# The same rule the runner follows: use the local image, pull only when it is
# missing, then verify the local image ID (a classic image store reports the
# per-platform ID, a containerd-backed store the index digest; both name the
# same pinned amd64 content). The build containers name the verified value with
# `--pull never`.
actual=""
if ! actual="$(docker image inspect -f '{{.Id}}' "${IMAGE_REF}" 2>/dev/null)"; then
    echo "test.sh: pulling ${IMAGE_REF}"
    docker pull --platform "${PLATFORM}" "${IMAGE_REF}" >&2
    actual="$(docker image inspect -f '{{.Id}}' "${IMAGE_REF}")"
fi
case "${actual}" in
    "${IMAGE_ID_EXPECTED}" | "${IMAGE_ID_INDEX}") ;;
    *) { echo "test.sh: image ${IMAGE_REF} has ID ${actual}, expected ${IMAGE_ID_EXPECTED}" >&2; exit 1; } ;;
esac
BUILD_IMAGE="${actual}"

[ -n "${KIT}" ] \
    || { echo "test.sh: set NVM_BUILD_KIT to the verified package directory (tools/build-kit.sh fetch)" >&2; exit 1; }
[ -d "${KIT}" ] || { echo "test.sh: ${KIT} is not a directory" >&2; exit 1; }
[ -f "${MANIFEST}" ] || { echo "test.sh: ${MANIFEST} is missing" >&2; exit 1; }
kit_files=0
while IFS="$(printf '\t')" read -r sha bytes filename package version url; do
    case "${sha}" in "" | "#"*) continue ;; esac
    target="${KIT}/${filename}"
    [ -f "${target}" ] || { echo "test.sh: ${target} is missing" >&2; exit 1; }
    [ "$(stat -c %s "${target}" 2>/dev/null || stat -f %z "${target}")" = "${bytes}" ] \
        || { echo "test.sh: ${target} has the wrong size" >&2; exit 1; }
    [ "$(file_sha256 "${target}")" = "${sha}" ] \
        || { echo "test.sh: ${target} has the wrong SHA256" >&2; exit 1; }
    kit_files=$((kit_files + 1))
done < "${MANIFEST}"
[ "${kit_files}" -gt 0 ] || { echo "test.sh: the manifest has no package row" >&2; exit 1; }
echo "test.sh: build environment ${IMAGE_REF} (${BUILD_IMAGE}) with ${kit_files} package(s)"

# The image's default PATH: prepend the stub directory when injecting a fake find.
CONTAINER_PATH="$(docker run --rm --platform "${PLATFORM}" --entrypoint /bin/sh "${BUILD_IMAGE}" -c 'printf %s "$PATH"')"
[ -n "${CONTAINER_PATH}" ] || { echo "test.sh: cannot read the image PATH" >&2; exit 1; }

WORK="${NVM_CI_WORK:-$(mktemp -d)}"
mkdir -p "${WORK}"
RUN_TOKEN="${RANDOM}${RANDOM}"
# The shared teardown machinery (ci/cleanup-lib.sh): containers and volumes are
# registered one by one and at EXIT torn down by the same implementation with
# read-back proof; a failed query is never "removed", and a volume carrying
# another run's label is never touched.
. "${SCRIPT_DIR}/../cleanup-lib.sh"

nvm_cleanup_one() { # nvm_cleanup_one <item> — item is container:<id> / volume:<name> / image:<tag>
    case "$1" in
        container:*) nvm_container_remove "${1#container:}" ;;
        volume:*) nvm_volume_remove "${1#volume:}" "${RUN_TOKEN}" ;;
        image:*) nvm_image_remove "${1#image:}" ;;
        *) echo "test.sh: unknown cleanup item $1" >&2; return 1 ;;
    esac
}

trap 'nvm_cleanup_finish "$?" "${WORK}"' EXIT

# The script runs in the CI job container while the Docker daemon is on the host; a
# `type=bind` source is resolved by the daemon on the host's filesystem, so it
# cannot see fixtures under the job container's /tmp. Named volumes belong to the
# daemon: a created (never started) seed
# container receives the fake host tree with `docker cp`, then the build
# containers mount the same volumes `readonly`.
new_volume() { # new_volume <suffix>; sets NEW_VOLUME
    # Let Docker allocate a unique name; an existing named volume is never reused.
    # Register before validating: when the check fails the volume
    # stays on the books, so the EXIT finish retries its removal and turns red
    # instead of silently leaking it.
    NEW_VOLUME="$(docker volume create --label "nvm.ci.run=${RUN_TOKEN}" --label "nvm.ci.part=$1")"
    [ -n "${NEW_VOLUME}" ] || return 1
    nvm_cleanup_track "volume:${NEW_VOLUME}"
    [ "$(docker volume inspect -f '{{index .Labels "nvm.ci.run"}}' "${NEW_VOLUME}")" = "${RUN_TOKEN}" ] || return 1
}
new_volume hdr; HDR_VOL="${NEW_VOLUME}"
new_volume boot; BOOT_VOL="${NEW_VOLUME}"
new_volume mods; MODS_VOL="${NEW_VOLUME}"
new_volume in; IN_VOL="${NEW_VOLUME}"
new_volume find; FIND_VOL="${NEW_VOLUME}"
new_volume artifact; ARTIFACT_VOL="${NEW_VOLUME}"
new_volume kit; KIT_VOL="${NEW_VOLUME}"

# The block below, up to the artifact registration line, is extracted verbatim by
# `ci/cleanup-lib-test.sh` and run against the stub docker in its control
# environment, so every variable it references — HDR_VOL, BOOT_VOL,
# MODS_VOL, IN_VOL, FIND_VOL, KERNEL, PLATFORM, BUILD_IMAGE, ARTIFACT_VOL — must
# be defined there too; a missing one is a bare `unbound variable` under
# `set -u`, and the control reports "references undefined variables" first and
# turns the CI red.
SEED_INPUTS="nvm-build-script-seed-${HDR_VOL}"
# docker create prints the container ID; registration must use that ID, not the
# name: `ps --filter id=` only matches IDs, and querying by name would judge a
# still-existing container as gone.
SEED_INPUTS_CID="$(docker create --name "${SEED_INPUTS}" --platform "${PLATFORM}" --pull never --entrypoint /bin/true \
    --mount "type=volume,src=${HDR_VOL},dst=/usr/src/linux-headers-truenas-production-amd64" \
    --mount "type=volume,src=${BOOT_VOL},dst=/boot" \
    --mount "type=volume,src=${MODS_VOL},dst=/host/modules/${KERNEL}" \
    --mount "type=volume,src=${IN_VOL},dst=/in" \
    --mount "type=volume,src=${FIND_VOL},dst=/find-shim" \
    "${BUILD_IMAGE}" /bin/true)"
nvm_cleanup_track "container:${SEED_INPUTS_CID}"

SEED_ARTIFACT="nvm-build-script-artifact-${ARTIFACT_VOL}"
SEED_ARTIFACT_CID="$(docker create --name "${SEED_ARTIFACT}" --platform "${PLATFORM}" --pull never --entrypoint /bin/true \
    --mount "type=volume,src=${ARTIFACT_VOL},dst=/w" \
    "${BUILD_IMAGE}" /bin/true)"
nvm_cleanup_track "container:${SEED_ARTIFACT_CID}"

# The package directory (a daemon-side named volume): a created (never started)
# seed container receives the .deb files with `docker cp`, and the build
# containers mount the same volume `readonly`.
SEED_KIT="nvm-build-script-kit-${KIT_VOL}"
SEED_KIT_CID="$(docker create --name "${SEED_KIT}" --platform "${PLATFORM}" --pull never --entrypoint /bin/true \
    --mount "type=volume,src=${KIT_VOL},dst=/build-kit" \
    "${BUILD_IMAGE}" /bin/true)"
nvm_cleanup_track "container:${SEED_KIT_CID}"
seed_root "${KIT}" . "${SEED_KIT_CID}:/build-kit"

. "${SCRIPT_DIR}/../test-lib.sh"

HDR="${WORK}/hdr"
CFG="${WORK}/boot/config-${KERNEL}"
MODS="${WORK}/mods/${KERNEL}"
IN="${WORK}/in"
OUT_EMPTY="${WORK}/out"
BIN="${WORK}/bin"

mkdir -p "${HDR}/include/config" "${WORK}/boot" "${MODS}/kernel/drivers/video" \
         "${IN}/toolkit" "${OUT_EMPTY}" "${BIN}"

write_auto_conf() { # $1 = gcc major (default 12)
    local major="${1:-12}"
    printf 'CONFIG_CC_VERSION_TEXT="gcc-%s (Debian 12.2.0-14) %s.2.0"\n' "${major}" "${major}" \
        > "${HDR}/include/config/auto.conf"
    printf 'CONFIG_CC_VERSION_TEXT="gcc-%s (Debian 12.2.0-14) %s.2.0"\n' "${major}" "${major}" \
        > "${CFG}"
}
write_auto_conf 12

for ko in nvidia nvidia-uvm nvidia-modeset nvidia-drm nvidia-peermem; do
    printf 'stub %s\n' "${ko}" > "${MODS}/kernel/drivers/video/${ko}.ko"
done
for idx in modules.alias modules.alias.bin modules.builtin.alias.bin modules.builtin.bin \
           modules.dep modules.dep.bin modules.devname modules.softdep modules.symbols modules.symbols.bin; do
    printf 'stub %s\n' "${idx}" > "${MODS}/${idx}"
done
for base in modules.builtin modules.builtin.modinfo modules.order; do
    printf 'stub %s\n' "${base}" > "${MODS}/${base}"
done

for variant in 555 545 470 check-fail no-options install-fail install-fail-kernel; do
    bash "${SCRIPT_DIR}/make-fake-run.sh" run "${variant}" "${BIN}/${variant}.run"
done
bash "${SCRIPT_DIR}/make-fake-run.sh" debs "${IN}/toolkit"
for deb in libnvidia-container1 libnvidia-container-tools nvidia-container-toolkit-base nvidia-container-toolkit; do
    [ -f "${IN}/toolkit/${deb}.deb" ] || { echo "test.sh: missing ${deb}.deb" >&2; exit 1; }
done

# The two scripts the program embeds and the package list: the runner writes them
# into the task's `in` directory before a build, and this script puts the same
# files into the `in` volume; the build script is the repository's own copy.
cp "${REPO_ROOT}/docker/install-build-kit.sh" "${IN}/install-build-kit.sh"
cp "${REPO_ROOT}/docker/build-driver.sh" "${IN}/build-driver.sh"
: > "${IN}/build-kit.list"
for deb in "${KIT}"/*.deb; do
    printf '/build-kit/%s\n' "$(basename "${deb}")" >> "${IN}/build-kit.list"
done
[ -s "${IN}/build-kit.list" ] || { echo "test.sh: the package list is empty" >&2; exit 1; }

# Reproduce the official package's non-searchable root without changing its files.
DEB_ROOT_FIXTURE="${WORK}/deb-root-fixture"
mkdir -p "${DEB_ROOT_FIXTURE}/tree"
dpkg-deb -x "${IN}/toolkit/nvidia-container-toolkit.deb" "${DEB_ROOT_FIXTURE}/tree"
dpkg-deb --ctrl-tarfile "${IN}/toolkit/nvidia-container-toolkit.deb" > "${DEB_ROOT_FIXTURE}/control.tar"
printf '2.0\n' > "${DEB_ROOT_FIXTURE}/debian-binary"
"${TAR_BIN}" --numeric-owner --owner=0 --group=0 --mode=0644 --no-recursion -C "${DEB_ROOT_FIXTURE}/tree" \
    -cf "${DEB_ROOT_FIXTURE}/data.tar" .
"${TAR_BIN}" --numeric-owner --owner=0 --group=0 -C "${DEB_ROOT_FIXTURE}/tree" \
    -rf "${DEB_ROOT_FIXTURE}/data.tar" ./usr ./etc
(cd "${DEB_ROOT_FIXTURE}" && ar qc toolkit.deb debian-binary control.tar data.tar)
mv "${DEB_ROOT_FIXTURE}/toolkit.deb" "${IN}/toolkit/nvidia-container-toolkit.deb"
dpkg-deb -c "${IN}/toolkit/nvidia-container-toolkit.deb" > "${DEB_ROOT_FIXTURE}/listing"
awk '$NF == "./" { n++; good=($1 == "drw-r--r--") } END { exit !(n == 1 && good) }' \
    "${DEB_ROOT_FIXTURE}/listing"

# A `find` that emits part of the list and then fails: a partial list must never
# reach the artifact.
FIND_SHIM="${BIN}/find-bin"
mkdir -p "${FIND_SHIM}"
cat >"${FIND_SHIM}/find" <<'FIND_SHIM_BODY'
#!/bin/sh
printf '%s' '/usr/lib/nvm-partial-find.so'
printf '\0'
echo "test.sh: injected partial find failure" >&2
exit 1
FIND_SHIM_BODY
chmod 0755 "${FIND_SHIM}/find"

mkdir -p "${FIND_SHIM}/chmod-fail"
cat > "${FIND_SHIM}/chmod-fail/chmod" <<'CHMOD_SHIM_BODY'
#!/bin/sh
if [ "${1:-}" = u+x ] && [ "${2:-}" = /tmp/nvm-deb/nvidia-container-toolkit ]; then
    echo "test.sh: injected toolkit root chmod failure" >&2
    exit 73
fi
exec /usr/bin/chmod "$@"
CHMOD_SHIM_BODY
chmod 0755 "${FIND_SHIM}/chmod-fail/chmod"

# Seed the daemon-side volumes; `run_build` reseeds HDR/CFG/IN after its rewrites.
seed_inputs() {
    seed_root "${HDR}" . "${SEED_INPUTS}:/usr/src/linux-headers-truenas-production-amd64"
    seed_root "${WORK}/boot" "config-${KERNEL}" "${SEED_INPUTS}:/boot"
    seed_root "${IN}" . "${SEED_INPUTS}:/in"
}
seed_inputs
seed_root "${MODS}" . "${SEED_INPUTS}:/host/modules/${KERNEL}"
seed_root "${FIND_SHIM}" . "${SEED_INPUTS}:/find-shim"

# The helper containers (artifact listings, small in-container commands) use an
# image this run derives itself: the official image plus the manifest packages.
# The derivation does not use `docker build`; it runs the same installation path
# as a build container (official image, read-only package directory, installer)
# once and commits the result, so it also works where no builder is available.
HELPER_IMAGE="nvm-ci-build-env-${RUN_TOKEN}"
HELPER_SEED="nvm-build-script-helper-${KIT_VOL}"
HELPER_SEED_CID="$(docker create --name "${HELPER_SEED}" \
    --platform "${PLATFORM}" --pull never --network none \
    -e NVM_BUILD_KIT_LIST=/in/build-kit.list \
    -e NVM_BUILD_SCRIPT=/dev/null \
    --mount "type=volume,src=${KIT_VOL},dst=/build-kit,readonly" \
    --mount "type=volume,src=${IN_VOL},dst=/in,readonly" \
    --entrypoint /bin/bash \
    "${BUILD_IMAGE}" /in/install-build-kit.sh)"
nvm_cleanup_track "container:${HELPER_SEED_CID}"
if ! docker start -a "${HELPER_SEED_CID}" >"${WORK}/helper-install.log" 2>&1; then
    echo "test.sh: the helper image could not install the build kit:" >&2
    sed -n '1,20p' "${WORK}/helper-install.log" >&2 || true
    exit 1
fi
docker commit "${HELPER_SEED_CID}" "${HELPER_IMAGE}" >/dev/null
nvm_cleanup_track "image:${HELPER_IMAGE}"
nvm_container_remove "${HELPER_SEED_CID}" \
    && nvm_cleanup_untrack "container:${HELPER_SEED_CID}"

seed_artifact() { # seed_artifact <file>... — copy artifact files from WORK into the artifact volume
    local f
    for f in "$@"; do
        docker cp "${WORK}/${f}" "${SEED_ARTIFACT}:/w/${f}" >/dev/null
    done
}

LAST_NAME=""
LAST_RC=0
LAST_LOG=""
BUILD_RUNS=0

run_build() { # run_build <variant> [module-type] [gcc-major] [failure-mode] [community-marker]
    local variant="$1" mtype="${2:-}" gcc="${3:-12}" failure_mode="${4:-}" marker="${5:-}"
    cp "${BIN}/${variant}.run" "${IN}/driver.run"
    write_auto_conf "${gcc}"
    seed_inputs

    local name image="${HELPER_IMAGE}" script=/in/build-driver.sh
    # The first build exercises the real package installer; later builds reuse
    # the separately installed helper image with a fresh container each time.
    if [ "${BUILD_RUNS}" -eq 0 ]; then
        image="${BUILD_IMAGE}"
        script=/in/install-build-kit.sh
    fi
    BUILD_RUNS=$((BUILD_RUNS + 1))
    local env_args=()
    if [ -n "${mtype}" ]; then
        env_args=(-e "NVM_KERNEL_MODULE_TYPE=${mtype}")
    fi
    if [ "${failure_mode}" = fail-deb-chmod ]; then
        env_args+=(-e "PATH=/find-shim/chmod-fail:${CONTAINER_PATH}")
    fi
    if [ "${failure_mode}" = fail-find ]; then
        env_args+=(-e "PATH=/find-shim:${CONTAINER_PATH}")
    fi
    if [ -n "${marker}" ]; then
        env_args+=(-e "NVM_COMMUNITY_PATCH=${marker}")
    fi

    name="$(docker create --cap-drop ALL --platform "${PLATFORM}" --pull never --network none \
        -e TAR_OPTIONS=--no-same-owner \
        -e NVM_BUILD_KIT_LIST=/in/build-kit.list \
        -e NVM_BUILD_SCRIPT=/in/build-driver.sh \
        ${env_args[@]+"${env_args[@]}"} \
        --mount "type=volume,src=${KIT_VOL},dst=/build-kit,readonly" \
        --mount "type=volume,src=${FIND_VOL},dst=/find-shim,readonly" \
        --mount "type=volume,src=${HDR_VOL},dst=/usr/src/linux-headers-truenas-production-amd64,readonly" \
        --mount "type=volume,src=${BOOT_VOL},dst=/boot,readonly" \
        --mount "type=volume,src=${MODS_VOL},dst=/host/modules/${KERNEL},readonly" \
        --mount "type=volume,src=${IN_VOL},dst=/in,readonly" \
        --entrypoint /bin/bash \
        "${image}" "${script}")"
    nvm_cleanup_track "container:${name}"
    seed_root "${WORK}" out "${name}:/"

    LAST_NAME="${name}"
    LAST_LOG="${WORK}/log-${variant}-${mtype:-none}-${gcc}.txt"
    set +e
    docker start -a "${name}" >"${LAST_LOG}" 2>&1
    LAST_RC=$?
    set -e
}

log_has() { grep -qF -- "$1" "${LAST_LOG}"; }
copy_out() { docker cp "${LAST_NAME}:$1" "$2" >/dev/null 2>&1; }

# The artifact listing must first succeed and be complete; only then may
# presence or absence be asserted. On a failed or empty listing both
# assertions fail, and the real rc and stderr are preserved.
artifact_paths() {
    local out="${WORK}/artifact-list.stdout"
    local err="${WORK}/artifact-list.stderr"
    local rc=0
    docker run --rm --platform "${PLATFORM}" --entrypoint /bin/sh \
        --mount "type=volume,src=${ARTIFACT_VOL},dst=/w,readonly" "${HELPER_IMAGE}" \
        -c "unsquashfs -l /w/nvidia.raw" >"${out}" 2>"${err}" || rc=$?
    if [ "${rc}" -ne 0 ]; then
        echo "test.sh: artifact listing failed (rc=${rc})" >&2
        sed -n '1,5p' "${err}" >&2 || true
        return 1
    fi
    if ! grep -q '^squashfs-root/' "${out}"; then
        echo "test.sh: artifact listing is empty" >&2
        sed -n '1,5p' "${err}" >&2 || true
        return 1
    fi
    sed 's#^squashfs-root/##' "${out}"
}
artifact_has() {
    local listing
    listing="$(artifact_paths)" || return 1
    grep -qxF -- "$1" <<<"${listing}"
}
artifact_lacks() {
    local listing
    listing="$(artifact_paths)" || return 1
    ! grep -qxF -- "$1" <<<"${listing}"
}
artifact_cat() {
    docker run --rm --platform "${PLATFORM}" --entrypoint /bin/sh \
        --mount "type=volume,src=${ARTIFACT_VOL},dst=/w,readonly" "${HELPER_IMAGE}" \
        -c "unsquashfs -cat /w/nvidia.raw '$1'" 2>/dev/null
}

echo "== first step is check (555) =="
run_build 555
check "555 exit code 0" test "${LAST_RC}" -eq 0
check "first NVM:STEP is check" bash -c "test \"\$(grep -m1 '^NVM:STEP:' '${LAST_LOG}')\" = NVM:STEP:check"
check "no NVM:FAIL in a successful build" bash -c "! grep -q '^NVM:FAIL:' '${LAST_LOG}'"
copy_out /usr/share/nvm-fake-calls.log "${WORK}/calls-555"
check "fake .run was called with --check first" bash -c "test \"\$(head -1 '${WORK}/calls-555')\" = check"

echo "== --check failure =="
run_build check-fail
check "check-fail exit code 1" test "${LAST_RC}" -eq 1
check "check-fail reason run_check_failed" log_has "NVM:FAIL:run_check_failed"
check "check-fail stopped before the modules step" bash -c "! grep -q '^NVM:STEP:modules' '${LAST_LOG}'"

echo "== probe: install mode and flag_unsupported =="
run_build 555
check "555: no flag_unsupported line" bash -c "! grep -q 'NVM:INFO:flag_unsupported=' '${LAST_LOG}'"
check "555: install_mode=single" log_has "NVM:INFO:install_mode=single"
check "555: cc=gcc-12" log_has "NVM:INFO:cc=gcc-12"
copy_out /usr/share/nvm-fake-installer "${WORK}/argv-555"
check "555: one install pass" bash -c "[ \"\$(ls '${WORK}/argv-555' | wc -l | tr -d ' ')\" = 1 ]"
check "555: argv has --allow-installation-with-running-driver" \
    bash -c "grep -q 'argv\[[0-9]*\]=--allow-installation-with-running-driver' '${WORK}/argv-555/argv.1'"
check "555: argv has --skip-module-load" bash -c "grep -q '=--skip-module-load' '${WORK}/argv-555/argv.1'"
check "555: argv has no --kernel-module-type" bash -c "! grep -q -- '--kernel-module-type' '${WORK}/argv-555/argv.1'"
check "555: argv has no --no-kernel-module" bash -c "! grep -q -- '--no-kernel-module' '${WORK}/argv-555/argv.1'"

run_build 545
check "545: flag_unsupported=--skip-module-load" log_has "NVM:INFO:flag_unsupported=--skip-module-load"
check "545: install_mode=single" log_has "NVM:INFO:install_mode=single"
check "545: flag_unsupported has no --kernel-module-type" \
    bash -c "! grep 'NVM:INFO:flag_unsupported=' '${LAST_LOG}' | grep -q -- '--kernel-module-type'"
copy_out /usr/share/nvm-fake-installer "${WORK}/argv-545"
check "545: one install pass" bash -c "[ \"\$(ls '${WORK}/argv-545' | wc -l | tr -d ' ')\" = 1 ]"

run_build 470
check "470: install_mode=two-pass" log_has "NVM:INFO:install_mode=two-pass"
check "470: flag_unsupported=--skip-module-load" log_has "NVM:INFO:flag_unsupported=--skip-module-load"
check "470: flag_unsupported=--no-rebuild-initramfs" log_has "NVM:INFO:flag_unsupported=--no-rebuild-initramfs"
check "470: flag_unsupported=--allow-installation-with-running-driver" \
    log_has "NVM:INFO:flag_unsupported=--allow-installation-with-running-driver"
copy_out /usr/share/nvm-fake-installer "${WORK}/argv-470"
check "470: two install passes" bash -c "[ \"\$(ls '${WORK}/argv-470' | wc -l | tr -d ' ')\" = 2 ]"
check "470: pass 1 has --no-kernel-module" bash -c "grep -q '=--no-kernel-module' '${WORK}/argv-470/argv.1'"
check "470: pass 2 has --kernel-module-only" bash -c "grep -q '=--kernel-module-only' '${WORK}/argv-470/argv.2'"
check "470: the two passes ran in different directories" \
    bash -c "! diff <(grep '^cwd=' '${WORK}/argv-470/argv.1') <(grep '^cwd=' '${WORK}/argv-470/argv.2') >/dev/null"
check "470: each pass ran inside its own makeself directory" \
    bash -c "grep -Eq '^cwd=/tmp/makeself\\.' '${WORK}/argv-470/argv.1' && grep -Eq '^cwd=/tmp/makeself\\.' '${WORK}/argv-470/argv.2'"

echo "== module type =="
run_build 555 open
check "555+open: exit 0" test "${LAST_RC}" -eq 0
copy_out /usr/share/nvm-fake-installer "${WORK}/argv-555-open"
check "555+open: argv has --kernel-module-type=open" \
    bash -c "grep -q '=--kernel-module-type=open' '${WORK}/argv-555-open/argv.1'"

run_build 555 proprietary
copy_out /usr/share/nvm-fake-installer "${WORK}/argv-555-prop"
check "555+proprietary: argv has --kernel-module-type=proprietary" \
    bash -c "grep -q '=--kernel-module-type=proprietary' '${WORK}/argv-555-prop/argv.1'"

run_build 545 open
check "545+open: exit code 1" test "${LAST_RC}" -eq 1
check "545+open: reason kernel_module_type_unsupported" log_has "NVM:FAIL:kernel_module_type_unsupported"
if copy_out /usr/share/nvm-fake-installer "${WORK}/argv-545-open"; then
    fail "545+open: no argv file (the installer must not run)"
else
    pass "545+open: no argv file (the installer must not run)"
fi
if copy_out /usr/share/nvm-fake-calls.log "${WORK}/calls-545-open"; then
    check "545+open: the installer was never invoked" \
        bash -c "! grep -qxF install '${WORK}/calls-545-open'"
else
    pass "545+open: the installer was never invoked"
fi

run_build 470 open
check "470+open: reason kernel_module_type_unsupported" log_has "NVM:FAIL:kernel_module_type_unsupported"

echo "== community patch input is fail-closed =="
run_build check-fail "" 12 "" 470.256.02
check "community: original --check still fails first" log_has "NVM:FAIL:run_check_failed"
check "community: failed --check has no patch step" bash -c "! grep -q '^NVM:STEP:community_patch$' '${LAST_LOG}'"

run_build 470 "" 12 "" 470.999.01
check "community: unknown marker exits 1" test "${LAST_RC}" -eq 1
check "community: unknown marker rejects the archive/source binding" \
    log_has "NVM:FAIL:community_patch_failed:archive"
check "community: unknown marker stops before extraction" \
    bash -c "! grep -q '^NVM:STEP:modules$' '${LAST_LOG}'"
check "community: unknown marker does not install" \
    bash -c "! grep -q '^NVM:STEP:install_userspace$' '${LAST_LOG}'"

run_build 470 "" 12 "" 470.256.02
check "community: untrusted fake .run or absent archive exits 1" test "${LAST_RC}" -eq 1
check "community: untrusted fake .run or absent archive is rejected" \
    log_has "NVM:FAIL:community_patch_failed:archive"
check "community: only --check runs before fixed input rejection" \
    bash -c "! grep -q '^NVM:STEP:modules$' '${LAST_LOG}'"
check "community: no user-space install after input rejection" \
    bash -c "! grep -q '^NVM:STEP:install_userspace$' '${LAST_LOG}'"
if copy_out /out/nvidia.raw "${WORK}/community-rejected.raw"; then
    fail "community: rejected input must not produce nvidia.raw"
else
    pass "community: rejected input produced no nvidia.raw"
fi

echo "== capture =="
run_build 555
docker cp "${LAST_NAME}:/out/nvidia.raw" "${WORK}/nvidia.raw" >/dev/null
docker cp "${LAST_NAME}:/out/result.txt" "${WORK}/result.txt" >/dev/null
seed_artifact nvidia.raw
check "555: artifact checksum equals the record" bash -c "
    recorded=\"\$(sed -n 's/^ARTIFACT_SHA256=//p' '${WORK}/result.txt')\"
    bytes=\"\$(sed -n 's/^ARTIFACT_BYTES=//p' '${WORK}/result.txt')\"
    actual_sha=\"\$(sha256sum '${WORK}/nvidia.raw' 2>/dev/null | cut -d' ' -f1 || shasum -a 256 '${WORK}/nvidia.raw' | cut -d' ' -f1)\"
    actual_bytes=\"\$(wc -c < '${WORK}/nvidia.raw' | tr -d ' ')\"
    [ -n \"\${recorded}\" ] && [ \"\${recorded}\" = \"\${actual_sha}\" ] && [ \"\${bytes}\" = \"\${actual_bytes}\" ]"
check "artifact has the installer's /usr files" artifact_has "usr/lib/libnvm-fake.so.1"
check "artifact has nvidia-smi" artifact_has "usr/bin/nvidia-smi"
for ko in nvidia nvidia-uvm nvidia-modeset nvidia-drm nvidia-peermem; do
    check "artifact has module ${ko}.ko" artifact_has "usr/lib/modules/${KERNEL}/kernel/drivers/video/${ko}.ko"
done
for idx in modules.alias modules.alias.bin modules.builtin.alias.bin modules.builtin.bin \
           modules.dep modules.dep.bin modules.devname modules.softdep modules.symbols modules.symbols.bin; do
    check "artifact has depmod index ${idx}" artifact_has "usr/lib/modules/${KERNEL}/${idx}"
done
# The coverage assertions must detect missing items: a known-absent path must
# fail artifact_has and a present path must fail artifact_lacks.
if artifact_has "usr/lib/definitely-not-in-the-artifact"; then
    fail "artifact_has control: a missing path passed"
else
    pass "artifact_has control: a missing path fails"
fi
if artifact_lacks "usr/bin/nvidia-smi"; then
    fail "artifact_lacks control: a present path passed"
else
    pass "artifact_lacks control: a present path fails"
fi
# A failed listing (the counterexample: a docker stub that exits 55 with empty
# stdout) must fail both assertions; evidence failure is never "absent".
mkdir -p "${WORK}/dock-stub"
cat >"${WORK}/dock-stub/docker" <<'DOCK_STUB'
#!/bin/sh
echo "stub docker: listing failed" >&2
exit 55
DOCK_STUB
chmod 0755 "${WORK}/dock-stub/docker"
if ( PATH="${WORK}/dock-stub:${PATH}"; artifact_has "usr/bin/nvidia-smi" ) >/dev/null 2>&1; then
    fail "artifact_has control: a failed listing was accepted"
else
    pass "artifact_has control: a failed listing fails"
fi
if ( PATH="${WORK}/dock-stub:${PATH}"; artifact_lacks "usr/src/nvidia-999.0.0/nvidia.c" ) >/dev/null 2>&1; then
    fail "artifact_lacks control: a failed listing was accepted"
else
    pass "artifact_lacks control: a failed listing fails"
fi
check "artifact excludes /usr/src" artifact_lacks "usr/src/nvidia-999.0.0/nvidia.c"
check "artifact excludes /etc (Vulkan)" artifact_lacks "etc/vulkan/icd.d/nvm_fake.json"
check "artifact excludes /var" artifact_lacks "var/log/nvidia-installer.log"
for base in modules.builtin modules.builtin.modinfo modules.order; do
    check "artifact excludes base index ${base}" artifact_lacks "usr/lib/modules/${KERNEL}/${base}"
done
check "toolkit lib/ mapped to usr/lib/" artifact_has "usr/lib/systemd/system/nvidia-cdi-refresh.path"
check "toolkit root 0644: runtime hook reaches the artifact without capabilities" \
    artifact_has "usr/bin/nvidia-container-runtime-hook"
check "toolkit etc/ excluded" artifact_lacks "etc/nvidia-container-toolkit/nvidia-cdi-refresh.env"
artifact_cat usr/lib/extension-release.d/extension-release.nvidia > "${WORK}/extension-release"
check "extension-release.nvidia content is ID=_any" bash -c "test \"\$(cat '${WORK}/extension-release')\" = 'ID=_any'"
docker cp "${LAST_NAME}:/usr/lib/modules/${KERNEL}/modules.dep" "${WORK}/modules.dep" >/dev/null
artifact_cat "usr/lib/modules/${KERNEL}/modules.dep" > "${WORK}/modules.dep.artifact"
check "container modules.dep equals the artifact's" cmp -s "${WORK}/modules.dep" "${WORK}/modules.dep.artifact"

echo "== two builds of the same fake .run give identical path listings =="
run_build 555
copy_out /out/nvidia.raw "${WORK}/nvidia.raw"
mv "${WORK}/nvidia.raw" "${WORK}/nvidia.raw.first"
seed_artifact nvidia.raw.first
docker run --rm --platform "${PLATFORM}" --entrypoint /bin/sh \
    --mount "type=volume,src=${ARTIFACT_VOL},dst=/w,readonly" "${HELPER_IMAGE}" \
    -c "unsquashfs -l /w/nvidia.raw.first" 2>/dev/null | sed 's#^squashfs-root/##' > "${WORK}/list.first"
run_build 555
copy_out /out/nvidia.raw "${WORK}/nvidia.raw"
seed_artifact nvidia.raw
docker run --rm --platform "${PLATFORM}" --entrypoint /bin/sh \
    --mount "type=volume,src=${ARTIFACT_VOL},dst=/w,readonly" "${HELPER_IMAGE}" \
    -c "unsquashfs -l /w/nvidia.raw" 2>/dev/null | sed 's#^squashfs-root/##' > "${WORK}/list.second"
check "identical path listings" cmp -s "${WORK}/list.first" "${WORK}/list.second"

echo "== capture: find fails after a partial list =="
run_build 555 "" 12 fail-find
check "find-fail exit code is not 0" test "${LAST_RC}" -ne 0
check "find-fail reason unexpected:<line>" bash -c "grep -Eq '^NVM:FAIL:unexpected:[0-9]+$' '${LAST_LOG}'"
check "find-fail stopped before the packaging step" bash -c "! grep -q '^NVM:STEP:package' '${LAST_LOG}'"
if docker cp "${LAST_NAME}:/out/nvidia.raw" "${WORK}/find-fail.raw" >/dev/null 2>&1; then
    fail "find-fail: nvidia.raw must not be produced"
else
    pass "find-fail: nvidia.raw was not produced"
fi
if docker cp "${LAST_NAME}:/out/result.txt" "${WORK}/find-fail-result.txt" >/dev/null 2>&1; then
    fail "find-fail: result.txt must not be produced"
else
    pass "find-fail: result.txt was not produced"
fi

echo "== failure reasons =="
run_build 555 "" 12 fail-deb-chmod
check "toolkit chmod failure: exit code 1" test "${LAST_RC}" -eq 1
check "toolkit chmod failure: deb_extract_failed" log_has "NVM:FAIL:deb_extract_failed:nvidia-container-toolkit"
check "toolkit chmod failure: stopped before packaging" bash -c "! grep -q '^NVM:STEP:package' '${LAST_LOG}'"
if copy_out /out/nvidia.raw "${WORK}/chmod-fail.raw"; then
    fail "toolkit chmod failure: nvidia.raw must not be produced"
else
    pass "toolkit chmod failure: nvidia.raw was not produced"
fi
if copy_out /out/result.txt "${WORK}/chmod-fail-result.txt"; then
    fail "toolkit chmod failure: result.txt must not be produced"
else
    pass "toolkit chmod failure: result.txt was not produced"
fi

run_build no-options
check "no-options exit code 1" test "${LAST_RC}" -eq 1
check "no-options reason options_unreadable" log_has "NVM:FAIL:options_unreadable"

run_build install-fail
check "install-fail exit code 1" test "${LAST_RC}" -eq 1
check "install-fail reason installer_failed" log_has "NVM:FAIL:installer_failed"
check "install-fail stderr has the tail of the installer log" log_has "fake installer log line 70"
check "install-fail stderr does not have the head of the installer log" bash -c "! grep -q 'fake installer log line 1\$' '${LAST_LOG}'"

run_build install-fail-kernel
check "install-fail-kernel exit code 1" test "${LAST_RC}" -eq 1
check "install-fail-kernel reason installer_kernel_module_failed" log_has "NVM:FAIL:installer_kernel_module_failed"

run_build 555 "" 99
check "gcc-99 exit code 1" test "${LAST_RC}" -eq 1
check "gcc-99 reason compiler_missing:gcc-99" log_has "NVM:FAIL:compiler_missing:gcc-99"

finish_tests "build script tests: ${FAILURES} assertion(s) failed" \
    "build script tests: all assertions passed"
