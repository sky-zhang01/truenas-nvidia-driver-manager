#!/usr/bin/env bash
# Exercise the download entry points without network, Docker, or system installs.
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT
. "${repo_root}/ci/test-lib.sh"

mkdir -p "${work}/repo/tools" "${work}/repo/ci" "${work}/repo/src/build_kit" "${work}/bin"
cp "${repo_root}/tools/build-kit.sh" "${work}/repo/tools/"
cp "${repo_root}/ci/osv-scan.sh" "${work}/repo/ci/"
export DOWNLOAD_TEST_ROOT="${work}"
export PATH="${work}/bin:${PATH}"
export OSV_SCANNER_VERSION="test-release"
export NVM_DOCKER="${work}/bin/docker"
printf 'package bytes\n' > "${work}/package"
cat > "${work}/scanner" <<'SCANNER'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${DOWNLOAD_TEST_ROOT}/scan.log"
printf '%s\n' "$0" >> "${DOWNLOAD_TEST_ROOT}/scanner-path.log"
exit "${DOWNLOAD_SCAN_EXIT:-0}"
SCANNER
package_sha="$(sha256sum "${work}/package" | cut -d' ' -f1)"
package_size="$(wc -c < "${work}/package" | tr -d ' ')"
printf '%s\t%s\tpackage.deb\tpackage\t1\thttps://snapshot.debian.org/archive/debian/test/package.deb\n' \
    "${package_sha}" "${package_size}" > "${work}/repo/src/build_kit/packages.tsv"

cat > "${work}/bin/curl" <<'CURL'
#!/usr/bin/env bash
set -Eeuo pipefail
printf 'call\n' >> "${DOWNLOAD_TEST_ROOT}/curl.log"
args=("$@")
has_arg() { local arg; for arg in "${args[@]}"; do [ "${arg}" != "$1" ] || return 0; done; return 1; }
has_pair() { local i; for ((i=0; i + 1 < ${#args[@]}; i++)); do
    if [ "${args[i]}" = "$1" ] && [ "${args[i + 1]}" = "$2" ]; then return 0; fi
done; return 1; }
for pair in '--retry 5' '--retry-delay 3' '--connect-timeout 15' '--max-time 300' "--proto =https"; do
    read -r flag value <<< "${pair}"
    has_pair "${flag}" "${value}" || { echo "missing curl option: ${pair}" >&2; exit 90; }
done
has_arg --retry-connrefused || exit 90
if has_arg --retry-all-errors; then echo 'permanent errors must not be retried' >&2; exit 90; fi
fail_http=false
follow=false
for arg in "${args[@]}"; do
    if [[ "${arg}" == -[^-]* && "${arg}" == *f* ]]; then fail_http=true; fi
    if [[ "${arg}" == --location || ( "${arg}" == -[^-]* && "${arg}" == *L* ) ]]; then follow=true; fi
done
"${fail_http}" && "${follow}" || exit 90
output=''
while [ "$#" -gt 0 ]; do
    case "$1" in -o) output="$2"; shift ;; esac
    shift
done
url="${args[${#args[@]} - 1]}"
[[ "${url}" == "https://"* ]] || exit 90
case "${DOWNLOAD_MODE}" in
    not-found) echo 'HTTP 404' >&2; exit 22 ;;
    sums-not-found) [[ "${url}" != */osv-scanner_SHA256SUMS ]] || { echo 'HTTP 404' >&2; exit 22; } ;;
esac
case "${url}" in
    */package.deb)
        case "${DOWNLOAD_MODE}" in
            bad-size) printf x > "${output}" ;;
            bad-hash) tr 'a' 'b' < "${DOWNLOAD_TEST_ROOT}/package" > "${output}" ;;
            *) cp "${DOWNLOAD_TEST_ROOT}/package" "${output}" ;;
        esac ;;
    */osv-scanner_linux_amd64) cp "${DOWNLOAD_TEST_ROOT}/scanner" "${output}" ;;
    */osv-scanner_SHA256SUMS)
        sha="$(sha256sum "${DOWNLOAD_TEST_ROOT}/scanner" | cut -d' ' -f1)"
        if [ "${DOWNLOAD_MODE}" = bad-hash ]; then sha="$(printf '%064d' 0)"; fi
        printf '%s  osv-scanner_linux_amd64\n' "${sha}" > "${output}" ;;
    *) exit 90 ;;
esac
CURL

cat > "${work}/bin/install" <<'INSTALL'
#!/usr/bin/env bash
set -Eeuo pipefail
[ "$1" = -m ] && [ "$2" = 0755 ] && [ "$3" = osv-scanner_linux_amd64 ] \
    && [ "$4" = "${PWD}/bin/osv-scanner" ]
printf 'install\n' >> "${DOWNLOAD_TEST_ROOT}/install.log"
cp "$3" "$4"
chmod +x "$4"
INSTALL

cat > "${work}/bin/docker" <<'DOCKER'
#!/usr/bin/env bash
set -Eeuo pipefail
case "$1" in
    image) sed -n 's/^IMAGE_ID="\(.*\)"/\1/p' "${DOWNLOAD_TEST_ROOT}/repo/tools/build-kit.sh" ;;
    run)
        for arg in "$@"; do inner="${arg}"; done
        printf '%s\n' "${inner}" > "${DOWNLOAD_TEST_ROOT}/apt-script"
        cat "${DOWNLOAD_TEST_ROOT}/repo/src/build_kit/packages.tsv" ;;
    *) exit 90 ;;
