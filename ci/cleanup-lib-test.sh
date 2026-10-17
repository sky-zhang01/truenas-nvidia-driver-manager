#!/usr/bin/env bash
# Exercise the real cleanup functions and extracted build-script consumers.
# A local Docker stub logs calls and simulates failures, residue and ownership;
# no daemon is used.
set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
LIB="${SCRIPT_DIR}/cleanup-lib.sh"
[ -f "${LIB}" ] || { echo "cleanup-lib-test: missing ${LIB}" >&2; exit 1; }
CALLER="${REPO_ROOT}/ci/build-script/test.sh"
[ -f "${CALLER}" ] || { echo "cleanup-lib-test: missing ${CALLER}" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

STUB="${WORK}/bin"
STATE="${WORK}/state"
mkdir -p "${STUB}" "${STATE}"

cat >"${STUB}/docker" <<'DOCKER_STUB'
#!/bin/sh
# The Docker stub records calls and simulates the state markers.
state="${NVM_STUB_STATE:?NVM_STUB_STATE is not set}"
printf '%s\n' "$*" >> "${state}/calls.log"
# Scalarize $* first: ${*##pat} operates per positional parameter and rejoins,
# which is not a match over the whole string.
args="$*"
case "$1" in
    compose)
        # docker compose -p <project> -f <file> down -v --remove-orphans
        project="" prev=""
        for arg in "$@"; do
            [ "${prev}" = "-p" ] && project="${arg}"
            prev="${arg}"
        done
        if [ -f "${state}/down-fail.${project}" ]; then
            echo "stub: injected down failure" >&2
            exit 17
        fi
        # A successful down clears the residue markers unless they are pinned persistent.
        if [ ! -f "${state}/residue-persist.${project}" ]; then
            /bin/rm -f "${state}/ps.${project}" "${state}/vols.${project}"
        fi
        exit 0
        ;;
    ps)
        # docker ps -aq --filter label=com.docker.compose.project=<p>
        # docker ps -aq --filter id=<id>
        case "${args}" in
            *"label=com.docker.compose.project="*)
                project="${args##*label=com.docker.compose.project=}"
                if [ -f "${state}/ps-fail.${project}" ]; then
                    echo "stub: injected ps failure" >&2
                    exit 18
                fi
                [ -f "${state}/ps.${project}" ] && cat "${state}/ps.${project}"
                exit 0
                ;;
            *"--filter id="*)
                id="${args##*--filter id=}"
                if [ -f "${state}/ps-id-fail" ]; then
                    echo "stub: injected id query failure" >&2
                    exit 18
                fi
                if [ -f "${state}/ctr.${id}" ] || [ -f "${state}/ctr-survive.${id}" ]; then
                    printf '%s\n' "${id}"
                fi
                exit 0
                ;;
        esac
        exit 0
        ;;
    volume)
        case "$2" in
            ls)
                case "${args}" in
                    *"label=com.docker.compose.project="*)
                        project="${args##*label=com.docker.compose.project=}"
                        if [ -f "${state}/vols-fail.${project}" ]; then
                            echo "stub: injected volume ls failure" >&2
                            exit 19
                        fi
                        [ -f "${state}/vols.${project}" ] && cat "${state}/vols.${project}"
                        exit 0
                        ;;
                    *"--filter name="*)
                        name="${args##*--filter name=}"
                        if [ -f "${state}/vol-ls-fail" ]; then
                            echo "stub: injected volume ls failure" >&2
                            exit 19
                        fi
                        # Long-listing special case: the
                        # exact match is the first line, followed by 100k other
                        # matches — without materializing 100k state files.
                        if [ "${name}" = "vol-big" ]; then
                            printf 'vol-big\n'
                            seq -f 'vol-big-%g' 1 100000
                            exit 0
                        fi
                        # Match substrings like the real daemon.
                        for v in "${state}"/vol-exists.*; do
                            [ -e "${v}" ] || continue
                            base="${v##*vol-exists.}"
                            case "${base}" in
                                *"${name}"*) printf '%s\n' "${base}" ;;
                            esac
                        done
                        exit 0
                        ;;
                esac
                exit 0
                ;;
            inspect)
                # docker volume inspect -f <template> <name>
                for name in "$@"; do :; done
                if [ -f "${state}/vol-inspect-fail" ]; then
                    echo "stub: injected inspect failure" >&2
                    exit 20
                fi
                if [ -f "${state}/vol-exists.${name}" ]; then
                    cat "${state}/vol-label.${name}" 2>/dev/null || printf '\n'
                    exit 0
                fi
                echo "Error response from daemon: no such volume" >&2
                exit 1
                ;;
            rm)
                for name in "$@"; do :; done
                # The volume-in-use dependency: rm must fail while the blocker
                # container exists.
                if [ -f "${state}/vol-blocked-by.${name}" ]; then
                    blocker="$(cat "${state}/vol-blocked-by.${name}")"
                    if [ -f "${state}/ctr.${blocker}" ]; then
                        echo "stub: volume ${name} is in use by ${blocker}" >&2
                        exit 21
                    fi
                fi
                if [ -f "${state}/vol-rm-fail.${name}" ]; then
                    echo "stub: injected volume rm failure" >&2
                    # Simulate rm failing after the volume has disappeared.
                    [ -f "${state}/vol-rm-fail-gone.${name}" ] && /bin/rm -f "${state}/vol-exists.${name}"
                    exit 21
                fi
                [ -f "${state}/vol-survive.${name}" ] || /bin/rm -f "${state}/vol-exists.${name}"
                printf '%s\n' "${name}"
                exit 0
                ;;
            create)
                prev="" label=""
                for arg in "$@"; do
                    if [ "${prev}" = "--label" ]; then
                        case "${arg}" in
                            nvm.ci.run=*) label="${arg#nvm.ci.run=}" ;;
                        esac
                    fi
                    prev="${arg}"
                done
                seq="$(cat "${state}/create-seq" 2>/dev/null || echo 0)"
                seq=$((seq + 1))
                printf '%s' "${seq}" > "${state}/create-seq"
                name="stubvol-${seq}"
                touch "${state}/vol-exists.${name}"
                printf '%s\n' "${label}" > "${state}/vol-label.${name}"
                printf '%s\n' "${name}"
                exit 0
                ;;
        esac
        echo "stub docker: unhandled volume call: $*" >&2
        exit 99
        ;;
    cp)
        if [ "$2" = - ]; then cat > "${state}/copied.tar"; fi
        exit 0
        ;;
    start) exit "${NVM_STUB_START_RC:-0}" ;;
    image)
        case "$2" in
            ls)
                [ ! -f "${state}/image-ls-fail" ] || exit 24
                [ ! -f "${state}/image-name" ] || cat "${state}/image-name"
                exit 0
                ;;
            rm)
                [ ! -f "${state}/image-rm-fail" ] || exit 25
                [ -f "${state}/image-survive" ] || /bin/rm -f "${state}/image-name"
                exit 0
                ;;
        esac
        exit 99
        ;;
    create)
        # Return a deterministic fake container ID (like the real daemon, the
        # ID is not the name) and records the name→ID mapping for rm.
        prev="" cname=""
        for arg in "$@"; do
            [ "${prev}" = "--name" ] && cname="${arg}"
            prev="${arg}"
        done
        cid="cid$(printf '%s' "${cname}" | cksum | awk '{print $1}')"
        touch "${state}/ctr.${cid}"
        printf '%s' "${cid}" > "${state}/ctr-name.${cname}"
        printf '%s\n' "${cid}"
        exit 0
        ;;
    rm)
        # The real daemon accepts names and IDs alike; resolve to the ID first.
        for id in "$@"; do :; done
        real="${id}"
        [ -f "${state}/ctr-name.${id}" ] && real="$(cat "${state}/ctr-name.${id}")"
        # The container-holds-volume dependency: rm must fail while the marked
        # volume exists.
        if [ -f "${state}/ctr-blocked-by-vol.${real}" ]; then
            bvol="$(cat "${state}/ctr-blocked-by-vol.${real}")"
            if [ -f "${state}/vol-exists.${bvol}" ]; then
                echo "stub: container ${real} holds volume ${bvol}" >&2
                exit 22
            fi
        fi
        if [ -f "${state}/ctr-rm-fail.${id}" ] || [ -f "${state}/ctr-rm-fail.${real}" ]; then
            echo "stub: injected container rm failure" >&2
            exit 22
        fi
        /bin/rm -f "${state}/ctr.${real}" "${state}/ctr-name.${id}"
        printf '%s\n' "${real}"
        exit 0
        ;;
