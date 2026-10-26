#!/usr/bin/env bash
# The build entrypoint embedded in the manager executable.
#
# Runs without arguments: the kernel name is read with uname -r and the target
# version is checked by the runner against the artifact; when the plan carries a
# kernel module type, the runner passes it through NVM_KERNEL_MODULE_TYPE.
# The runner sets NVM_COMMUNITY_PATCH=470.256.02 only for the exact official
# version on an actual 6.12 or 6.18 kernel and supplies the fixed Debian
# 470.256.02-10 archive in /in.
#
# Lines this script writes itself use only the three machine-readable forms
# below; other lines are diagnostic logs or verbatim external-command output.
#   NVM:STEP:<step>
#   NVM:INFO:<key>=<value>   Keys: cc, install_mode, flag_unsupported.
#   NVM:FAIL:<reason>        Reasons are parsed by the runner.
set -Eeuo pipefail
umask 022

KERNEL="$(uname -r)"
DRIVER_RUN=/in/driver.run
COMMUNITY_ARCHIVE=/in/community-patches.debian.tar.xz
TOOLKIT_DIR=/in/toolkit
HOST_HEADERS=/usr/src/linux-headers-truenas-production-amd64
MODULES_UNDER="/usr/lib/modules/${KERNEL}"
STAGE=/tmp/nvm-stage
MARK=/tmp/nvm-mark
PROBE_DIR=/tmp/nvm-probe
OPTIONS_RAW=/tmp/nvm-options.raw
OPTIONS_LIST=/tmp/nvm-options.list
CAPTURE_LIST=/tmp/nvm-capture.list
CAPTURE_RAW=/tmp/nvm-capture.raw
INSTALLER_LOG=/var/log/nvidia-installer.log

step() { printf 'NVM:STEP:%s\n' "$1"; }
info() { printf 'NVM:INFO:%s\n' "$1"; }
fail() {
    printf 'NVM:FAIL:%s\n' "$1"
    exit "${2:-1}"
}
fixed_file() { # fixed_file <path> <bytes> <sha256>
    [ -f "$1" ] && [ ! -L "$1" ] \
        && [ "$(stat -c %s "$1" 2>/dev/null)" = "$2" ] \
        && [ "$(sha256sum "$1" | cut -d' ' -f1)" = "$3" ]
}
kernel_digest() { # hash every regular file in the kernel tree in byte-sorted path order
    local dir="${PROBE_DIR}/kernel" odd
    [ -d "${dir}" ] && [ ! -L "${dir}" ] || return 1
    odd="$(cd "${dir}" && find . -mindepth 1 ! -type f ! -type d -print -quit)" \
        || return 1
    [ -z "${odd}" ] || return 1
    (cd "${dir}" && find . -type f -print0 | LC_ALL=C sort -z \
        | xargs -0 sha256sum | sha256sum | cut -d' ' -f1)
}
community_member() { # community_member <literal archive member> <fixed local output>
    tar -xJOf "${COMMUNITY_ARCHIVE}" "$1" > "$2"
}
community_inputs() {
    [ "${NVM_COMMUNITY_PATCH}" = 470.256.02 ] \
        && [[ "${KERNEL}" =~ ^6[.](12|18)[.][0-9] ]] \
        && [ -z "${NVM_KERNEL_MODULE_TYPE:-}" ] \
        && fixed_file "${DRIVER_RUN}" 223168377 \
            fe8f58732055dacc4af0c4bb2371022d6e116e9f9594d7d3bea71f5a8a29e2b1 \
        && fixed_file "${COMMUNITY_ARCHIVE}" 245240 \
            c05d60926d5fc604f06384e881db9348c067018a8ba639562e03329d7f55ca19
}
# The build container runs code from a user-supplied installer package, so its
# output is external input: any unexpected failure becomes unexpected:<line>.
trap 'printf "NVM:FAIL:unexpected:%s\n" "$LINENO"; exit 1' ERR

step check
sh "${DRIVER_RUN}" --check || fail run_check_failed
# Verify both immutable qualified inputs before executing the extracted
# installer for option probing; --check remains the first build action.
if [ "${NVM_COMMUNITY_PATCH+x}" = x ]; then
    community_inputs || fail community_patch_failed:archive
fi

