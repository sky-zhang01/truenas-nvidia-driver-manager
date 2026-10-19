#!/usr/bin/env bash
# test.sh — container-level tests.
#
# Starts the App container on the CI Docker host from deploy/compose.yaml with the
# placeholders replaced by test values; each variant is probed from the CI runner
# and every `docker logs` line must be an ASCII-only JSON object. For the
# untouched template "no setting problems" is asserted, plus one empty-base
# assertion that does not depend on the CI host (its own mountinfo must carry the
# /etc/hostname row): its startup check runs `docker inspect` on the CI host
# through the program's own namespace mode, and that part is host-dependent and is
# only recorded.
#
# Variants are independent and concurrent runs on one daemon do not collide:
#     the rendered `image:` is the final NVM_IMAGE, and the container's image id
#     is checked against it after `up`;
#     every variant has its own data volume, removed by down -v and checked;
#     host ports are allocated by Docker (`0:<container port>` in the mapping) and
#     the project name carries a per-run token.
# All scenarios use Docker's existing bridge: they need published ports, not
# Compose DNS or inter-service traffic, so no per-project subnet is allocated.
set -Eeuo pipefail

IMAGE="${NVM_IMAGE:-nvm:ci}"
SERVICE="nvidia-driver-manager"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TEMPLATE="${REPO_ROOT}/deploy/compose.yaml"
LOG_CHECK="${SCRIPT_DIR}/log-object-check.py"

command -v docker >/dev/null 2>&1 || { echo "test.sh: docker is not installed" >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo "test.sh: python3 is required" >&2; exit 1; }
command -v node >/dev/null 2>&1 || { echo "test.sh: node is required" >&2; exit 1; }
[ -f "${LOG_CHECK}" ] || { echo "test.sh: missing ${LOG_CHECK}" >&2; exit 1; }
docker image inspect "${IMAGE}" >/dev/null 2>&1 \
    || { echo "test.sh: image ${IMAGE} not found (set NVM_IMAGE or docker build -t ${IMAGE} .)" >&2; exit 1; }
IMAGE_ID="$(docker image inspect -f '{{.Id}}' "${IMAGE}")"
[ -n "${IMAGE_ID}" ] || { echo "test.sh: cannot read the image id of ${IMAGE}" >&2; exit 1; }

# Probe with the CI job's existing curl image in the App network namespace.
# docker port separately checks publication; no daemon-host address is guessed.
CURL_IMAGE="${NVM_CURL_IMAGE:-}"
if [ -z "${CURL_IMAGE}" ]; then
    CURL_IMAGE="$(docker inspect -f '{{.Image}}' "${HOSTNAME:-$(hostname)}")" \
        || { echo "test.sh: set NVM_CURL_IMAGE to an image containing curl when running outside a Docker job" >&2; exit 1; }
fi
curl_in_app() {
    local cid="$1"; shift
    docker run --rm --name "nvmct-${RUN_ID}-curl-${RANDOM}-${RANDOM}" \
        --network "container:${cid}" --entrypoint curl "${CURL_IMAGE}" \
        --noproxy '*' --max-time 5 "$@"
}

WORK="${NVM_CI_WORK:-$(mktemp -d)}"
mkdir -p "${WORK}"
RUN_ID="$(python3 -c 'import secrets; print(secrets.token_hex(4))')"
# The shared teardown machinery (ci/cleanup-lib.sh): register → tear down →
# read-back proof → final judgment.
. "${SCRIPT_DIR}/../cleanup-lib.sh"