esac
echo "stub docker: unhandled call: $*" >&2
exit 99
DOCKER_STUB
chmod 0700 "${STUB}/docker"

cat >"${STUB}/rm" <<'RM_STUB'
#!/bin/sh
# The rm stub:
# fails `rm -rf` only for the marked evidence dir; everything else goes to the
# real rm.
state="${NVM_STUB_STATE:?NVM_STUB_STATE is not set}"
if [ "${1:-}" = "-rf" ] && [ -f "${state}/rm-evidence-fail" ]; then
    target="$(cat "${state}/rm-evidence-fail")"
    if [ "${2:-}" = "${target}" ]; then
        echo "stub rm: injected evidence removal failure" >&2
        exit 23
    fi
fi
exec /bin/rm "$@"
RM_STUB
chmod 0700 "${STUB}/rm"

export NVM_STUB_STATE="${STATE}"
export PATH="${STUB}:${PATH}"

# shellcheck source=cleanup-lib.sh
. "${LIB}"

. "${SCRIPT_DIR}/test-lib.sh"

# finish exits, so it is observed in a subshell;
# the ledger is inherited and the stub records to the filesystem, which the
# parent can read.
cleanup_one_for_test() {
    case "$1" in
        project:*) nvm_compose_teardown "${1#project:}" "${WORK}/compose.yaml" "${WORK}/fin.${1#project:}" ;;
        container:*) nvm_container_remove "${1#container:}" ;;
        volume:*) nvm_volume_remove "${1#volume:}" "tok-this-run" ;;
        *) return 1 ;;
    esac
}

