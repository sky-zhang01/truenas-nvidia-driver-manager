#!/usr/bin/env bash
# make-fake-run.sh — fabricate the fake build inputs.
#
#   make-fake-run.sh run  <variant> <out.run>   Create a fake `.run`.
#   make-fake-run.sh debs <outdir>              Create four fake toolkit packages.
#
# Variants:
#   555                  Both running-driver and module-type flags; one pass.
#   545                  Running-driver flag without module-type; one pass.
#   470                  Neither flag; two installation passes.
#   check-fail           `.run --check` exits with status 1.
#   no-options           Option-list command exits with status 1.
#   install-fail         Installation exits with a nonzero status.
#   install-fail-kernel  Only the kernel-module pass fails.
#
# Every option list carries --print-recommended-kernel-module-type so the
# whole-name matching of the probe is exercised. No NVIDIA code or data is used:
# the payload is synthesized here.
set -Eeuo pipefail
umask 022

# Temp dirs to remove at exit. An EXIT trap, not a RETURN trap: the latter also
# fires when a nested function returns and would delete a directory still in use.
cleanup_dirs=()
cleanup() { rm -rf ${cleanup_dirs[@]+"${cleanup_dirs[@]}"}; }
trap cleanup EXIT
# Create the temp dir in the caller's shell and store the path in the named
# variable. Never call this as `x="$(new_tmp)"`: the command substitution runs
# in a subshell, the parent EXIT trap cannot see the registration, and the
# directory leaks. The explicit TMPDIR template lets tests scope the dirs.
new_tmp() { # new_tmp <variable name>
    local var="$1" d
    d="$(mktemp -d "${TMPDIR:-/tmp}/nvm-fake-run.XXXXXXXX")"
    cleanup_dirs+=("${d}")
    printf -v "${var}" '%s' "${d}"
}

installer_body() {
    cat <<'INSTALLER_BODY'
#!/bin/sh
# Fake nvidia-installer: does only what the make-fake-run.sh variant dictates.
set -eu
OPTIONS_MODE=__OPTIONS_MODE__
OPTIONS_FAIL=__OPTIONS_FAIL__
INSTALL_FAIL=__INSTALL_FAIL__
FAIL_KERNEL_ONLY=__FAIL_KERNEL_ONLY__

version=999.0.0
kernel="$(uname -r)"
for a in "$@"; do
    case "$a" in
        --kernel-name=*) kernel="${a#--kernel-name=}" ;;
    esac
done

# The installer log is written first: on failure the build script dumps its last
# 60 lines to stderr.
mkdir -p /var/log
i=1
while [ "${i}" -le 70 ]; do
    printf 'fake installer log line %s\n' "${i}"
    i=$((i + 1))
done > /var/log/nvidia-installer.log

print_options() {
    printf '  --silent\n      Do not ask any questions.\n'
    printf '  --kernel-name=KERNEL_NAME, --no-kernel-name\n      Build the kernel module for a kernel name.\n'
    printf '  --advanced-options-args-only\n      Print the advanced options and exit.\n'
    printf '  --no-x-check\n      Skip the X server check.\n'
    printf '  --no-nouveau-check\n      Skip the nouveau check.\n'
    printf '  --no-backup, --no-no-backup\n      Do not back up conflicting files.\n'
    printf '  --systemd, --no-systemd\n      Install systemd units or not.\n'
    printf '  --install-libglvnd, --no-install-libglvnd\n      Install GLVND libraries or not.\n'
    printf '  --print-recommended-kernel-module-type\n      Print the recommended kernel module type and exit.\n'
    case "$OPTIONS_MODE" in
        555)
            printf '  --allow-installation-with-running-driver, --no-allow-installation-with-running-driver\n      Tolerate a running driver.\n'
            printf '  --kernel-module-type=KERNEL_MODULE_TYPE\n      Set the kernel module type. Valid values are "open" and "proprietary".\n'
            printf '  --skip-module-load, --no-skip-module-load\n      Do not try to load the new modules.\n'
            printf '  --rebuild-initramfs, --no-rebuild-initramfs\n      Rebuild the initramfs or not.\n'
            ;;
        545)
            printf '  --allow-installation-with-running-driver, --no-allow-installation-with-running-driver\n      Tolerate a running driver.\n'
            printf '  --rebuild-initramfs, --no-rebuild-initramfs\n      Rebuild the initramfs or not.\n'
            ;;
        470)
            ;;
    esac
}

for a in "$@"; do
    if [ "$a" = "--advanced-options-args-only" ]; then
        if [ "$OPTIONS_FAIL" = 1 ]; then
            echo "fake nvidia-installer: options unreadable" >&2
            exit 1
        fi
        print_options
        exit 0
    fi
done

if [ "$FAIL_KERNEL_ONLY" = 1 ]; then
    for a in "$@"; do
        if [ "$a" = "--kernel-module-only" ]; then
            echo "fake nvidia-installer: kernel-module pass failed" >&2
            exit 1
        fi
    done