nvm_cleanup_one() { # nvm_cleanup_one <item>
    case "$1" in
        helpers:*)
            # Command substitutions cannot update the parent's ledger. Register
            # the run's helper namespace before any of its containers can start.
            local helpers="" id remaining=""
            helpers="$(docker ps -aq --filter "name=^/nvmct-${RUN_ID}-curl-" \
                2>"${WORK}/helpers.ps.err")" || return 1
            for id in ${helpers}; do
                nvm_container_remove "${id}" || return 1
            done
            remaining="$(docker ps -aq --filter "name=^/nvmct-${RUN_ID}-curl-" \
                2>"${WORK}/helpers.readback.err")" || return 1
            [ -z "${remaining}" ]
            ;;
        project:*)
            local project="${1#project:}"
            nvm_compose_teardown "${project}" "${WORK}/compose.${project}.yaml" \
                "${WORK}/exit-retry.${project}"
            ;;
        *)
            echo "test.sh: unknown cleanup item $1" >&2
            return 1
            ;;
    esac
}
trap 'nvm_cleanup_finish "$?" "${WORK}"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
nvm_cleanup_track "helpers:${RUN_ID}"

# Only old, inactive resources in this test namespace are eligible for removal.
python3 "${SCRIPT_DIR}/stale-cleanup.py"
docker run --rm --name "nvmct-${RUN_ID}-curl-version" \
    --entrypoint curl "${CURL_IMAGE}" --version >/dev/null

. "${SCRIPT_DIR}/../test-lib.sh"

NEW_PASSWORD="ci-fixture-password-not-a-secret-2"

# Login setup problems: follow the product schema exactly. A parse error, a missing
# field or a wrong type exits non-zero so the caller fails; an empty result must
# never be treated as "no problems". A data_mapping item also yields its nested
# mapping_reason key (for example
# `startup.failed/startup.data_mapping/startup.mapping.no_hostname_line`), which the
# empty-base positive assertion needs.
json_setup_problem_keys() {
    node -e '
const fs = require("node:fs");
const object = value => value !== null && typeof value === "object" && !Array.isArray(value);
const die = message => { console.error(message); process.exit(1); };
let doc;
try { doc = JSON.parse(fs.readFileSync(0, "utf8")); }
catch (error) { die(`login status is not JSON: ${error.message}`); }
if (!object(doc)) die("login status is not a JSON object");
if (!object(doc.setup)) die("login status has no setup object");
if (!Array.isArray(doc.setup.problems)) die("login status setup.problems is not a list");
const keys = [];
for (const problem of doc.setup.problems) {
    if (!object(problem) || !object(problem.params)) die("a setup.problems entry has no object params");
    if (problem.key === "setting.unknown" || problem.key === "setting.invalid") {
        if (typeof problem.params.variable !== "string" || !problem.params.variable)
            die("a setting problem has no string variable");
        keys.push(problem.key);
    } else if (problem.key === "startup.failed") {
        const items = problem.params.items;
        if (!Array.isArray(items) || items.length === 0) die("startup.failed has no items");
        keys.push(problem.key);
        for (const item of items) {
            if (!object(item) || !object(item.params) ||
                !["startup.pid_host", "startup.runtime", "startup.data_mapping"].includes(item.key))
                die("startup.failed has an invalid item");
            keys.push(`startup.failed/${item.key}`);
            if (item.key === "startup.data_mapping") {
                const reason = item.params.mapping_reason;
                if (!object(reason) || typeof reason.key !== "string" || !reason.key)
                    die("startup.data_mapping carries no mapping_reason key");
                keys.push(`startup.failed/${item.key}/${reason.key}`);
            }
        }
    } else {
        die("a setup.problems entry has an unknown key");
    }
}
process.stdout.write(keys.join("\n"));
'
}

