#!/usr/bin/env bash
# Sanitize every tracked file, using the same entry point locally and in CI.
# Name-based coordinates still require review before publication.
set -Eeuo pipefail
export LC_ALL=C

repo_root="$(git rev-parse --show-toplevel)" || {
    echo "guards: cannot locate the Git worktree" >&2
    exit 1
}
cd "${repo_root}"
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

# Keep the existing ban on tracked attributes. Scans use -a so binary matches
# retain file:line output instead of only reporting that a binary file matches.
rc=0
git ls-files -- '.gitattributes' '*/.gitattributes' >"${work}/attributes" || rc=$?
if [ "${rc}" -ne 0 ]; then
    echo "guards: ls-files failed (rc=${rc})" >&2
    exit 1
fi
if [ -s "${work}/attributes" ]; then
    echo "guards: tracked .gitattributes found:" >&2
    cat "${work}/attributes" >&2
    exit 1
fi

# A positive tracked-file control prevents a broken scan returning only
# 'no match' from turning a skipped worktree into a pass.
git grep -qaF 'name = "truenas-nvidia-driver-manager"' -- Cargo.toml || {
    echo "guards: tracked-file engine control failed" >&2
    exit 1
}

# A sentence-ending period bounds an address; another dotted numeric segment
# does not. Three-segment versions cannot match the four-octet alternatives.
ipv4='(^|[^0-9.])(10(\.[0-9]{1,3}){3}|192\.168(\.[0-9]{1,3}){2}|172\.(1[6-9]|2[0-9]|3[01])(\.[0-9]{1,3}){2})([^0-9.]|\.([^0-9.]|$)|$)'
# A label containing a non-hex letter may end in the MAC's separator.
# All-hex/numeric prefixes stay excluded to avoid IPv6, dates and longer chains.
mac_colon='(^|[^0-9A-Fa-f:]|[G-Zg-z][[:alnum:]_]*:)([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}([^0-9A-Fa-f:]|$)'
mac_hyphen='(^|[^0-9A-Fa-f-]|[G-Zg-z][[:alnum:]_]*-)([0-9A-Fa-f]{2}-){5}[0-9A-Fa-f]{2}([^0-9A-Fa-f-]|$)'
mac_dotted='(^|[^0-9A-Fa-f.]|[G-Zg-z][[:alnum:]_]*\.)([0-9A-Fa-f]{4}\.){2}[0-9A-Fa-f]{4}([^0-9A-Fa-f.]|\.([^0-9A-Fa-f.]|$)|$)'
mac="${mac_colon}|${mac_hyphen}|${mac_dotted}"
# Assemble the pool prefix so the guard itself needs no path exemption.
pool_root='/mnt'
pool="${pool_root}/[^<[:space:]]"

canary() { # canary <expected grep rc> <rule> <pattern> <text>
    local want="$1" rule="$2" pattern="$3" value="$4" got=0
    printf '%s\n' "${value}" >"${work}/canary"
    # Use git grep's actual ERE engine and text mode, not system grep.
    (cd "${work}" && git grep --no-index -naE "${pattern}" -- canary) \
        >"${work}/canary.out" 2>"${work}/canary.err" || got=$?
    if [ "${got}" -ne "${want}" ]; then
        echo "guards: ${rule} canary failed (expected rc=${want}, got rc=${got})" >&2
        cat "${work}/canary.err" >&2
        exit 1
    fi
}

for stem in 10.1.2 192.168.1 172.16.1 172.31.255; do
    canary 0 IPv4 "${ipv4}" "${stem}.3"
    canary 0 IPv4 "${ipv4}" "reach ${stem}.3."
done
canary 1 IPv4 "${ipv4}" 'version ^10.6.0 || >=11.0.0; 1.2.3.4'
for stem in 172.15.1 172.32.1 192.169.1 110.1.2 11.1.2; do
    canary 1 IPv4 "${ipv4}" "${stem}.3"
done
for sep in ':' '-'; do
    address="aa${sep}bb${sep}cc${sep}dd${sep}ee${sep}ff"
    canary 0 MAC "${mac}" "${address}"
    canary 0 MAC "${mac}" "nic${sep}${address}"
    canary 1 MAC "${mac}" "f0${sep}${address}"
    if [ "${sep}" = ':' ]; then
        canary 1 MAC "${mac}" "fe80::${address}"
    fi
done
sep='.'
address="aabb${sep}ccdd${sep}eeff"
canary 0 MAC "${mac}" "${address}"
canary 0 MAC "${mac}" "x${sep}${address}"
canary 1 MAC "${mac}" "0000${sep}${address}"
date_stem='2026-10-04'
canary 1 MAC "${mac}" "${date_stem}-12-30-45"
canary 1 MAC "${mac}" 'deadbeef 0123456789abcdef 2026 1.2.3.4'
canary 0 pool "${pool}" "${pool_root}/probe/file"
canary 1 pool "${pool}" "${pool_root}/<pool>/file"

scan() { # scan <rule> <pattern>
    local rule="$1" pattern="$2" got=0
    # Pathspecs after -- cannot be interpreted as options; no tracked path
    # (including this script and the workflow) is excluded from the scan.
    git grep -naE "${pattern}" -- . >"${work}/hits" 2>"${work}/scan.err" || got=$?
    case "${got}" in
        0)
            echo "guards: ${rule} literals found in tracked files:" >&2
            cat "${work}/hits" >&2
            exit 1
            ;;
        1)
            echo "OK: no ${rule} literals in tracked files"
            ;;
        *)
            echo "guards: ${rule} scan failed (rc=${got})" >&2
            cat "${work}/scan.err" >&2
            exit 1
            ;;
    esac
}

scan 'private IPv4' "${ipv4}"
scan MAC "${mac}"
scan 'non-placeholder pool path' "${pool}"