: > "${STATE}/calls.log"
check "clean compose teardown returns 0" \
    nvm_compose_teardown p-clean "${WORK}/compose.yaml" "${WORK}/t.clean"
check "clean teardown wrote the down evidence" test -f "${WORK}/t.clean.down.txt"
check "clean teardown called compose down" \
    grep -q '^compose -p p-clean ' "${STATE}/calls.log"

touch "${STATE}/down-fail.p-down-fail"
if nvm_compose_teardown p-down-fail "${WORK}/compose.yaml" "${WORK}/t.downfail" 2>/dev/null; then
    fail "a failed down must not pass"
else
    pass "a failed down returns non-zero"
fi
rm -f "${STATE}/down-fail.p-down-fail"

touch "${STATE}/ps-fail.p-qfail"
if nvm_compose_teardown p-qfail "${WORK}/compose.yaml" "${WORK}/t.qfail" 2>/dev/null; then
    fail "a failed container query must not pass"
else
    pass "a failed container query returns non-zero"
fi
rm -f "${STATE}/ps-fail.p-qfail"
touch "${STATE}/vols-fail.p-vqfail"
if nvm_compose_teardown p-vqfail "${WORK}/compose.yaml" "${WORK}/t.vqfail" 2>/dev/null; then
    fail "a failed volume query must not pass"
else
    pass "a failed volume query returns non-zero"
fi
rm -f "${STATE}/vols-fail.p-vqfail"

printf 'ctr-left-1\n' > "${STATE}/ps.p-residue"
touch "${STATE}/residue-persist.p-residue"
if nvm_compose_teardown p-residue "${WORK}/compose.yaml" "${WORK}/t.residue" 2>/dev/null; then
    fail "residue after a successful down must not pass"
else
    pass "residue after a successful down returns non-zero"
fi
NVM_CLEANUP_ITEMS=()
nvm_cleanup_track "project:p-residue"
# The consumer rule: no proof, no untrack.
if nvm_compose_teardown p-residue "${WORK}/compose.yaml" "${WORK}/t.residue2" 2>/dev/null; then
    nvm_cleanup_untrack "project:p-residue"
fi
if printf '%s\n' ${NVM_CLEANUP_ITEMS[@]+"${NVM_CLEANUP_ITEMS[@]}"} | grep -qxF "project:p-residue"; then
    pass "an unproven teardown keeps the item registered"
else
    fail "an unproven teardown dropped the item"
fi

mkdir -p "${WORK}/ev-kept"
: > "${STATE}/calls.log"
rc=0
( nvm_cleanup_one() { cleanup_one_for_test "$1"; }
  nvm_cleanup_finish 0 "${WORK}/ev-kept" ) || rc=$?
check "finish retried the still-registered project" \
    grep -q '^compose -p p-residue ' "${STATE}/calls.log"
check "finish with unproven residue exits non-zero" test "${rc}" -ne 0
check "finish kept the evidence dir" test -d "${WORK}/ev-kept"
rm -f "${STATE}/residue-persist.p-residue" "${STATE}/ps.p-residue"
mkdir -p "${WORK}/ev-clean"
: > "${STATE}/calls.log"
rc=0
( nvm_cleanup_one() { cleanup_one_for_test "$1"; }
  nvm_cleanup_finish 0 "${WORK}/ev-clean" ) || rc=$?
check "finish with a proven teardown exits 0" test "${rc}" -eq 0
check "finish removed the evidence dir on a clean run" test ! -e "${WORK}/ev-clean"
check "the retried project was torn down on the second pass" \
    grep -q '^compose -p p-residue ' "${STATE}/calls.log"
NVM_CLEANUP_ITEMS=()

mkdir -p "${WORK}/ev-rc7"
rc=0
( nvm_cleanup_one() { cleanup_one_for_test "$1"; }
  nvm_cleanup_finish 7 "${WORK}/ev-rc7" ) || rc=$?
