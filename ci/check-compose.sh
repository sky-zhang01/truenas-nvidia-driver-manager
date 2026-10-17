#!/usr/bin/env bash
# Static checks over the compose template, plus one `docker compose config` run
# for each of the three network sections.
#
# The template uses placeholders only, so each run substitutes test values
# before handing the file to docker compose. The test values are documentation
# addresses and a runtime-assembled MAC, never literals in this file.
#
# The descriptive comments (the offline image note, `$$`, the
# port-change note) are checked as text; the structural keys (privileged/pid/
# runtime, service count, the environment sequence, the mappings) are proven by
# the rendered config and the structural checks, so a comment cannot satisfy
# them.
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
template="${repo_root}/deploy/compose.yaml"
check="${repo_root}/ci/check-compose-structure.py"
workdir="$(mktemp -d)"
trap 'rm -rf "${workdir}"' EXIT

command -v docker >/dev/null 2>&1 \
    || { echo "check-compose: docker is not installed; cannot run 'docker compose config'" >&2; exit 1; }
command -v python3 >/dev/null 2>&1 \
    || { echo "check-compose: python3 is required for the structural checks" >&2; exit 1; }

fail_check() { echo "check-compose: $1" >&2; exit 1; }

[ -f "${template}" ] || fail_check "missing ${template}"
[ -f "${check}" ] || fail_check "missing ${check}"
require() { grep -qF -- "$1" "${template}" || fail_check "template is missing: $1"; }

require 'privileged: true'
require 'pid: host'
require 'runtime: runc'
require 'restart: unless-stopped'
require '      - "8199:8199"'
require 'NVM_INITIAL_ADMIN_PASSWORD=<initial-password>'
require '/mnt/<pool>/<dataset>:/var/lib/nvm'
require 'Write each $ in the password'
require 'Write each $ in the password as $$.'
require 'change both sides of the mapping to that port.'
require 'If you change NVM_PORT, change both sides of the mapping to that port.'
require 'image: ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:latest'
require 'Public GHCR images can be pulled without registry credentials'
require 'roll back = replace :latest with the previous version tag'
require 'update = click Update when offered'
require 'pull/save on a networked computer, load on the NAS'
require 'Plain HTTP on all NAS interfaces by default'
require 'mac_address: <mac-address>'
require 'bridge section'
require 'macvlan section (service part)'
require 'ipvlan section (service part)'
require 'macvlan section (end-of-file part)'
require 'ipvlan section (end-of-file part)'

# The data-directory line uses the placeholder form; the full path-placeholder
# scan lives in `ci/guards.sh` (the static job).

render_variant() {
    local mode="$1" out="$2"
    awk -v mode="${mode}" '
        function structural_service(s) {
            return s ~ /^    # +(networks:|lan:|ipv4_address:|mac_address:)/
        }
        function structural_eof(s) {
            return s ~ /^# +(networks:|lan:|driver:|driver_opts:|parent:|ipam:|config:|- subnet:|gateway:)/
        }
        /^    # ==== macvlan section \(service part\)/ { svc="macvlan"; print; next }
        /^    # ==== ipvlan section \(service part\)/  { svc="ipvlan"; print; next }
        /^# ==== macvlan section \(end-of-file part\)/ { eof="macvlan"; svc=""; print; next }
        /^# ==== ipvlan section \(end-of-file part\)/  { eof="ipvlan"; svc=""; print; next }
        {
            if (mode == "bridge") { print; next }
            if ($0 == "    ports:" || $0 == "      - \"8199:8199\"") next
            if (svc == mode && structural_service($0)) { sub(/^    # /, "    "); print; next }
            if (eof == mode && structural_eof($0)) { sub(/^# /, ""); print; next }
            print
        }
    ' "${template}" > "${out}"
}

substitute() {
    local file="$1"
    local sep=':' mac
    mac="02${sep}00${sep}00${sep}00${sep}00${sep}01"
    sed -i.bak \
        -e 's|<initial-password>|ci-fixture-password-not-a-secret|g' \
        -e 's|<pool>|tank|g' \
        -e 's|<dataset>|nvm|g' \
        -e 's|<container-static-ip>|192.0.2.10|g' \
        -e "s|<mac-address>|${mac}|g" \
        -e 's|<host-interface>|eth0|g' \
        -e 's|<lan-subnet>|192.0.2.0/24|g' \
        -e 's|<lan-gateway>|192.0.2.1|g' \
        "${file}"
    rm -f "${file}.bak"
}

for mode in bridge macvlan ipvlan; do
    variant="${workdir}/compose.${mode}.yaml"
    rendered="${workdir}/compose.${mode}.json"
    render_variant "${mode}" "${variant}"
    substitute "${variant}"
    echo "check-compose: docker compose config (${mode})"
    docker compose -f "${variant}" config -q \
        || fail_check "docker compose config failed for the ${mode} variant"
    docker compose -f "${variant}" config --format json > "${rendered}" \
        || fail_check "docker compose config --format json failed for the ${mode} variant"
    python3 "${check}" "${mode}" "${template}" "${rendered}" \
        || fail_check "structural checks failed for the ${mode} variant"
done

echo "check-compose: OK (comment checks + structural config checks for bridge/macvlan/ipvlan)"