print_setup_diagnostics() { # print_setup_diagnostics <unauthenticated_login_status> <container-id>
    printf '%s' "$1" | node -e '
const doc = JSON.parse(require("node:fs").readFileSync(0, "utf8"));
console.error("setup.problems=" + JSON.stringify(doc.setup.problems));
' || true
    docker logs "$2" 2>&1 \
        | grep -E 'startup_check_failed|startup_checks_passed' >&2 || true
    docker inspect -f 'pid_mode={{.HostConfig.PidMode}} privileged={{.HostConfig.Privileged}}' "$2" >&2 || true
    # The empty-base image carries no readlink, so a one-shot container
    # reads the two links: it shares the host PID namespace (--pid host), so
    # /proc/1 is the Docker host's PID 1 and /proc/<host pid> is the container init
    # docker inspect just reported — the same two links the in-container command
    # reads. --privileged is required: reading another process's ns link
    # needs ptrace access (--cap-add SYS_PTRACE is not enough), and the App
    # container itself is privileged and reads /proc/1/ns/mnt the same way.
    # readlink gets one path per invocation: the CI default (the job image) has
    # GNU readlink, but NVM_CURL_IMAGE=curlimages/curl locally is busybox readlink,
    # which takes a single FILE.
    local diag_pid="" link=""
    if diag_pid="$(docker inspect -f '{{.State.Pid}}' "$2" 2>/dev/null)" \
        && [ -n "${diag_pid}" ] && [ "${diag_pid}" != "0" ]; then
        for link in /proc/1/ns/mnt "/proc/${diag_pid}/ns/mnt"; do
            docker run --rm --name "nvmct-${RUN_ID}-curl-${RANDOM}-${RANDOM}" \
                --privileged --pid host --user 0:0 \
                --entrypoint readlink "${CURL_IMAGE}" "${link}" >&2 || true
        done
    else
        echo "diagnostics: the container reports no host PID to read its mount namespace from" >&2
    fi
    docker info -f '{{json .SecurityOptions}}' >&2 || true
}

# The login status update_blocked list (the product only carries it when logged in).
json_update_blocked_keys() {
    python3 -c '
import json, sys

def die(message):
    print(message, file=sys.stderr)
    sys.exit(1)

try:
    doc = json.load(sys.stdin)
except Exception as exc:
    die("login status is not JSON: %s" % exc)
if not isinstance(doc, dict):
    die("login status is not a JSON object")
blocked = doc.get("update_blocked")
if not isinstance(blocked, list):
    die("login status update_blocked is not a list")
keys = []
for item in blocked:
    if not isinstance(item, dict):
        die("an update_blocked entry is not an object")
    key = item.get("key")
    if not isinstance(key, str) or not key:
        die("an update_blocked entry has no string key")
    keys.append(key)
print("\n".join(keys))
'
}

# The request token in a response body. A missing or mistyped field exits
# non-zero so the caller cannot treat an empty string as a token.
json_token() { # json_token <file>
    python3 -c '
import json, sys
path = sys.argv[1]
try:
    with open(path, encoding="utf-8") as handle:
        doc = json.load(handle)
except Exception as exc:
    print("response is not JSON: %s" % exc, file=sys.stderr)
    sys.exit(1)
if not isinstance(doc, dict):
    print("response is not a JSON object", file=sys.stderr)
    sys.exit(1)
token = doc.get("token")
if not isinstance(token, str) or not token:
    print("response has no string token", file=sys.stderr)
    sys.exit(1)
sys.stdout.write(token)
' "$1"
}

render_base() { # render_base <out> <container-port>
    local out="$1" container_port="$2"
    awk -v image="${IMAGE}" -v cport="${container_port}" '
        /^    image: / {
            print "    image: " image
            print "    network_mode: bridge"
            next
        }
        /^      - "8199:8199"$/ {
            printf "      - \"0:%s\"\n", cport
            next
        }
        { print }
    ' "${TEMPLATE}" \
        | sed \
            -e 's|/mnt/<pool>/<dataset>|nvm-data|g' \
            -e 's|<registry>|registry.example|g' \
            -e 's|<tag>|ci|g' \
            -e 's|<container-static-ip>|192.0.2.10|g' \
            -e 's|<host-interface>|eth0|g' \
            -e 's|<lan-subnet>|192.0.2.0/24|g' \
            -e 's|<lan-gateway>|192.0.2.1|g' \
            > "${out}"
    # The template's image line must really become the CI-built image, otherwise
    # the variant tests something else; a rewritten template image line this
    # renderer does not follow must fail instead of silently testing another image.
    grep -qF "    image: ${IMAGE}" "${out}" \
        || { echo "test.sh: the rendered variant does not carry the test image ${IMAGE}" >&2; exit 1; }
    # A project-owned volume avoids job-container paths invisible to the daemon.
    printf '\nvolumes:\n  nvm-data:\n' >> "${out}"
}

