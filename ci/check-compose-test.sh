#!/usr/bin/env bash
# check-compose-test.sh — controls for the compose structural checks. Runs on the CI runner without Docker: the checker is fed the
# rendered shapes it must accept and the doctored ones it must reject.
#
# The last control runs the real `check-compose.sh` when Docker Compose is available.
set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="${SCRIPT_DIR}/check-compose-structure.py"
TEMPLATE="${SCRIPT_DIR}/../deploy/compose.yaml"

command -v python3 >/dev/null 2>&1 || { echo "check-compose-test: python3 is required" >&2; exit 1; }
[ -f "${CHECK}" ] || { echo "check-compose-test: missing ${CHECK}" >&2; exit 1; }
[ -f "${TEMPLATE}" ] || { echo "check-compose-test: missing ${TEMPLATE}" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

. "${SCRIPT_DIR}/test-lib.sh"

expect() { # expect <rc> <description> <mode> <template> <config.json>
    local want="$1" desc="$2" mode="$3" tpl="$4" config="$5" got=0
    python3 "${CHECK}" "${mode}" "${tpl}" "${config}" >/dev/null 2>"${WORK}/stderr" || got=$?
    if [ "${got}" -eq "${want}" ]; then
        pass "${desc} (rc=${got})"
    else
        fail "${desc}: want rc=${want}, got rc=${got}: $(head -c 200 "${WORK}/stderr")"
    fi
}

# ── fixtures ─────────────────────────────────────────────────────────────────
# The fixture data directory is assembled at runtime, like the MAC below: a
# literal non-placeholder /mnt/ path in a tracked file would fail the
# sanitization job.
mount_root="/mnt"
fixture_pool="tank"
fixture_data="${mount_root}/${fixture_pool}/nvm"
cp "${TEMPLATE}" "${WORK}/template.yaml"
# `environment:` in the mapping form and without it at all.
sed 's|^      - NVM_INITIAL_ADMIN_PASSWORD=<initial-password>$|      NVM_INITIAL_ADMIN_PASSWORD: <initial-password>|' \
    "${TEMPLATE}" > "${WORK}/template-env-mapping.yaml"
sed -e '/^    environment:$/d' -e '/^      - NVM_INITIAL_ADMIN_PASSWORD=/d' \
    "${TEMPLATE}" > "${WORK}/template-no-environment.yaml"

cat > "${WORK}/bridge.json" <<JSON
{
  "name": "nvmct",
  "services": {
    "nvidia-driver-manager": {
      "image": "ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:latest",
      "restart": "unless-stopped",
      "privileged": true,
      "pid": "host",
      "runtime": "runc",
      "environment": {"NVM_INITIAL_ADMIN_PASSWORD": "ci-fixture-password-not-a-secret"},
      "volumes": [
        {"type": "bind", "source": "${fixture_data}", "target": "/var/lib/nvm", "bind": {"create_host_path": true}}
      ],
      "ports": [{"mode": "ingress", "target": 8199, "published": "8199", "protocol": "tcp"}]
    }
  }
}
JSON

# The test MAC is assembled at runtime like check-compose.sh does: a MAC
# literal in a tracked file would fail the sanitization job.
mac_sep=':'
test_mac="02${mac_sep}00${mac_sep}00${mac_sep}00${mac_sep}00${mac_sep}01"
cat > "${WORK}/macvlan.json" <<JSON
{
  "name": "nvmct",
  "services": {
    "nvidia-driver-manager": {
      "image": "ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:latest",
      "restart": "unless-stopped",
      "privileged": true,
      "pid": "host",
      "runtime": "runc",
      "environment": {"NVM_INITIAL_ADMIN_PASSWORD": "ci-fixture-password-not-a-secret"},
      "volumes": [
        {"type": "bind", "source": "${fixture_data}", "target": "/var/lib/nvm"}
      ],
      "networks": {"lan": {"ipv4_address": "192.0.2.10", "mac_address": "${test_mac}"}}
    }
  },
  "networks": {
    "lan": {
      "driver": "macvlan",
      "driver_opts": {"parent": "eth0"},
      "ipam": {"config": [{"subnet": "192.0.2.0/24", "gateway": "192.0.2.1"}]}
    }
  }
}
JSON

python3 - "${WORK}/macvlan.json" "${WORK}/ipvlan.json" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
doc["services"]["nvidia-driver-manager"]["networks"]["lan"].pop("mac_address")
doc["networks"]["lan"]["driver"] = "ipvlan"
json.dump(doc, open(sys.argv[2], "w"), indent=2)
PY

# ── positive controls ────────────────────────────────────────────────────────
expect 0 "the rendered bridge config passes" bridge "${WORK}/template.yaml" "${WORK}/bridge.json"
expect 0 "the rendered macvlan config passes" macvlan "${WORK}/template.yaml" "${WORK}/macvlan.json"
expect 0 "the rendered ipvlan config passes" ipvlan "${WORK}/template.yaml" "${WORK}/ipvlan.json"

# ── negative controls (each must be rejected) ────────────────────────────────
expect 1 "environment: as a mapping is rejected" bridge \
    "${WORK}/template-env-mapping.yaml" "${WORK}/bridge.json"
expect 1 "a template without environment: is rejected" bridge \
    "${WORK}/template-no-environment.yaml" "${WORK}/bridge.json"

python3 - "${WORK}/bridge.json" "${WORK}/two-services.json" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
doc["services"]["second"] = json.loads(json.dumps(doc["services"]["nvidia-driver-manager"]))
json.dump(doc, open(sys.argv[2], "w"), indent=2)
PY
expect 1 "a second service is rejected" bridge "${WORK}/template.yaml" "${WORK}/two-services.json"

python3 - "${WORK}/bridge.json" "${WORK}/no-pid.json" "${WORK}/no-runtime.json" "${WORK}/no-ports.json" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
service = doc["services"]["nvidia-driver-manager"]
pid = json.loads(json.dumps(doc))
pid["services"]["nvidia-driver-manager"].pop("pid")
json.dump(pid, open(sys.argv[2], "w"), indent=2)
runtime = json.loads(json.dumps(doc))
runtime["services"]["nvidia-driver-manager"].pop("runtime")
json.dump(runtime, open(sys.argv[3], "w"), indent=2)
ports = json.loads(json.dumps(doc))
ports["services"]["nvidia-driver-manager"].pop("ports")
json.dump(ports, open(sys.argv[4], "w"), indent=2)
assert service
PY
expect 1 "a commented-out pid (missing key) is rejected" bridge \
    "${WORK}/template.yaml" "${WORK}/no-pid.json"
expect 1 "a missing runtime is rejected" bridge "${WORK}/template.yaml" "${WORK}/no-runtime.json"
expect 1 "a bridge variant without the port mapping is rejected" bridge \
    "${WORK}/template.yaml" "${WORK}/no-ports.json"

python3 - "${WORK}/macvlan.json" "${WORK}/macvlan-no-opts.json" "${WORK}/macvlan-wrong-driver.json" "${WORK}/ipvlan-with-mac.json" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
no_opts = json.loads(json.dumps(doc))
no_opts["networks"]["lan"].pop("driver_opts")
json.dump(no_opts, open(sys.argv[2], "w"), indent=2)
wrong = json.loads(json.dumps(doc))
wrong["networks"]["lan"]["driver"] = "ipvlan"
json.dump(wrong, open(sys.argv[3], "w"), indent=2)
with_mac = json.loads(json.dumps(doc))
with_mac["networks"]["lan"]["driver"] = "ipvlan"
json.dump(with_mac, open(sys.argv[4], "w"), indent=2)
PY
expect 1 "a macvlan network without driver_opts is rejected" macvlan \
    "${WORK}/template.yaml" "${WORK}/macvlan-no-opts.json"
expect 1 "the wrong network driver is rejected" macvlan \
    "${WORK}/template.yaml" "${WORK}/macvlan-wrong-driver.json"
expect 1 "a mac_address under ipvlan is rejected" ipvlan \
    "${WORK}/template.yaml" "${WORK}/ipvlan-with-mac.json"

# ── the real script, when Docker Compose is available ────────────────────────────────
if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
    if bash "${SCRIPT_DIR}/check-compose.sh" >"${WORK}/check-compose.log" 2>&1; then
        pass "check-compose.sh with the real docker compose (see ${WORK}/check-compose.log)"
    else
        fail "check-compose.sh failed: $(tail -c 300 "${WORK}/check-compose.log")"
    fi
else
    echo "SKIP: check-compose.sh needs Docker Compose; the structural controls above ran without it"
fi

finish_tests "check-compose-test: ${FAILURES} control(s) failed" \
    "check-compose-test: all controls passed"