step modules
mkdir -p /usr/lib/modules
cp -a "/host/modules/${KERNEL}" "${MODULES_UNDER}" || fail modules_copy_failed
# Preserve modules.builtin, modules.builtin.modinfo and modules.order.
for rel in \
    kernel/drivers/video/nvidia.ko \
    kernel/drivers/video/nvidia-uvm.ko \
    kernel/drivers/video/nvidia-modeset.ko \
    kernel/drivers/video/nvidia-drm.ko \
    kernel/drivers/video/nvidia-peermem.ko \
    modules.alias \
    modules.alias.bin \
    modules.builtin.alias.bin \
    modules.builtin.bin \
    modules.dep \
    modules.dep.bin \
    modules.devname \
    modules.softdep \
    modules.symbols \
    modules.symbols.bin ; do
    rm -f "${MODULES_UNDER}/${rel}"
done

step compiler
# The host runner selected this before any downloads; do not parse kernel config here.
[ -n "${NVM_BUILD_COMPILER:-}" ] || fail kernel_gcc_unknown
command -v "${NVM_BUILD_COMPILER}" >/dev/null 2>&1 || fail "compiler_missing:${NVM_BUILD_COMPILER}"
export CC="${NVM_BUILD_COMPILER}" HOSTCC="${NVM_BUILD_COMPILER}" IGNORE_CC_MISMATCH=1
info "cc=${CC}"

step probe
sh "${DRIVER_RUN}" --extract-only --target "${PROBE_DIR}" || fail extract_failed
[ -x "${PROBE_DIR}/nvidia-installer" ] || fail extract_failed
"${PROBE_DIR}/nvidia-installer" --advanced-options-args-only > "${OPTIONS_RAW}" \
    || fail options_unreadable
[ -s "${OPTIONS_RAW}" ] || fail options_unreadable

# Match complete option names, excluding any value suffix after =.
: > "${OPTIONS_LIST}"
while IFS= read -r line; do
    case "${line}" in
        "  -"*) ;;
        *) continue ;;
    esac
    IFS=',' read -r -a parts <<< "${line:2}"
    for part in "${parts[@]}"; do
        name="${part//[[:space:]]/}"
        case "${name}" in
            --*) ;;
            *) continue ;;
        esac
        printf '%s\n' "${name%%=*}" >> "${OPTIONS_LIST}"
    done
done < "${OPTIONS_RAW}"

# The ordinary installer still extracts itself for each install pass. The
# qualified route retains this checked tree because make must compile the
# patched kernel sources, not a second extraction of the original .run.
if [ "${NVM_COMMUNITY_PATCH+x}" != x ]; then
    rm -rf "${PROBE_DIR}"
else
    step community_patch
    community_inputs || fail community_patch_failed:archive

    # Extract only the fixed series and its named members. No other
    # Debian files or maintainer scripts are extracted or run.
    patch_dir="$(mktemp -d /tmp/nvm-community.XXXXXXXX)" \
        || fail community_patch_failed:archive
    community_member debian/module/debian/patches/series.in "${patch_dir}/series.in" \
        || fail community_patch_failed:archive
    patch_names=()
    while IFS= read -r name || [ -n "${name}" ]; do
        case "${name}" in ''|'#'*) continue ;; esac
        patch_names+=("${name}")
    done < "${patch_dir}/series.in"
    # Stage the entire series before applying its first patch.
    for i in "${!patch_names[@]}"; do
        printf -v ordinal '%02d' "$((i + 1))"
        community_member "debian/module/debian/patches/${patch_names[$i]}" \
            "${patch_dir}/${ordinal}.patch" || fail community_patch_failed:archive
    done
    for i in "${!patch_names[@]}"; do
        printf -v ordinal '%02d' "$((i + 1))"
        printf 'Applying community patch %s: %s\n' "${ordinal}" "${patch_names[$i]}"
        if patch -d "${PROBE_DIR}/kernel" -p1 --batch --forward --fuzz=0 \
            --no-backup-if-mismatch -i "${patch_dir}/${ordinal}.patch"; then
            :
        else
            rc=$?
            fail "community_patch_failed:${ordinal}" "${rc}"
        fi
    done
    tree_after="$(kernel_digest)" \
        && [ "${tree_after}" = 91b69e7d074e79c7997cf78f06c717ffd188217b183c2e8eefef4b5b91a53ea7 ] \
        || fail community_patch_failed:archive

fi

supports() { grep -qxF -- "$1" "${OPTIONS_LIST}"; }