check "finish preserves the original rc=7" test "${rc}" -eq 7
check "evidence kept on an original failure" test -d "${WORK}/ev-rc7"
mkdir -p "${WORK}/ev-rm-fail"
printf '%s' "${WORK}/ev-rm-fail" > "${STATE}/rm-evidence-fail"
rc=0
( nvm_cleanup_one() { cleanup_one_for_test "$1"; }
  nvm_cleanup_finish 0 "${WORK}/ev-rm-fail" ) || rc=$?
check "finish turns an unremovable evidence dir into rc=1" test "${rc}" -eq 1
rm -f "${STATE}/rm-evidence-fail"

touch "${STATE}/ctr.abc123"
check "container remove: clean" nvm_container_remove abc123
check "container state really removed" test ! -f "${STATE}/ctr.abc123"
touch "${STATE}/ctr.def456" "${STATE}/ctr-rm-fail.def456"
if nvm_container_remove def456 2>/dev/null; then
    fail "container rm failure must not pass"
else
    pass "container rm failure returns non-zero"
fi
rm -f "${STATE}/ctr-rm-fail.def456" "${STATE}/ctr.def456"
touch "${STATE}/ctr.survive1" "${STATE}/ctr-survive.survive1"
if nvm_container_remove survive1 2>/dev/null; then
    fail "a surviving container must not pass"
else
    pass "a surviving container returns non-zero"
fi
rm -f "${STATE}/ctr.survive1" "${STATE}/ctr-survive.survive1"
touch "${STATE}/ctr.query1" "${STATE}/ps-id-fail"
if nvm_container_remove query1 2>/dev/null; then
    fail "a failed existence query must not pass"
else
    pass "a failed existence query returns non-zero"
fi
rm -f "${STATE}/ctr.query1" "${STATE}/ps-id-fail"
# rm errors but the container
# is in fact absent: proven by the listing, counts as removed.
touch "${STATE}/ctr-rm-fail.gone001"
check "an already-absent container counts as removed" nvm_container_remove gone001
rm -f "${STATE}/ctr-rm-fail.gone001"

touch "${STATE}/vol-exists.vol-own"
printf 'tok-this-run\n' > "${STATE}/vol-label.vol-own"
check "own volume is removed" nvm_volume_remove vol-own tok-this-run
check "own volume state really removed" test ! -f "${STATE}/vol-exists.vol-own"
touch "${STATE}/vol-exists.vol-foreign"
printf 'tok-OTHER-run\n' > "${STATE}/vol-label.vol-foreign"
: > "${STATE}/calls.log"
if nvm_volume_remove vol-foreign tok-this-run 2>/dev/null; then
    fail "a foreign volume must not be removed"
else
    pass "a foreign volume is refused"
fi
if grep -qxF "volume rm vol-foreign" "${STATE}/calls.log"; then
    fail "the refusal still issued docker volume rm"
else
    pass "the refusal never issued docker volume rm"
fi
check "the foreign volume still exists" test -f "${STATE}/vol-exists.vol-foreign"
touch "${STATE}/vol-exists.vol-stays" "${STATE}/vol-survive.vol-stays"
printf 'tok-this-run\n' > "${STATE}/vol-label.vol-stays"
if nvm_volume_remove vol-stays tok-this-run 2>/dev/null; then
    fail "a surviving volume must not pass"
else
    pass "a surviving volume returns non-zero"
fi
rm -f "${STATE}/vol-exists.vol-stays" "${STATE}/vol-survive.vol-stays"
touch "${STATE}/vol-exists.vol-broken" "${STATE}/vol-inspect-fail"
printf 'tok-this-run\n' > "${STATE}/vol-label.vol-broken"
if nvm_volume_remove vol-broken tok-this-run 2>/dev/null; then
    fail "a failed volume inspect must not pass"
else
    pass "a failed volume inspect returns non-zero"
fi
check "a volume already gone counts as removed even when inspect errors" \
    nvm_volume_remove vol-never-existed tok-this-run
rm -f "${STATE}/vol-inspect-fail"
touch "${STATE}/vol-exists.vol-rmfail" "${STATE}/vol-rm-fail.vol-rmfail"
printf 'tok-this-run\n' > "${STATE}/vol-label.vol-rmfail"
if nvm_volume_remove vol-rmfail tok-this-run 2>/dev/null; then
    fail "a failed volume rm must not pass"
else
    pass "a failed volume rm returns non-zero"