fi
if [ "$INSTALL_FAIL" = 1 ]; then
    echo "fake nvidia-installer: install failed" >&2
    exit 1
fi

mkdir -p /usr/lib /usr/bin "/usr/src/nvidia-${version}" /etc/vulkan/icd.d
printf '# fake lib\n' > /usr/lib/libnvm-fake.so.1
printf '#!/bin/sh\necho fake nvidia-smi\n' > /usr/bin/nvidia-smi
chmod 0755 /usr/bin/nvidia-smi
printf '/* fake */\n' > "/usr/src/nvidia-${version}/nvidia.c"
printf '{}\n' > /etc/vulkan/icd.d/nvm_fake.json

# Use the container kernel release for vermagic so depmod accepts the modules.
moddir="/usr/lib/modules/${kernel}/kernel/drivers/video"
mkdir -p "${moddir}"
src=/tmp/nvm-fake-module.c
{
    printf 'const char nvm_fake_vermagic[] __attribute__((section(".modinfo"), used)) = "vermagic=%s SMP mod_unload";\n' "${kernel}"
    printf 'const char nvm_fake_license[] __attribute__((section(".modinfo"), used)) = "license=MIT";\n'
} > "${src}"
for m in nvidia nvidia-uvm nvidia-modeset nvidia-drm nvidia-peermem; do
    gcc-12 -c -o "${moddir}/${m}.ko" "${src}"
done
rm -f "${src}"

mkdir -p /usr/share/nvm-fake-installer
n=1
while [ -f "/usr/share/nvm-fake-installer/argv.${n}" ]; do n=$((n + 1)); done
{
    printf 'cwd=%s\n' "${PWD}"
    i=1
    for a in "$@"; do
        printf 'argv[%d]=%s\n' "${i}" "${a}"
        i=$((i + 1))
    done
} > "/usr/share/nvm-fake-installer/argv.${n}"
exit 0
INSTALLER_BODY
}

write_installer() {
    local out="$1" mode="$2" options_fail="$3" install_fail="$4" fail_kernel_only="$5"
    installer_body > "${out}"
    sed -i.bak \
        -e "s/__OPTIONS_MODE__/${mode}/" \
        -e "s/__OPTIONS_FAIL__/${options_fail}/" \
        -e "s/__INSTALL_FAIL__/${install_fail}/" \
        -e "s/__FAIL_KERNEL_ONLY__/${fail_kernel_only}/" \
        "${out}"
    rm -f "${out}.bak"
    chmod 0755 "${out}"
}

write_run() {
    local variant="$1" out="$2"
    local mode options_fail install_fail fail_kernel_only check_ok
    case "${variant}" in
        555)                 mode=555; options_fail=0; install_fail=0; fail_kernel_only=0; check_ok=1 ;;
        545)                 mode=545; options_fail=0; install_fail=0; fail_kernel_only=0; check_ok=1 ;;
        470)                 mode=470; options_fail=0; install_fail=0; fail_kernel_only=0; check_ok=1 ;;
        check-fail)          mode=555; options_fail=0; install_fail=0; fail_kernel_only=0; check_ok=0 ;;
        no-options)          mode=555; options_fail=1; install_fail=0; fail_kernel_only=0; check_ok=1 ;;
        install-fail)        mode=555; options_fail=0; install_fail=1; fail_kernel_only=0; check_ok=1 ;;
        install-fail-kernel) mode=470; options_fail=0; install_fail=0; fail_kernel_only=1; check_ok=1 ;;
        *) echo "make-fake-run: unknown variant '${variant}'" >&2; exit 2 ;;
    esac

    local tmp installer
    new_tmp tmp
    installer="${tmp}/nvidia-installer"
    write_installer "${installer}" "${mode}" "${options_fail}" "${install_fail}" "${fail_kernel_only}"

    mkdir -p "$(dirname "${out}")"
    {
        printf '#!/bin/sh\n'
        printf '# Fake NVIDIA installer fixture.\n'
        printf 'set -eu\n'
        printf 'CALLS_LOG="${NVM_FAKE_CALLS_LOG:-/usr/share/nvm-fake-calls.log}"\n'
        printf 'CHECK_OK=%s\n' "${check_ok}"
        cat <<'RUN_HEAD'
record() {
    mkdir -p "$(dirname "${CALLS_LOG}")"
    printf '%s\n' "$1" >> "${CALLS_LOG}"
}
extract_to() {
    target="$1"
    mkdir -p "$target"
    cat > "$target/nvidia-installer" <<'XINSTALLER'
RUN_HEAD
        cat "${installer}"
        cat <<'RUN_TAIL'
XINSTALLER
    chmod 0755 "$target/nvidia-installer"
}

case " $* " in
    *" --check "*)
        record check
        if [ "$CHECK_OK" = 1 ]; then
            echo "fake .run: check ok"
            exit 0
        fi
        echo "fake .run: check failed" >&2
        exit 1
        ;;