modify_variant() { # modify_variant <variant> <file>
    local variant="$1" file="$2" env_setting=""
    case "${variant}" in
        baseline) ;;
        no-pid-host)
            sed -i.bak -e '/^    pid: host$/d' "${file}"
            ;;
        no-data-mount)
            sed -i.bak -e '/^    volumes:$/d' -e '\|^      - nvm-data:/var/lib/nvm$|d' \
                -e '/^volumes:$/d' -e '/^  nvm-data:$/d' "${file}"
            ;;
        runtime-containerd)
            sed -i.bak -e 's|^    runtime: runc$|    runtime: io.containerd.runc.v2|' "${file}"
            ;;
        extra-env) env_setting='NVM_FOO=1' ;;
        build-timeout-zero) env_setting='NVM_BUILD_TIMEOUT_MINUTES=0' ;;
        port-18199) env_setting='NVM_PORT=18199' ;;
        port-abc) env_setting='NVM_PORT=abc' ;;
        *) echo "test.sh: unknown variant ${variant}" >&2; exit 2 ;;
    esac
    if [ -n "${env_setting}" ]; then
        awk -v setting="${env_setting}" '
            /^    # environment:$/ { print "    environment:"; print "      - " setting; next }
            { print }
        ' "${file}" >"${file}.settings"
        mv "${file}.settings" "${file}"
    fi
    rm -f "${file}.bak"
}

# Tear one variant down: keep the full logs first, then `down`, then read back
# that no container or volume is left. The project is dropped from the books
# only when `down` and both residue queries succeeded and came back empty; a
# failed query or any residue keeps it registered for the EXIT trap's retry.
teardown_variant() { # teardown_variant <variant> <project> <cid>
    local variant="$1" project="$2" cid="$3"
    local file="${WORK}/compose.${project}.yaml"
    # Log collection is best-effort evidence and never decides the cleanup
    # result; a failure here does not stop the teardown checks below.
    if [ -n "${cid}" ]; then
        docker logs "${cid}" >"${WORK}/logs.${variant}.txt" 2>&1 || true
        docker inspect "${cid}" >"${WORK}/inspect.${variant}.json" 2>&1 || true
    fi
    docker compose -p "${project}" -f "${file}" logs --no-color \
        >"${WORK}/compose-logs.${variant}.txt" 2>&1 || true

    if nvm_compose_teardown "${project}" "${file}" "${WORK}/teardown.${variant}"; then
        nvm_cleanup_untrack "project:${project}"
    else
        fail "${variant}: teardown not proven for project ${project}; it stays registered (see ${WORK}/teardown.${variant}.*)"
    fi
}

# Successful probes stay alive until every scenario has finished its checks.
OBSERVED_VARIANTS=()
OBSERVED_PROJECTS=()
OBSERVED_CONTAINERS=()
OBSERVED_RESTARTS=()
OBSERVED_AT=()

record_observation() { # record_observation <variant> <project> <cid> <count> <time>
    OBSERVED_VARIANTS+=("$1")
    OBSERVED_PROJECTS+=("$2")
    OBSERVED_CONTAINERS+=("$3")
    OBSERVED_RESTARTS+=("$4")
    OBSERVED_AT+=("$5")
}

# Docker reports a nonnegative integer; malformed output cannot prove stability.
read_restart_count() { # read_restart_count <cid>
    local count
    count="$(docker inspect -f '{{.RestartCount}}' "$1")" || return 1
    case "${count}" in ''|*[!0-9]*) return 1 ;; esac
    printf '%s\n' "${count}"
}

restart_count_unchanged() { test "$1" = "$2"; }
not_restart_stable() { ! restart_count_unchanged "$1" "$2"; }