fi
rm -f "${STATE}/vol-exists.vol-rmfail" "${STATE}/vol-rm-fail.vol-rmfail"
touch "${STATE}/vol-exists.vol-rmgone" "${STATE}/vol-rm-fail.vol-rmgone" "${STATE}/vol-rm-fail-gone.vol-rmgone"
printf 'tok-this-run\n' > "${STATE}/vol-label.vol-rmgone"
check "an rm error on an actually-gone volume counts as removed" \
    nvm_volume_remove vol-rmgone tok-this-run

touch "${STATE}/ctr.mix1"
touch "${STATE}/vol-exists.vol-mix"
printf 'tok-this-run\n' > "${STATE}/vol-label.vol-mix"
mkdir -p "${WORK}/ev-mix"
: > "${STATE}/calls.log"
rc=0
( nvm_cleanup_one() { cleanup_one_for_test "$1"; }
  NVM_CLEANUP_ITEMS=()
  nvm_cleanup_track "container:mix1"
  nvm_cleanup_track "volume:vol-mix"
  nvm_cleanup_track "project:p-mix"
  nvm_cleanup_finish 0 "${WORK}/ev-mix" ) || rc=$?
check "mixed finish exits 0" test "${rc}" -eq 0
check "mixed finish removed the container" test ! -f "${STATE}/ctr.mix1"
check "mixed finish removed the volume" test ! -f "${STATE}/vol-exists.vol-mix"
check "mixed finish ran compose down" grep -q '^compose -p p-mix ' "${STATE}/calls.log"
check "mixed finish removed the evidence dir" test ! -e "${WORK}/ev-mix"

touch "${STATE}/ctr.blocker1"
touch "${STATE}/vol-exists.vol-dep"
printf 'tok-this-run\n' > "${STATE}/vol-label.vol-dep"
printf 'blocker1' > "${STATE}/vol-blocked-by.vol-dep"
mkdir -p "${WORK}/ev-multipass"
rc=0
( nvm_cleanup_one() { cleanup_one_for_test "$1"; }
  NVM_CLEANUP_ITEMS=()
  nvm_cleanup_track "volume:vol-dep"
  nvm_cleanup_track "container:blocker1"
  nvm_cleanup_finish 0 "${WORK}/ev-multipass" ) || rc=$?
check "multi-pass finish exits 0 once the blocker is gone" test "${rc}" -eq 0
check "multi-pass removed the blocked volume" test ! -f "${STATE}/vol-exists.vol-dep"
check "multi-pass removed the blocker container" test ! -f "${STATE}/ctr.blocker1"
check "multi-pass removed the evidence dir" test ! -e "${WORK}/ev-multipass"

# For name=vol-big the stub returns a listing whose first line is
# the exact match, followed by 100k other matches; with a pipeline, grep -q
# exits after the first line and printf dies of SIGPIPE, misjudging "present"
# as "absent".
if nvm_volume_absent vol-big 2>/dev/null; then
    fail "a long listing containing the exact match was judged absent"
else
    pass "a long listing containing the exact match is not 'absent'"
fi

# The function body is extracted verbatim from ci/build-script/test.sh (no
# copied logic) and driven against the stub docker; a drift of the function
# (e.g. registration occurring after verification) fails this case.
awk '
    index($0, "new_volume() {") == 1 {f=1}
    f {print}
    f && $0 == "}" {exit}
' "${CALLER}" > "${WORK}/new_volume.fn"
grep -q 'docker volume create' "${WORK}/new_volume.fn" \
    && grep -q 'nvm_cleanup_track "volume:${NEW_VOLUME}"' "${WORK}/new_volume.fn" \
    || { echo "cleanup-lib-test: new_volume extraction drifted" >&2; exit 1; }
. "${WORK}/new_volume.fn"
RUN_TOKEN="tok-this-run"
NVM_CLEANUP_ITEMS=()
touch "${STATE}/vol-inspect-fail"
if new_volume probe; then
    fail "new_volume must fail when the label read-back fails"
else
    pass "the real new_volume fails on a failed label read-back"
fi
if printf '%s\n' ${NVM_CLEANUP_ITEMS[@]+"${NVM_CLEANUP_ITEMS[@]}"} | grep -qxF "volume:${NEW_VOLUME}"; then
    pass "the real new_volume keeps a failed-verification volume registered"
else
    fail "the real new_volume dropped a failed-verification volume from the ledger"
fi
rm -f "${STATE}/vol-inspect-fail"
mkdir -p "${WORK}/ev-unverified"
rc=0
( nvm_cleanup_one() { cleanup_one_for_test "$1"; }
  nvm_cleanup_finish 0 "${WORK}/ev-unverified" ) || rc=$?
check "finish later removes the previously unverified volume" test "${rc}" -eq 0
check "the previously unverified volume is gone" test ! -f "${STATE}/vol-exists.${NEW_VOLUME}"

