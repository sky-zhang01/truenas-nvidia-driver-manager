#!/usr/bin/env bash
# The build entrypoint embedded in the manager executable.
#
# Runs without arguments: the kernel name is read with uname -r and the target
# version is checked by the runner against the artifact; when the plan carries a
# kernel module type, the runner passes it through NVM_KERNEL_MODULE_TYPE.
# The runner sets NVM_COMMUNITY_PATCH=470.256.02 only for the exact official
# version on an actual 6.12 kernel and supplies the fixed Debian archive in /in.
#
# Lines this script writes itself use only the three machine-readable forms
# below; every other line is the verbatim output of an external command.
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
KERNEL_CONFIG="/boot/config-${KERNEL}"
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
source_sha() { # source_sha <relative kernel path> <sha256>
    local file="${PROBE_DIR}/kernel/$1"
    [ -f "${file}" ] && [ ! -L "${file}" ] \
        && [ "$(sha256sum "${file}" | cut -d' ' -f1)" = "$2" ]
}
community_inputs() {
    [ "${NVM_COMMUNITY_PATCH}" = 470.256.02 ] \
        && [[ "${KERNEL}" =~ ^6[.]12[.][0-9] ]] \
        && [ -z "${NVM_KERNEL_MODULE_TYPE:-}" ] \
        && fixed_file "${DRIVER_RUN}" 223168377 \
            fe8f58732055dacc4af0c4bb2371022d6e116e9f9594d7d3bea71f5a8a29e2b1 \
        && fixed_file "${COMMUNITY_ARCHIVE}" 225888 \
            3aeddf4e778ef10535de28dbfa613b24bcf334767c0ac6473193b840ec48d973
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
cc_major=""
for cfg in "${HOST_HEADERS}/include/config/auto.conf" "${KERNEL_CONFIG}" ; do
    [ -f "${cfg}" ] || continue
    line="$(grep -m1 '^CONFIG_CC_VERSION_TEXT=' "${cfg}" 2>/dev/null || true)"
    [ -n "${line}" ] || continue
    value="${line#CONFIG_CC_VERSION_TEXT=}"
    value="${value%\"}"
    value="${value#\"}"
    # Require gcc in the kernel compiler string and use its final dotted version.
    case "${value,,}" in
        *gcc*) ;;
        *) continue ;;
    esac
    rest="${value}"
    major=""
    while [[ "${rest}" =~ ([0-9]+)\.[0-9]+\.[0-9]+ ]]; do
        major="${BASH_REMATCH[1]}"
        rest="${rest#*"${BASH_REMATCH[0]}"}"
    done
    if [ -n "${major}" ]; then
        cc_major="${major}"
        break
    fi