esac

if [ "${1:-}" = "--extract-only" ]; then
    record extract-only
    shift
    target=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --target)
                target="${2:-}"
                shift 2
                ;;
            *)
                shift
                ;;
        esac
    done
    [ -n "${target}" ] || { echo "fake .run: --extract-only without --target" >&2; exit 2; }
    extract_to "${target}"
    exit 0
fi

record install
makeself_dir="$(mktemp -d /tmp/makeself.XXXXXXXX)"
extract_to "${makeself_dir}"
# Run the installer from inside its own extracted directory: the fake installer
# records its PWD, and each pass must point at its own makeself directory so the
# test proves the independent extraction directories are actually used.
cd "${makeself_dir}"
"./nvidia-installer" "$@"
RUN_TAIL
    } > "${out}"
    chmod 0755 "${out}"
}

make_debs() {
    local outdir="$1" version="1.20.1-1"
    mkdir -p "${outdir}"
    local build
    new_tmp build

    deb_control() {
        local dir="$1" name="$2"
        mkdir -p "${dir}/DEBIAN"
        {
            printf 'Package: %s\n' "${name}"
            printf 'Version: %s\n' "${version}"
            printf 'Architecture: amd64\n'
            printf 'Maintainer: ci <ci@example.invalid>\n'
            printf 'Description: fake toolkit package for the build-script tests\n'
        } > "${dir}/DEBIAN/control"
    }

    local d
    d="${build}/libnvidia-container1"; deb_control "${d}" libnvidia-container1
    mkdir -p "${d}/usr/lib/x86_64-linux-gnu"
    printf '# fake libnvidia-container\n' > "${d}/usr/lib/x86_64-linux-gnu/libnvidia-container.so.1"
    dpkg-deb --root-owner-group --build "${d}" "${outdir}/libnvidia-container1.deb" >/dev/null

    d="${build}/libnvidia-container-tools"; deb_control "${d}" libnvidia-container-tools
    mkdir -p "${d}/usr/bin"
    printf '#!/bin/sh\necho fake nvidia-container-cli\n' > "${d}/usr/bin/nvidia-container-cli"
    chmod 0755 "${d}/usr/bin/nvidia-container-cli"
    dpkg-deb --root-owner-group --build "${d}" "${outdir}/libnvidia-container-tools.deb" >/dev/null

    d="${build}/nvidia-container-toolkit-base"; deb_control "${d}" nvidia-container-toolkit-base
    mkdir -p "${d}/usr/bin" "${d}/lib/systemd/system"
    printf '#!/bin/sh\necho fake nvidia-container-runtime\n' > "${d}/usr/bin/nvidia-container-runtime"
    printf '#!/bin/sh\necho fake nvidia-ctk\n' > "${d}/usr/bin/nvidia-ctk"
    chmod 0755 "${d}/usr/bin/nvidia-container-runtime" "${d}/usr/bin/nvidia-ctk"
    printf '[Path]\nPathExists=/run/nvidia/driver\n' > "${d}/lib/systemd/system/nvidia-cdi-refresh.path"
    printf '[Service]\nExecStart=/usr/bin/nvidia-ctk cdi generate\n' > "${d}/lib/systemd/system/nvidia-cdi-refresh.service"
    dpkg-deb --root-owner-group --build "${d}" "${outdir}/nvidia-container-toolkit-base.deb" >/dev/null

    d="${build}/nvidia-container-toolkit"; deb_control "${d}" nvidia-container-toolkit
    mkdir -p "${d}/usr/bin" "${d}/etc/nvidia-container-toolkit"
    printf '#!/bin/sh\necho fake nvidia-container-runtime-hook\n' > "${d}/usr/bin/nvidia-container-runtime-hook"
    chmod 0755 "${d}/usr/bin/nvidia-container-runtime-hook"
    printf 'NVIDIA_CDI_REFRESH=true\n' > "${d}/etc/nvidia-container-toolkit/nvidia-cdi-refresh.env"
    dpkg-deb --root-owner-group --build "${d}" "${outdir}/nvidia-container-toolkit.deb" >/dev/null
}

main() {
    local cmd="${1:-}"
    case "${cmd}" in
        run)
            [ "$#" -eq 3 ] || { echo "make-fake-run: usage: run <variant> <out.run>" >&2; exit 2; }
            write_run "$2" "$3"
            ;;
        debs)
            [ "$#" -eq 2 ] || { echo "make-fake-run: usage: debs <outdir>" >&2; exit 2; }
            make_debs "$2"
            ;;
        *)
            echo "make-fake-run: usage: $0 {run <variant> <out.run>|debs <outdir>}" >&2
            exit 2
            ;;
    esac
}

main "$@"