# The SEED_INPUTS/SEED_ARTIFACT registration blocks are extracted verbatim from
# ci/build-script/test.sh; a drift of what they register (e.g. registering names)
# fails this case.
extract_seed_fragment() { # extract_seed_fragment <caller-file> <out>
    awk '
        index($0, "SEED_INPUTS=\"nvm-build-script-seed-") == 1 {f=1}
        f {print}
        f && index($0, "nvm_cleanup_track \"container:${SEED_ARTIFACT_CID}\"") == 1 {found=1; exit}
        END {if (!found) exit 1}
    ' "$1" > "$2" || return 1
    grep -q 'SEED_INPUTS_CID="$(docker create' "$2" \
        && grep -q 'SEED_ARTIFACT_CID="$(docker create' "$2" \
        && grep -q 'nvm_cleanup_track "container:${SEED_INPUTS_CID}"' "$2" \
        && grep -q 'nvm_cleanup_track "container:${SEED_ARTIFACT_CID}"' "$2" \
        || return 1
    # End boundary: the artifact track line must be
    # the fragment's last line; otherwise the end anchor drifted and the
    # fragment may carry unrelated code.
    [ "$(tail -1 "$2")" = 'nvm_cleanup_track "container:${SEED_ARTIFACT_CID}"' ]
}
extract_seed_fragment "${CALLER}" "${WORK}/seed-fragment.sh" \
    || { echo "cleanup-lib-test: seed registration extraction drifted" >&2; exit 1; }
# Negative controls of the extraction guard itself: the artifact
# registering its name, or the artifact block being deleted, must both be
# refused.
sed 's|nvm_cleanup_track "container:${SEED_ARTIFACT_CID}"|nvm_cleanup_track "container:${SEED_ARTIFACT}"|' \
    "${CALLER}" > "${WORK}/drifted-name.sh"
if extract_seed_fragment "${WORK}/drifted-name.sh" "${WORK}/drifted-name.fragment"; then
    fail "extraction guard: a name-registered artifact was not refused"
else
    pass "extraction guard refuses a name-registered artifact"
fi
awk '!/SEED_ARTIFACT/' "${CALLER}" > "${WORK}/drifted-deleted.sh"
if extract_seed_fragment "${WORK}/drifted-deleted.sh" "${WORK}/drifted-deleted.fragment"; then
    fail "extraction guard: a deleted artifact block was not refused"
else
    pass "extraction guard refuses a deleted artifact block"
fi
HDR_VOL=v-hdr BOOT_VOL=v-boot MODS_VOL=v-mods IN_VOL=v-in FIND_VOL=v-find ARTIFACT_VOL=v-artifact
# The fragment's build-container arguments mirror the caller: the platform and
# the image name must be defined too (the seed block requires
# `--platform "${PLATFORM}"` and `"${BUILD_IMAGE}"`).
KERNEL=stub-kernel PLATFORM=linux/amd64 BUILD_IMAGE=stub-image

# Preflight: every `${VAR}` the fragment references must be defined (names the
# fragment assigns itself are exempt). A missing one is a bare `unbound
# variable` abort under `set -u` rather than an assertion failure.
seed_fragment_undefined() { # seed_fragment_undefined <fragment>
    local name assigned missing=""
    assigned="$(grep -oE '^[A-Za-z_][A-Za-z0-9_]*=' "$1" | tr -d '=')"
    while IFS= read -r name; do
        [ -n "${name}" ] || continue
        printf '%s\n' "${assigned}" | grep -qxF "${name}" && continue
        [ -n "${!name+x}" ] || missing="${missing} ${name}"
    done < <(sed 's/#.*//' "$1" \
        | grep -oE '\$\{[A-Za-z_][A-Za-z0-9_]*\}' | tr -d '${}' | sort -u)
    printf '%s' "${missing# }"
}
printf '%s\n' 'echo "${NOT_DEFINED_ANYWHERE}"' > "${WORK}/preflight-undefined.sh"
check "the preflight reports an undefined variable" \
    test "$(seed_fragment_undefined "${WORK}/preflight-undefined.sh")" = "NOT_DEFINED_ANYWHERE"
printf '%s\n' 'echo "${KERNEL}"' > "${WORK}/preflight-defined.sh"
check "the preflight accepts a defined variable" \
    test -z "$(seed_fragment_undefined "${WORK}/preflight-defined.sh")"
undefined="$(seed_fragment_undefined "${WORK}/seed-fragment.sh")"
if [ -n "${undefined}" ]; then
    echo "cleanup-lib-test: the extracted seed fragment references undefined variables: ${undefined}" >&2
    exit 1
