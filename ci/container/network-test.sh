#!/usr/bin/env bash
# Exercise the real entry point with a daemon that cannot allocate subnets.
set -Eeuo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "${SCRIPT_DIR}/../test-lib.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
mkdir -p "${WORK}/bin"
cat >"${WORK}/bin/docker" <<'STUB'
#!/usr/bin/env bash
set -eu
state="${NVM_NETWORK_STUB:?}"
printf '%s\n' "$*" >>"${state}/calls"
case "$1" in
    image) echo sha256:fixture ;;
    run)
        if [ "${NVM_NETWORK_MODE}" = helper-term ]; then
            touch "${state}/helper"
            kill -TERM "${PPID}"
        fi
        ;;
    info) echo '{"io.containerd.runc.v2":{}}' ;;
    ps)
        if [[ "$*" == *name=^/nvmct-*-curl-* || "$*" == *id=fixture-helper ]]; then
            if [ "${NVM_NETWORK_MODE}" = helper-query-failure ]; then exit 29; fi
            if [ -f "${state}/helper" ]; then echo fixture-helper; fi
        elif [[ "$*" == *label=com.docker.compose.project=* ]]; then
            if [ "${NVM_NETWORK_MODE}" = query-failure ]; then exit 29; fi
            if [ -f "${state}/container" ]; then echo fixture; fi
        fi
        ;;
    rm) rm -f "${state}/helper" ;;
    container) [ "$2" = ls ] ;;
    volume|network)
        if [ "$2" != ls ]; then echo 'unexpected network/volume allocation' >&2; exit 30; fi
        if [ "$1" = volume ] && [[ "$*" == *label=com.docker.compose.project=* ]] && [ -f "${state}/volume" ]; then
            echo fixture-volume
        fi
        ;;
    compose)
        project="$3"; file="$5"; action="$6"
        case "${action}" in
            up)
                cp "${file}" "${state}/${project}.yaml"
                if ! grep -qx '    network_mode: bridge' "${file}"; then
                    echo allocation >>"${state}/networks"
                    echo 'all predefined address pools have been fully subnetted' >&2
                    exit 31
                fi
                if [ "${NVM_NETWORK_MODE}" = exhausted ]; then exit 32; fi
                touch "${state}/container" "${state}/volume"
                if [ "${NVM_NETWORK_MODE}" = term ]; then kill -TERM "${PPID}"; fi
                if [ "${NVM_NETWORK_MODE}" = int ]; then kill -INT "${PPID}"; fi
                ;;
            ps) exit 27 ;;
            down) rm -f "${state}/container" "${state}/volume" ;;
            logs) : ;;
            *) exit 33 ;;
        esac
        ;;
    *) echo 'unexpected Docker command' >&2; exit 34 ;;
esac
STUB
chmod +x "${WORK}/bin/docker"

run_control() {
    local mode="$1" rc=0 state="${WORK}/$1"
    mkdir -p "${state}"
    PATH="${WORK}/bin:${PATH}" NVM_NETWORK_STUB="${state}" NVM_NETWORK_MODE="${mode}" \
        NVM_IMAGE=fixture NVM_CURL_IMAGE=fixture NVM_CI_WORK="${state}/evidence" \
        bash "${SCRIPT_DIR}/test.sh" >"${state}/stdout" 2>"${state}/stderr" || rc=$?
    check "${mode}: failures remain nonzero" test "${rc}" -ne 0
    check "${mode}: no new subnet even when the pool is exhausted" test ! -e "${state}/networks"
    check "${mode}: no container residue" test ! -e "${state}/container"
    check "${mode}: no volume residue" test ! -e "${state}/volume"
    check "${mode}: no helper residue" test ! -e "${state}/helper"
    check "${mode}: helper namespace absence was read back" grep -q 'ps -aq --filter name=\^/nvmct-.*-curl-' "${state}/calls"
    if [ "${mode}" = helper-term ]; then
        check 'interrupted helper is removed through the EXIT ledger' grep -q 'rm -f fixture-helper' "${state}/calls"
        check 'helper removal is read back by id' grep -q 'ps -aq --filter id=fixture-helper' "${state}/calls"
        check 'helper TERM preserves signal status' test "${rc}" -eq 143
        return
    fi
    check "${mode}: project teardown ran" grep -q 'down -v --remove-orphans' "${state}/calls"
    check "${mode}: container absence was read back" grep -q 'ps -aq --filter label=com.docker.compose.project=' "${state}/calls"
    check "${mode}: volume absence was read back" grep -q 'volume ls -q --filter label=com.docker.compose.project=' "${state}/calls"
    if [ "${mode}" = exhausted ]; then
        check 'all eight variants and crash control reached compose without requesting a subnet' \
            test "$(grep -c ' up -d$' "${state}/calls")" -eq 9
    elif [ "${mode}" = term ]; then
        check 'TERM triggers EXIT cleanup and preserves signal status' test "${rc}" -eq 143
    elif [ "${mode}" = int ]; then
        check 'INT triggers EXIT cleanup and preserves signal status' test "${rc}" -eq 130
    elif [ "${mode}" = query-failure ] || [ "${mode}" = helper-query-failure ]; then
        check 'a failed absence query keeps the cleanup ledger red' grep -q 'not proven removed' "${state}/stderr"
    else
        check 'mid-run failure preserves the original status' test "${rc}" -eq 27
    fi
}
run_control exhausted
run_control mid-failure
run_control term
run_control int
run_control query-failure
run_control helper-term
run_control helper-query-failure
finish_tests "network-test: ${FAILURES} control(s) failed" 'network-test: all controls passed'