# These flags are available in every supported installer branch.
install_args=(--silent "--kernel-name=${KERNEL}" "--kernel-source-path=${HOST_HEADERS}")
# Pass optional flags only when advertised; report each unsupported flag.
probed=(
    --no-x-check
    --no-nouveau-check
    --no-backup
    --no-systemd
    --install-libglvnd
    --skip-module-load
    --no-rebuild-initramfs
    --allow-installation-with-running-driver
)
for flag in "${probed[@]}"; do
    if supports "${flag}"; then
        install_args+=("${flag}")
    else
        info "flag_unsupported=${flag}"
    fi
done

# Without the running-driver flag, userspace and modules require separate passes.
if [ "${NVM_COMMUNITY_PATCH+x}" = x ]; then
    install_mode=userspace-make
elif supports --allow-installation-with-running-driver; then
    install_mode=single
else
    install_mode=two-pass
fi
info "install_mode=${install_mode}"

# A requested module type requires installer support; never silently omit the plan choice.
if [ -n "${NVM_KERNEL_MODULE_TYPE:-}" ]; then
    supports --kernel-module-type || fail kernel_module_type_unsupported
    install_args+=("--kernel-module-type=${NVM_KERNEL_MODULE_TYPE}")
fi

dump_installer_log() {
    if [ -f "${INSTALLER_LOG}" ]; then
        tail -n 60 "${INSTALLER_LOG}" >&2 || true
    fi
}

# ── install / install_userspace + install_kernel_module ──────────────────────
# Capture files whose ctime changes after this marker during installation or depmod.
touch "${MARK}"
sleep 1

if [ "${install_mode}" = single ]; then
    step install
    if ! sh "${DRIVER_RUN}" "${install_args[@]}"; then
        dump_installer_log
        fail installer_failed
    fi
else
    # --kernel-module-only requires userspace to be installed first.
    step install_userspace
    if ! sh "${DRIVER_RUN}" "${install_args[@]}" --no-kernel-module; then
        dump_installer_log
        fail installer_userspace_failed
    fi
    step install_kernel_module
    if [ "${install_mode}" = userspace-make ]; then
        jobs="$(nproc)" || fail kernel_module_build_failed
        [[ "${jobs}" =~ ^[1-9][0-9]*$ ]] || fail kernel_module_build_failed
        if make -C "${PROBE_DIR}/kernel" -j"${jobs}" \
            "SYSSRC=${HOST_HEADERS}" "SYSOUT=${HOST_HEADERS}" \
            "KERNEL_UNAME=${KERNEL}" ARCH=x86_64 "CC=${CC}" "HOSTCC=${HOSTCC}" \
            'NV_KERNEL_MODULES=nvidia nvidia-uvm nvidia-modeset nvidia-drm nvidia-peermem' \
            NV_VERBOSE=1 modules; then
            :
        else
            rc=$?
            fail kernel_module_build_failed "${rc}"
        fi
        module_files=(nvidia nvidia-uvm nvidia-modeset nvidia-drm nvidia-peermem)
        module_ids=(nvidia nvidia_uvm nvidia_modeset nvidia_drm nvidia_peermem)
        for i in "${!module_files[@]}"; do
            ko="${PROBE_DIR}/kernel/${module_files[$i]}.ko"
            [ -f "${ko}" ] && [ ! -L "${ko}" ] && [ -s "${ko}" ] \
                || fail "kernel_module_missing:${module_ids[$i]}"
        done
        module_dest="${MODULES_UNDER}/kernel/drivers/video"
        mkdir -p "${module_dest}" || fail kernel_module_install_failed:nvidia
        for i in "${!module_files[@]}"; do
            if install -m0644 "${PROBE_DIR}/kernel/${module_files[$i]}.ko" \
                "${module_dest}/${module_files[$i]}.ko"; then
                :
            else
                rc=$?
                fail "kernel_module_install_failed:${module_ids[$i]}" "${rc}"
            fi
        done
    elif ! sh "${DRIVER_RUN}" "${install_args[@]}" --kernel-module-only; then
        dump_installer_log
        fail installer_kernel_module_failed
    fi
fi

step depmod
depmod -a "${KERNEL}" || fail depmod_failed