fi
NVM_CLEANUP_ITEMS=()
. "${WORK}/seed-fragment.sh"
check "the real seed fragment tracks the create-printed ID, not the name" \
    test "${SEED_INPUTS_CID}" != "${SEED_INPUTS}"
if printf '%s\n' ${NVM_CLEANUP_ITEMS[@]+"${NVM_CLEANUP_ITEMS[@]}"} | grep -qxF "container:${SEED_INPUTS_CID}"; then
    pass "the real seed fragment registered the inputs container by ID"
else
    fail "the real seed fragment did not register the inputs container by ID"
fi
if printf '%s\n' ${NVM_CLEANUP_ITEMS[@]+"${NVM_CLEANUP_ITEMS[@]}"} | grep -qxF "container:${SEED_ARTIFACT_CID}"; then
    pass "the real seed fragment registered the artifact container by ID"
else
    fail "the real seed fragment did not register the artifact container by ID"
fi
mkdir -p "${WORK}/ev-seed"
rc=0
( nvm_cleanup_one() { cleanup_one_for_test "$1"; }
  nvm_cleanup_finish 0 "${WORK}/ev-seed" ) || rc=$?
check "the seed containers are removed by ID" test "${rc}" -eq 0
check "the inputs seed container is gone" test ! -f "${STATE}/ctr.${SEED_INPUTS_CID}"
check "the artifact seed container is gone" test ! -f "${STATE}/ctr.${SEED_ARTIFACT_CID}"

# Hazard control: with the NAME registered and rm-by-name failing, the
# `ps --filter id=<name>` lookup never matches (the id filter only knows IDs),
# so the live container is misjudged as gone.
hazard_cid="$(docker create --name seed-hazard some-image /bin/true)"
NVM_CLEANUP_ITEMS=()
nvm_cleanup_track "container:seed-hazard"
touch "${STATE}/ctr-rm-fail.seed-hazard" "${STATE}/ctr-rm-fail.${hazard_cid}"
mkdir -p "${WORK}/ev-hazard"
rc=0
( nvm_cleanup_one() { cleanup_one_for_test "$1"; }
  nvm_cleanup_finish 0 "${WORK}/ev-hazard" ) || rc=$?
check "name-registered cleanup with a failed rm pretends success (the hazard)" test "${rc}" -eq 0
check "the real container survived — this is why the ID is registered" test -f "${STATE}/ctr.${hazard_cid}"
# The correct path: on a failed rm the ID lookup still finds it and it stays
# registered; once the marker clears it is removed.
NVM_CLEANUP_ITEMS=()
nvm_cleanup_track "container:${hazard_cid}"
mkdir -p "${WORK}/ev-hazard2"
rc=0
( nvm_cleanup_one() { cleanup_one_for_test "$1"; }
  nvm_cleanup_finish 0 "${WORK}/ev-hazard2" ) || rc=$?
check "id-registered cleanup with a failing rm stays red" test "${rc}" -ne 0
check "the id-registered container survives the failed rm" test -f "${STATE}/ctr.${hazard_cid}"
rm -f "${STATE}/ctr-rm-fail.seed-hazard" "${STATE}/ctr-rm-fail.${hazard_cid}"
mkdir -p "${WORK}/ev-hazard3"
rc=0
( nvm_cleanup_one() { cleanup_one_for_test "$1"; }
  nvm_cleanup_finish 0 "${WORK}/ev-hazard3" ) || rc=$?
check "id-registered cleanup removes the container once rm works" test "${rc}" -eq 0
check "the hazard container is really gone" test ! -f "${STATE}/ctr.${hazard_cid}"

# volA←ctrA←volB←ctrB←volC: each pass frees exactly one
# link and the chain needs five passes; any fixed pass cap below five leaves
# residue behind.
touch "${STATE}/ctr.chainA" "${STATE}/ctr.chainB"
touch "${STATE}/vol-exists.vol-chainA" "${STATE}/vol-exists.vol-chainB" "${STATE}/vol-exists.vol-chainC"
printf 'tok-this-run\n' > "${STATE}/vol-label.vol-chainA"
printf 'tok-this-run\n' > "${STATE}/vol-label.vol-chainB"
printf 'tok-this-run\n' > "${STATE}/vol-label.vol-chainC"
printf 'chainA' > "${STATE}/vol-blocked-by.vol-chainA"
printf 'vol-chainB' > "${STATE}/ctr-blocked-by-vol.chainA"
printf 'chainB' > "${STATE}/vol-blocked-by.vol-chainB"
printf 'vol-chainC' > "${STATE}/ctr-blocked-by-vol.chainB"
mkdir -p "${WORK}/ev-chain"
rc=0
( nvm_cleanup_one() { cleanup_one_for_test "$1"; }
  NVM_CLEANUP_ITEMS=()
  nvm_cleanup_track "volume:vol-chainA"
  nvm_cleanup_track "container:chainA"
  nvm_cleanup_track "volume:vol-chainB"
  nvm_cleanup_track "container:chainB"
  nvm_cleanup_track "volume:vol-chainC"
  nvm_cleanup_finish 0 "${WORK}/ev-chain" ) || rc=$?