probe_variant() { # probe_variant <variant> <expected-key|-> <container-port>
    local variant="$1" expected="$2" container_port="$3"
    local project="nvmct-${RUN_ID}-${variant//_/-}"
    local file="${WORK}/compose.${project}.yaml"
    render_base "${file}" "${container_port}"
    modify_variant "${variant}" "${file}"

    echo "== variant ${variant} (expected ${expected}, container port ${container_port}) =="
    nvm_cleanup_track "project:${project}"
    local cid=""
    if ! docker compose -p "${project}" -f "${file}" up -d >"${WORK}/up.${variant}.log" 2>&1; then
        fail "${variant}: docker compose up failed (see ${WORK}/up.${variant}.log)"
        cat "${WORK}/up.${variant}.log" >&2 || true
    else
        cid="$(docker compose -p "${project}" -f "${file}" ps -q "${SERVICE}")"
        if [ -z "${cid}" ]; then
            fail "${variant}: compose reported no container"
        fi
    fi

    local host_port=""
    if [ -n "${cid}" ]; then
        # The container that runs must be the final image inspected above.
        local actual_id=""
        if actual_id="$(docker inspect -f '{{.Image}}' "${cid}" 2>/dev/null)"; then
            check "${variant}: runs the inspected final image" test "${actual_id}" = "${IMAGE_ID}"
        else
            fail "${variant}: cannot read the running image id"
        fi
        # Docker allocates the host port, so parallel runs on one daemon do not
        # collide on a fixed port.
        local ports=""
        if ports="$(docker port "${cid}" "${container_port}/tcp" 2>/dev/null)"; then
            host_port="$(printf '%s\n' "${ports}" | head -1 | awk -F: '{print $NF}')"
        fi
        if [ -z "${host_port}" ]; then
            fail "${variant}: container port ${container_port} has no published host port"
        fi
        # Positive control of the residue read-back: for variants that mount the
        # data volume, the project label query must actually find the volume
        # after up — otherwise "no residue" after teardown proves nothing
        # (no-data-mount has no volume and is not forced through this).
        if [ "${variant}" != "no-data-mount" ]; then
            local project_vols="" vols_rc=0
            project_vols="$(docker volume ls -q --filter "label=com.docker.compose.project=${project}" \
                2>"${WORK}/vol-positive.${variant}.err")" || vols_rc=$?
            if [ "${vols_rc}" -ne 0 ]; then
                fail "${variant}: volume presence query failed (rc=${vols_rc}; see ${WORK}/vol-positive.${variant}.err)"
            elif [ -z "${project_vols}" ]; then
                fail "${variant}: no volume carries the project label after up; the residue read-back would prove nothing"
            else
                pass "${variant}: the project volume is visible to the label query after up"
            fi
        fi
    fi

    local probe_url="http://127.0.0.1:${container_port}"
    local code="" i
    if [ -n "${host_port}" ]; then
        for i in $(seq 1 30); do
            code="$(curl_in_app "${cid}" -s -o /dev/null -w '%{http_code}' "${probe_url}/" || true)"
            [ "${code}" = "200" ] && break
            sleep 1
        done
        check "${variant}: GET / is 200 on container port ${container_port}" test "${code}" = "200"
    fi

    if [ "${code:-}" != "200" ]; then
        # The page never came up; later probes would be noise on top of the
        # recorded failure.
        teardown_variant "${variant}" "${project}" "${cid}"
        return
    fi

    # Read before the functional checks, then keep this container in the shared window.
    local restart_before="" observed_at
    if ! restart_before="$(read_restart_count "${cid}" 2>"${WORK}/restart-before.${variant}.err")"; then
        restart_before=""
        fail "${variant}: RestartCount is unreadable before the wait"
    fi
    observed_at="$(date +%s)"

    # Unauthenticated status must expose setup.problems.
    local login_status login_status_ok=1 keys=""
    if ! login_status="$(curl_in_app "${cid}" -s "${probe_url}/api/login")"; then
        login_status_ok=0
        fail "${variant}: GET /api/login failed"
    elif ! keys="$(printf '%s' "${login_status}" | json_setup_problem_keys 2>"${WORK}/login-status.${variant}.err")"; then
        login_status_ok=0
        fail "${variant}: login status setup.problems is unreadable: $(cat "${WORK}/login-status.${variant}.err")"
    fi
    if [ "${login_status_ok}" -eq 1 ]; then
        printf '%s\n' "${keys}" >"${WORK}/login-status-keys.${variant}.txt"
        if [ "${expected}" = "-" ]; then
            local diagnose=0
            if grep -Eq '^setting\.(unknown|invalid)$' "${WORK}/login-status-keys.${variant}.txt"; then
                fail "${variant}: no setting problem"
                diagnose=1
            else
                pass "${variant}: no setting problem"
            fi
            if grep -qxF -- startup.failed "${WORK}/login-status-keys.${variant}.txt"; then
                echo "  (record) host-dependent startup problem" >&2
                diagnose=1
            fi
            # Positive assertion of the empty-base image: Docker still
            # bind-mounts /etc/hostname, so the startup check inside the default-template
            # container must find that row in its own mountinfo;
            # startup.mapping.no_hostname_line means the premise is gone and is red (this one
            # does not depend on the CI host).
            if grep -qxF -- 'startup.failed/startup.data_mapping/startup.mapping.no_hostname_line' \
                "${WORK}/login-status-keys.${variant}.txt"; then
                fail "${variant}: the container's own mountinfo has no /etc/hostname row (startup.mapping.no_hostname_line)"
                diagnose=1
            else
                pass "${variant}: no startup.mapping.no_hostname_line in the default-template container"
            fi
            if [ "${diagnose}" -eq 1 ]; then
                print_setup_diagnostics "${login_status}" "${cid}"
            fi
        else
            if grep -qxF -- "${expected}" "${WORK}/login-status-keys.${variant}.txt"; then
                pass "${variant}: setup.problems contains ${expected}"
            else
                fail "${variant}: setup.problems contains ${expected}"
                print_setup_diagnostics "${login_status}" "${cid}"
            fi
        fi
    fi

    # Bootstrap the real image using the newest code from its actual log event.
    # Keep response bodies and headers separate; every authenticated request uses
    # both the cookie and the request token returned by setup.
    local setup_code setup_hdr setup_body setup_status cookie token
    docker logs "${cid}" >"${WORK}/setup-log.${variant}.stdout" 2>"${WORK}/setup-log.${variant}.stderr"
    setup_code="$(python3 "${SCRIPT_DIR}/setup-code.py" <"${WORK}/setup-log.${variant}.stdout")"
    if [ "${variant}" = baseline ]; then
        local preceding_code="${setup_code}" restart_status
        docker restart "${cid}" >"${WORK}/restart.${variant}.stdout" 2>"${WORK}/restart.${variant}.stderr"
        for i in $(seq 1 30); do
            restart_status="$(curl_in_app "${cid}" -s -o /dev/null -w '%{http_code}' "${probe_url}/api/login" || true)"
            [ "${restart_status}" = 200 ] && break
            sleep 1
        done
        check "${variant}: status responds after restart" test "${restart_status}" = 200
        docker logs "${cid}" >"${WORK}/setup-log.${variant}.stdout" 2>"${WORK}/setup-log.${variant}.stderr"
        setup_code="$(python3 "${SCRIPT_DIR}/setup-code.py" <"${WORK}/setup-log.${variant}.stdout")"
        check "${variant}: restart issues a different code" test "${setup_code}" != "${preceding_code}"
        restart_status="$(curl_in_app "${cid}" -s -o /dev/null -w '%{http_code}' -X POST \
            -H "Origin: ${probe_url}" -H 'Content-Type: application/json' \
            -d "{\"code\":\"${preceding_code}\",\"new_password\":\"${NEW_PASSWORD}\"}" \
            "${probe_url}/api/setup")"
        check "${variant}: preceding process code is rejected" test "${restart_status}" = 403
        restart_before="$(read_restart_count "${cid}")"
        observed_at="$(date +%s)"
    fi
    check "${variant}: GET /api/login never exposes the code" \
        bash -c '! printf "%s" "$1" | grep -qF "$2"' _ "${login_status}" "${setup_code}"
    setup_hdr="${WORK}/setup.${variant}.hdr"
    setup_body="${WORK}/setup.${variant}.json"
    curl_in_app "${cid}" -s -D /dev/stderr -X POST \
        -H "Origin: ${probe_url}" -H 'Content-Type: application/json' \
        -d "{\"code\":\"${setup_code}\",\"new_password\":\"${NEW_PASSWORD}\"}" \
        "${probe_url}/api/setup" >"${setup_body}" 2>"${setup_hdr}" || true
    setup_status="$(awk 'NR == 1 {print $2}' "${setup_hdr}")"
    check "${variant}: POST /api/setup is 200" test "${setup_status}" = "200"
    cookie="$(grep -i '^set-cookie:' "${setup_hdr}" | sed 's/^[Ss]et-[Cc]ookie: //' | cut -d';' -f1)" || true
    check "${variant}: setup set a session cookie" test -n "${cookie}"
    token="$(json_token "${setup_body}" 2>"${WORK}/setup.${variant}.token.err" || true)"
    check "${variant}: setup returned a request token" test -n "${token}"
    local reuse_status
    reuse_status="$(curl_in_app "${cid}" -s -o /dev/null -w '%{http_code}' -X POST \
        -H "Origin: ${probe_url}" -H 'Content-Type: application/json' \
        -d "{\"code\":\"${setup_code}\",\"new_password\":\"${NEW_PASSWORD}\"}" \
        "${probe_url}/api/setup")"
    check "${variant}: setup cannot overwrite an account" test "${reuse_status}" = 409

    if [ "${expected}" != "-" ]; then
        local authenticated_login_status blocked="" blocked_ok=1
        if ! authenticated_login_status="$(curl_in_app "${cid}" -s \
            -H "Cookie: ${cookie}" -H "X-NVM-Token: ${token}" \
            "${probe_url}/api/login")"; then
            blocked_ok=0
            fail "${variant}: GET /api/login with the session failed"
        elif ! blocked="$(printf '%s' "${authenticated_login_status}" | json_update_blocked_keys 2>"${WORK}/authenticated_login_status.${variant}.err")"; then
            blocked_ok=0
            fail "${variant}: login status update_blocked is unreadable: $(cat "${WORK}/authenticated_login_status.${variant}.err")"
        fi
        if [ "${blocked_ok}" -eq 1 ]; then
            printf '%s\n' "${blocked}" >"${WORK}/blocked-keys.${variant}.txt"
            check "${variant}: update_blocked contains setup" \
                grep -qxF -- setup "${WORK}/blocked-keys.${variant}.txt"
        fi

        local plan_code plan_response
        plan_response="$(curl_in_app "${cid}" -s -w '\n%{http_code}' -X POST \
            -H "Origin: ${probe_url}" -H "Cookie: ${cookie}" -H "X-NVM-Token: ${token}" \
            -H 'Content-Type: application/json' \
            -d '{"source":{"kind":"official","version":"580.178.04","gpus":[]}}' \
            "${probe_url}/api/plans")" || true
        plan_code="${plan_response##*$'\n'}"
        printf '%s' "${plan_response%$'\n'*}" >"${WORK}/plan.${variant}.json"
        check "${variant}: plan creation is 409 setup.blocked" \
            bash -c "test '${plan_code}' = 409 && grep -q 'setup.blocked' '${WORK}/plan.${variant}.json'"
    else
        echo "  (record) startup check through the program's own namespace mode is host-dependent; see docker logs below"
    fi

    record_observation "${variant}" "${project}" "${cid}" "${restart_before}" "${observed_at}"
}