step capture
mkdir -p "${STAGE}"
# Capture /usr except /usr/src, matching the host sysext capture scope.
# The list is written to a file first: inside a process substitution a find that
# emits some entries and then fails would lose its status, and a partial list
# must never be packaged. A find failure trips the ERR trap (`unexpected:<line>`) before any artifact is produced.
find /usr -xdev \( -type f -o -type l \) -cnewer "${MARK}" -print0 > "${CAPTURE_RAW}"
while IFS= read -r -d '' path; do
    case "${path}" in
        /usr/src/*) continue ;;
    esac
    printf '%s\0' "${path#/}"
done < "${CAPTURE_RAW}" \
    | LC_ALL=C sort -z > "${CAPTURE_LIST}"
[ -s "${CAPTURE_LIST}" ] || fail capture_empty
tar -C / --null --no-recursion -T "${CAPTURE_LIST}" -cpf - \
    | tar -C "${STAGE}" -xpf - || fail stage_copy_failed

step toolkit
for pkg in \
    libnvidia-container1 \
    libnvidia-container-tools \
    nvidia-container-toolkit-base \
    nvidia-container-toolkit ; do
    rm -rf "/tmp/nvm-deb/${pkg}" || fail "deb_extract_failed:${pkg}"
    mkdir -p "/tmp/nvm-deb/${pkg}" || fail "deb_extract_failed:${pkg}"
    dpkg-deb -x "${TOOLKIT_DIR}/${pkg}.deb" "/tmp/nvm-deb/${pkg}" || fail "deb_extract_failed:${pkg}"
    # Official 1.20.1 sets ./ to 0644; restore only its owner's search bit.
    [ -d "/tmp/nvm-deb/${pkg}" ] && [ ! -L "/tmp/nvm-deb/${pkg}" ] \
        || fail "deb_extract_failed:${pkg}"
    chmod u+x "/tmp/nvm-deb/${pkg}" || fail "deb_extract_failed:${pkg}"
    [ -r "/tmp/nvm-deb/${pkg}" ] && [ -x "/tmp/nvm-deb/${pkg}" ] \
        || fail "deb_extract_failed:${pkg}"
    if [ -e "/tmp/nvm-deb/${pkg}/usr" ] || [ -L "/tmp/nvm-deb/${pkg}/usr" ]; then
        [ -d "/tmp/nvm-deb/${pkg}/usr" ] && [ ! -L "/tmp/nvm-deb/${pkg}/usr" ] \
            && [ -r "/tmp/nvm-deb/${pkg}/usr" ] && [ -x "/tmp/nvm-deb/${pkg}/usr" ] \
            || fail "deb_extract_failed:${pkg}"
        mkdir -p "${STAGE}/usr" || fail "deb_extract_failed:${pkg}"
        cp -a "/tmp/nvm-deb/${pkg}/usr/." "${STAGE}/usr/" || fail "deb_extract_failed:${pkg}"
    fi
    if [ -e "/tmp/nvm-deb/${pkg}/lib" ] || [ -L "/tmp/nvm-deb/${pkg}/lib" ]; then
        [ -d "/tmp/nvm-deb/${pkg}/lib" ] && [ ! -L "/tmp/nvm-deb/${pkg}/lib" ] \
            && [ -r "/tmp/nvm-deb/${pkg}/lib" ] && [ -x "/tmp/nvm-deb/${pkg}/lib" ] \
            || fail "deb_extract_failed:${pkg}"
        mkdir -p "${STAGE}/usr/lib" || fail "deb_extract_failed:${pkg}"
        cp -a "/tmp/nvm-deb/${pkg}/lib/." "${STAGE}/usr/lib/" || fail "deb_extract_failed:${pkg}"
    fi
done

step release
mkdir -p "${STAGE}/usr/lib/extension-release.d" \
    && printf 'ID=_any\n' > "${STAGE}/usr/lib/extension-release.d/extension-release.nvidia" \
    && chmod 0644 "${STAGE}/usr/lib/extension-release.d/extension-release.nvidia" \
    || fail release_write_failed

step package
mksquashfs "${STAGE}" /out/nvidia.raw -comp xz -all-root -noappend -no-progress \
    || fail mksquashfs_failed

step result
artifact_sha256="$(sha256sum /out/nvidia.raw | cut -d' ' -f1)" || fail result_write_failed
artifact_bytes="$(stat -c %s /out/nvidia.raw)" || fail result_write_failed
{
    printf 'ARTIFACT_SHA256=%s\n' "${artifact_sha256}"
    printf 'ARTIFACT_BYTES=%s\n' "${artifact_bytes}"
} > /out/result.txt || fail result_write_failed