check "a five-link dependency chain finishes with rc 0" test "${rc}" -eq 0
check "the chain left no volumes" bash -c '! compgen -G "$1/vol-exists.vol-chain*" >/dev/null' _ "${STATE}"
check "the chain left no containers" bash -c '! compgen -G "$1/ctr.chain*" >/dev/null' _ "${STATE}"
check "the chain removed the evidence dir" test ! -e "${WORK}/ev-chain"

# Exercise helper-image cleanup through the shared implementation.
printf 'helper-image\n' > "${STATE}/image-name"
touch "${STATE}/image-ls-fail"
if nvm_image_remove helper-image; then fail "failed image listing was accepted"; else pass "failed image listing cannot prove removal"; fi
rm -f "${STATE}/image-ls-fail"
printf 'helper-image\n' > "${STATE}/image-name"
touch "${STATE}/image-survive"
if nvm_image_remove helper-image; then fail "surviving image was accepted"; else pass "surviving image is rejected"; fi
rm -f "${STATE}/image-survive"
check "helper image removal is proven" nvm_image_remove helper-image
touch "${STATE}/image-rm-fail"
check "an already absent image is proven despite rm failure" nvm_image_remove helper-image
rm -f "${STATE}/image-rm-fail"

# Extract the actual build dispatcher and check its installation boundary.
awk '
    index($0, "run_build() {") == 1 {f=1}
    f {print}
    f && $0 == "}" {exit}
' "${CALLER}" > "${WORK}/run-build.fn"
awk '/^seed_root\(\) \{/ {f=1} f {print} f && /^}/ {exit}' \
    "${CALLER}" > "${WORK}/seed-root.fn"
awk '/^OUT_EMPTY=/ {print}' "${CALLER}" > "${WORK}/out-empty.sh"
(
    . "${WORK}/run-build.fn"
    . "${WORK}/seed-root.fn"
    BIN="${WORK}/dispatcher-bin"; IN="${WORK}/dispatcher-in"
    . "${WORK}/out-empty.sh"
    TAR_BIN=tar
    if ! "${TAR_BIN}" --version 2>/dev/null | grep -q 'GNU tar'; then TAR_BIN=gtar; fi
    mkdir -p "${BIN}" "${IN}" "${OUT_EMPTY}"
    : > "${BIN}/555.run"
    write_auto_conf() { :; }
    seed_inputs() { :; }
    BUILD_RUNS=0; BUILD_IMAGE=official-image; HELPER_IMAGE=installed-image
    KIT_VOL=kit; FIND_VOL=find; HDR_VOL=hdr; BOOT_VOL=boot
    MODS_VOL=mods; IN_VOL=in; PLATFORM=linux/amd64; KERNEL=fixture-kernel
    CONTAINER_PATH=/usr/bin:/bin
    : > "${STATE}/calls.log"
    run_build 555
    check "first build retains the real package installer" grep -qF 'official-image /in/install-build-kit.sh' "${STATE}/calls.log"
    check "build output directory is seeded at the container root" grep -qxF "cp - ${LAST_NAME}:/" "${STATE}/calls.log"
    check "build output directory archive contains only root-owned writable /out" python3 - "${STATE}/copied.tar" <<'PY_CHECK'
import sys
import tarfile
with tarfile.open(sys.argv[1]) as archive:
    members = archive.getmembers()
    assert len(members) == 1
    member = members[0]
    assert member.name == "out" and member.isdir()
    assert member.uid == member.gid == 0 and member.mode & 0o300 == 0o300
PY_CHECK
    : > "${STATE}/calls.log"
    run_build 555
    check "subsequent build uses installed dependencies" grep -qF 'installed-image /in/build-driver.sh' "${STATE}/calls.log"
    : > "${STATE}/calls.log"
    NVM_STUB_START_RC=31; export NVM_STUB_START_RC
    run_build 555 "" 12 fail-find
    check "find failure keeps its injected PATH" grep -qF 'PATH=/find-shim:/usr/bin:/bin' "${STATE}/calls.log"
    check "build dispatcher preserves container failure" test "${LAST_RC}" -eq 31
    [ "${FAILURES}" -eq 0 ]
) || fail "build dispatcher controls failed"

finish_tests "cleanup-lib-test: ${FAILURES} control(s) failed" \
    "cleanup-lib-test: all controls passed"