esac
DOCKER
chmod +x "${work}/bin/"*

reset_case() {
    export DOWNLOAD_MODE="$1"
    : > "${work}/curl.log"
    rm -f "${work}/install.log" "${work}/scan.log" "${work}/bin/osv-scanner" "${work}/scanner-path.log"
    printf '#!/usr/bin/env bash\nexit 91\n' > "${work}/bin/osv-scanner"
    chmod +x "${work}/bin/osv-scanner"
}
fetch() { bash "${work}/repo/tools/build-kit.sh" fetch "$1" > "${work}/out" 2> "${work}/err"; }
scan() { bash "${work}/repo/ci/osv-scan.sh" > "${work}/out" 2> "${work}/err"; }
calls_are() { [ "$(wc -l < "${work}/curl.log" | tr -d ' ')" = "$1" ]; }
scanner_removed() {
    [ -s "${work}/scanner-path.log" ] || return 1
    local scanner_path
    scanner_path="$(cat "${work}/scanner-path.log")"
    [ ! -e "${scanner_path}" ]
}

reset_case good
check 'fetch accepts correct bytes with bounded curl retry options' fetch "${work}/debs"
check 'fetch publishes exactly the verified package' cmp "${work}/package" "${work}/debs/package.deb"
reset_case not-found
check 'fetch reuses an already complete package even when download is unavailable' fetch "${work}/debs"
check 'a verified package is reused without curl' calls_are 0
check 'reuse preserves existing verified bytes' cmp "${work}/package" "${work}/debs/package.deb"

for mode in not-found bad-size bad-hash; do
    reset_case "${mode}"
    mkdir -p "${work}/${mode}"
    printf 'existing incomplete package\n' > "${work}/${mode}/package.deb"
    cp "${work}/${mode}/package.deb" "${work}/before"
    if fetch "${work}/${mode}"; then fail "fetch rejects ${mode}"; else pass "fetch rejects ${mode}"; fi
    case "${mode}" in
        not-found) reason='HTTP 404' ;;
        bad-size) reason='got 1 bytes' ;;
        bad-hash) reason='got SHA256' ;;
    esac
    check "fetch ${mode} reaches the intended failure" grep -Fq "${reason}" "${work}/err"
    check "fetch ${mode} has no outer download retry" calls_are 1
    check "fetch ${mode} preserves the existing target" cmp "${work}/before" "${work}/${mode}/package.deb"
done

reset_case good
check 'OSV downloads verify before installing and scanning' scan
check 'OSV downloads both release files' calls_are 2
check 'OSV removes its temporary binary after scanning' scanner_removed
check 'OSV scans both lockfiles' grep -Fxq 'scan source --lockfile=Cargo.lock --lockfile=web/package-lock.json' "${work}/scan.log"
reset_case good
export DOWNLOAD_SCAN_EXIT=17
if scan; then fail 'OSV propagates scanner failures'; else
    scan_status=$?
    check 'OSV preserves the scanner exit status' test "${scan_status}" -eq 17
fi
unset DOWNLOAD_SCAN_EXIT
check 'OSV removes its temporary binary after scan failure' scanner_removed
for mode in not-found sums-not-found bad-hash; do
    reset_case "${mode}"
    if scan; then fail "OSV rejects ${mode}"; else pass "OSV rejects ${mode}"; fi
    if [ "${mode}" = bad-hash ]; then
        check 'OSV rejects a checksum mismatch' grep -Fq 'FAILED' "${work}/out"
    else
        check "OSV ${mode} reaches the HTTP failure" grep -Fq 'HTTP 404' "${work}/err"
    fi
    expected=2
    [ "${mode}" != not-found ] || expected=1
    check "OSV ${mode} has no outer download retry" calls_are "${expected}"
    check "OSV ${mode} never installs unverified bytes" test ! -f "${work}/install.log"
    check "OSV ${mode} never scans with unverified bytes" test ! -f "${work}/scan.log"
done

reset_case good
check 'gen passes its apt setup to the pinned container' bash "${work}/repo/tools/build-kit.sh" gen "${work}/generated.tsv"
# Inspect the actual shell passed to Docker, including placement before apt update.
apt_config="$(sed -n '/cat > \/etc\/apt\/apt.conf.d\/80-download-resilience <<.*$/,/^EOF$/p' "${work}/apt-script")"
for setting in 'Acquire::Retries "5";' 'Acquire::http::Timeout "30";' 'Acquire::https::Timeout "30";'; do
    check "container apt config sets ${setting}" grep -Fxq "${setting}" <<< "${apt_config}"
done
config_line="$(grep -n 'cat > /etc/apt/apt.conf.d/80-download-resilience' "${work}/apt-script" | cut -d: -f1 || true)"
update_line="$(grep -n '^apt-get -qq update' "${work}/apt-script" | cut -d: -f1)"
check 'apt retry and timeout configuration precedes index downloads' test "${config_line:-999999}" -lt "${update_line}"

finish_tests "download-test: ${FAILURES} control(s) failed" 'download-test: all controls passed'