done
[ -n "${cc_major}" ] || fail kernel_gcc_unknown
command -v "gcc-${cc_major}" >/dev/null 2>&1 || fail "compiler_missing:gcc-${cc_major}"
export CC="gcc-${cc_major}" HOSTCC="gcc-${cc_major}" IGNORE_CC_MISMATCH=1
info "cc=gcc-${cc_major}"

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

    # The source hashes bind the patch input to this one official installer.
    source_paths=(
        conftest.sh
        nvidia/nvidia.Kbuild
        nvidia/os-mlock.c
        nvidia-drm/nvidia-drm-drv.c
        nvidia-drm/nvidia-drm.Kbuild
        nvidia/nv.c
        nvidia-modeset/nvidia-modeset-linux.c
    )
    source_before=(
        7aca4b60efbae7cc24f54f62bb2e10eed42bc1104578dbdfb12d0aeeebc981b7
        8656a2a673022e9a91255e6ee809278306c531805ce0b1967b363ceef04a92d2
        de34036fe20a190d013a6b1adbc83bf4c57f496ab72443d144431ccb9f5d2ecc
        e1ade5185cdf612165151b13b399b6c7f0b0cb8c6355c8f61cdea0f0303d6349
        c844df17b5b7105eae4a056ad889d59fd4db6e8f205705398b88cb0ad3a19839
        a1c2f2693d442c02f2a865b8826aaa6fdfb680e42198c3c6f4a801ff1a72fdad
        021d5383b551977654dfe64989f23684d271b4e9e9946b712954bb7a56dcd127
    )
    source_after=(
        e66e3f1181498adeb10893152b83be57359cf95977ed5aef10092ceb20289418
        2a09e8de7d7a39bdf05ef7ab3c8cad89c6278d1fbeb3416b004b95b300b55285
        8f32ef6e5fa5b070fa84f3d1367229e262c934923991e9892c0ea057b0b2dce2
        b818030f98e5c0ce518f7bdc555e65addde12fcdb6fbf280235ffe2d8030fc29
        3a733181aea7c11f8a884ed67dfb7db692447ad2cfc545c12b099af7ee4ecf2c
        20cec097b20400b7673008cd156c5898868e9524d8cc5345e5defb3e1ba0a076
        db63758d02962019f151c26340be11c724fb90300160443741c01df60f526805
    )
    source_failure=(0055 0043 0043 0047 0055 0033 0033)
    for i in "${!source_paths[@]}"; do
        source_sha "${source_paths[$i]}" "${source_before[$i]}" \
            || fail community_patch_failed:archive
    done

    # Only four exact regular archive members are emitted to fixed local files.
    # The Debian tree and its maintainer scripts are never extracted or run.
    patch_ids=(0033 0043 0047 0055)
    patch_members=(
        debian/module/debian/patches/0033-refuse-to-load-legacy-module-if-IBT-is-enabled.patch
        debian/module/debian/patches/0043-backport-follow_pfn-changes-from-550.90.07.patch
        debian/module/debian/patches/0047-backport-drm_output_poll_changed-changes-from-535.21.patch
        debian/module/debian/patches/0055-backport-file_operations_fop_unsigned_offset_present.patch
    )
    patch_bytes=(2235 2891 3324 2331)
    patch_hashes=(
        8f63eeaeab9de29d5ed578a6613d39d11752b2214c0f31e608227d2cf620bc4d
        c1f43733ca9890b31cd511c7b9af74fdc80c6608384dda0c88e95dde31932237
        78b44db0e978d0f398a7b3f9ebf0341550a2339a29fe264570bfb4cb5f22047b
        f6c730b3127ffb1003976a574c9bd937fba3cda7ae31448147a0d508d4f9cd58
    )
    patch_dir="$(mktemp -d /tmp/nvm-community.XXXXXXXX)" \
        || fail community_patch_failed:archive
    tar -tJf "${COMMUNITY_ARCHIVE}" > "${patch_dir}/members" \
        || fail community_patch_failed:archive
    for i in "${!patch_ids[@]}"; do
        member="${patch_members[$i]}"
        # The literal member has no absolute/parent/alias segment. Confirm the
        # archive contains it once and advertises a plain file, not a link.
        [ "$(grep -cFx -- "${member}" "${patch_dir}/members" || true)" = 1 ] \
            || fail community_patch_failed:archive
        metadata="$(tar -tvJf "${COMMUNITY_ARCHIVE}" "${member}")" \
            || fail community_patch_failed:archive
        [[ "${metadata}" = -* && "${metadata}" != *$'\n'* \
            && "${metadata}" = *" ${member}" ]] \
            || fail community_patch_failed:archive
        [ "$(awk '{print $3}' <<< "${metadata}")" = "${patch_bytes[$i]}" ] \
            || fail community_patch_failed:archive
        patch_file="${patch_dir}/${patch_ids[$i]}.patch"
        tar -xJOf "${COMMUNITY_ARCHIVE}" "${member}" > "${patch_file}" \
            || fail community_patch_failed:archive
        fixed_file "${patch_file}" "${patch_bytes[$i]}" "${patch_hashes[$i]}" \
            || fail community_patch_failed:archive
    done
    for i in "${!patch_ids[@]}"; do
        if patch -d "${PROBE_DIR}/kernel" -p1 --batch --forward --fuzz=0 \
            -i "${patch_dir}/${patch_ids[$i]}.patch"; then
            :
        else
            rc=$?
            fail "community_patch_failed:${patch_ids[$i]}" "${rc}"
        fi
    done
    for i in "${!source_paths[@]}"; do
        source_sha "${source_paths[$i]}" "${source_after[$i]}" \
            || fail "community_patch_failed:${source_failure[$i]}"
    done
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