# A short-lived process exercises Docker's restart policy and the same predicate
# as the App probes. It has no host namespaces, privileges, data, or published ports.
start_restart_control() {
    local variant=crash-restart project="nvmct-${RUN_ID}-crash-restart"
    local file="${WORK}/compose.${project}.yaml" cid="" restart_before=""
    cat > "${file}" <<EOF
services:
  ${SERVICE}:
    image: ${CURL_IMAGE}
    network_mode: bridge
    restart: unless-stopped
    entrypoint: ["/bin/sh", "-c", "sleep 1; exit 1"]
EOF
    nvm_cleanup_track "project:${project}"
    if ! docker compose -p "${project}" -f "${file}" up -d >"${WORK}/up.${variant}.log" 2>&1; then
        fail "${variant}: docker compose up failed (see ${WORK}/up.${variant}.log)"
    elif ! cid="$(docker compose -p "${project}" -f "${file}" ps -aq "${SERVICE}")" || [ -z "${cid}" ]; then
        fail "${variant}: compose reported no container"
    elif ! restart_before="$(read_restart_count "${cid}" 2>"${WORK}/restart-before.${variant}.err")"; then
        fail "${variant}: RestartCount is unreadable before the wait"
    else
        record_observation "${variant}" "${project}" "${cid}" "${restart_before}" "$(date +%s)"
        return
    fi
    teardown_variant "${variant}" "${project}" "${cid}"
}

finish_observation() { # finish_observation <index>
    local index="$1"
    local variant="${OBSERVED_VARIANTS[index]}" project="${OBSERVED_PROJECTS[index]}"
    local cid="${OBSERVED_CONTAINERS[index]}" restart_before="${OBSERVED_RESTARTS[index]}"
    local restart_after="" elapsed
    elapsed=$(( $(date +%s) - OBSERVED_AT[index] ))
    check "${variant}: observed for at least 30s (${elapsed}s)" test "${elapsed}" -ge 30
    if ! restart_after="$(read_restart_count "${cid}" 2>"${WORK}/restart-after.${variant}.err")"; then
        restart_after=""
        fail "${variant}: RestartCount is unreadable after the wait"
    fi
    if [ -n "${restart_before}" ] && [ -n "${restart_after}" ]; then
        printf '%s: RestartCount %s -> %s after %ss\n' "${variant}" "${restart_before}" "${restart_after}" "${elapsed}"
        if [ "${variant}" = crash-restart ]; then
            check "${variant}: the process actually restarted" test "${restart_after}" -gt "${restart_before}"
            check "${variant}: the stability predicate rejects the crashing process" \
                not_restart_stable "${restart_before}" "${restart_after}"
        else
            check "${variant}: RestartCount unchanged after 30s" \
                restart_count_unchanged "${restart_before}" "${restart_after}"
        fi
    fi

    # Every Docker log line must be an ASCII-only JSON object.
    local ascii_ok=1
    if ! docker logs "${cid}" 2>&1 | python3 "${LOG_CHECK}"; then
        ascii_ok=0
    fi
    check "${variant}: docker logs lines are ASCII-only JSON objects" test "${ascii_ok}" = 1
    teardown_variant "${variant}" "${project}" "${cid}"
}

start_restart_control

# The unmodified template must have no setup problems; startup problems depend on the CI host.
probe_variant baseline - 8199
probe_variant no-pid-host startup.failed/startup.pid_host 8199
probe_variant no-data-mount startup.failed/startup.data_mapping 8199
# runtime.io.containerd.runc.v2 can only start when the daemon offers that
# runtime; when it does not, record UNVERIFIED
# instead of passing off an arbitrary `up` failure as a tested scenario.
if docker info -f '{{json .Runtimes}}' 2>/dev/null | grep -q 'io.containerd.runc.v2'; then
    probe_variant runtime-containerd startup.failed/startup.runtime 8199
else
    echo "UNVERIFIED: runtime-containerd: this Docker daemon does not list io.containerd.runc.v2" >&2
    echo "  The scenario is not claimed as tested here." >&2
fi
probe_variant extra-env setting.unknown 8199
probe_variant build-timeout-zero setting.invalid 8199
probe_variant port-abc setting.invalid 8199
probe_variant port-18199 - 18199

# The final probe has also completed before this one shared observation wait.
# Thus every recorded count, including the last one, is at least 30 s old.
if [ "${#OBSERVED_VARIANTS[@]}" -gt 0 ]; then
    sleep 30
    for ((index=0; index < ${#OBSERVED_VARIANTS[@]}; index++)); do
        finish_observation "${index}"
    done
fi

finish_tests "container tests: ${FAILURES} assertion(s) failed" \
    "container tests: all assertions passed"
