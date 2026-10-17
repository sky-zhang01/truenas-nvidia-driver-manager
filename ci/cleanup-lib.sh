# Shared teardown for Docker-backed tests. Only registered resources are touched.
# Consumers define nvm_cleanup_one <item>, returning 0 only after proven removal.
# Track each resource before validation; untrack it only after a successful proof.
# nvm_cleanup_finish <original-rc> <evidence-dir> retries while progress is made.
# Original failures are preserved, cleanup failures fail the run, and evidence
# is removed only after complete success. Failed queries never prove absence.
# Volumes must carry the current run token before deletion.

NVM_CLEANUP_ITEMS=()

nvm_cleanup_track() { # nvm_cleanup_track <item>
    NVM_CLEANUP_ITEMS+=("$1")
}

nvm_cleanup_untrack() { # nvm_cleanup_untrack <item> — call only after removal is proven
    local drop="$1" item
    local keep=()
    for item in ${NVM_CLEANUP_ITEMS[@]+"${NVM_CLEANUP_ITEMS[@]}"}; do
        [ "${item}" = "${drop}" ] || keep+=("${item}")
    done
    NVM_CLEANUP_ITEMS=(${keep[@]+"${keep[@]}"})
}

nvm_cleanup_finish() { # nvm_cleanup_finish <original-rc> <evidence-dir>
    local rc="$1" evidence="$2" item
    # Progress-driven retries: items can have removal-order dependencies (a
    # volume is not removable while a container holds it), so each pass removes
    # what it can; another pass runs while the previous one made progress, and
    # a pass with none stops the loop. The ledger is finite and every pass with
    # progress removes at least one item, so the loop always ends.
    local progress
    while [ "${#NVM_CLEANUP_ITEMS[@]}" -gt 0 ]; do
        progress=0
        for item in ${NVM_CLEANUP_ITEMS[@]+"${NVM_CLEANUP_ITEMS[@]}"}; do
            [ -n "${item}" ] || continue
            if nvm_cleanup_one "${item}"; then
                nvm_cleanup_untrack "${item}"
                progress=1
            fi
        done
        [ "${progress}" -eq 1 ] || break
    done
    local failed=0
    for item in ${NVM_CLEANUP_ITEMS[@]+"${NVM_CLEANUP_ITEMS[@]}"}; do
        [ -n "${item}" ] || continue
        echo "cleanup-lib: ${item} is not proven removed; it stays registered" >&2
        failed=1
    done
    if [ "${rc}" -ne 0 ] || [ "${failed}" -ne 0 ]; then
        echo "cleanup-lib: evidence kept in ${evidence}" >&2
        if [ "${rc}" -ne 0 ]; then
            exit "${rc}"
        fi
        exit 1
    fi
    if ! rm -rf "${evidence}"; then
        echo "cleanup-lib: cannot remove evidence dir ${evidence}" >&2
        exit 1
    fi
    exit 0
}

# nvm_compose_teardown <project> <compose-file> <evidence-prefix>
# If any of the three commands — `down -v`, the container residue query, the
# volume residue query — fails or answers non-empty, the project is not torn
# down (a failed query is not "no residue"). Evidence (command output) is
# always written to disk.
nvm_compose_teardown() {
    local project="$1" file="$2" prefix="$3"
    local down_rc=0 remaining="" volumes="" remaining_rc=0 volumes_rc=0
    docker compose -p "${project}" -f "${file}" down -v --remove-orphans \
        >"${prefix}.down.txt" 2>&1 || down_rc=$?
    [ "${down_rc}" -eq 0 ] \
        || echo "cleanup-lib: docker compose down rc=${down_rc} for ${project} (see ${prefix}.down.txt)" >&2
    remaining="$(docker ps -aq --filter "label=com.docker.compose.project=${project}" \
        2>"${prefix}.ps.err")" || remaining_rc=$?
    [ "${remaining_rc}" -eq 0 ] \
        || echo "cleanup-lib: container residue query rc=${remaining_rc} for ${project} (see ${prefix}.ps.err)" >&2
    volumes="$(docker volume ls -q --filter "label=com.docker.compose.project=${project}" \
        2>"${prefix}.volumes.err")" || volumes_rc=$?
    [ "${volumes_rc}" -eq 0 ] \
        || echo "cleanup-lib: volume residue query rc=${volumes_rc} for ${project} (see ${prefix}.volumes.err)" >&2
    [ -z "${remaining}" ] \
        || echo "cleanup-lib: ${project} left containers: ${remaining}" >&2
    [ -z "${volumes}" ] \
        || echo "cleanup-lib: ${project} left volumes: ${volumes}" >&2
    [ "${down_rc}" -eq 0 ] && [ "${remaining_rc}" -eq 0 ] && [ "${volumes_rc}" -eq 0 ] \
        && [ -z "${remaining}" ] && [ -z "${volumes}" ]
}

# Return success only when a successful listing proves the container absent.
nvm_container_absent() {
    local id="$1" listed="" ls_rc=0
    listed="$(docker ps -aq --filter "id=${id}" 2>&1)" || ls_rc=$?
    if [ "${ls_rc}" -ne 0 ]; then
        echo "cleanup-lib: container existence query for ${id} failed: ${listed}" >&2
        return 1
    fi
    [ -z "${listed}" ]
}

# nvm_container_remove <id>
# `rm -f` first, then a successful listing proves it gone; an rm error on a
# container that is in fact absent still counts as removed (proven by the
# listing, not by the error text).
nvm_container_remove() {
    local id="$1"
    local out="" rm_rc=0
    out="$(docker rm -f "${id}" 2>&1)" || rm_rc=$?
    if [ "${rm_rc}" -ne 0 ]; then
        if nvm_container_absent "${id}"; then
            return 0
        fi
        echo "cleanup-lib: docker rm -f ${id} rc=${rm_rc}: ${out}" >&2
        return 1
    fi
    if ! nvm_container_absent "${id}"; then
        echo "cleanup-lib: container ${id} survived docker rm -f" >&2
        return 1
    fi
}

# Return success only when a successful listing has no exact volume match.
nvm_volume_absent() {
    local name="$1"
    nvm_listing_absent "${name}" docker volume ls -q --filter "name=${name}"
}

# A successful listing must omit the exact resource name; query errors fail.
nvm_listing_absent() {
    local name="$1" listed="" ls_rc=0 line
    shift
    listed="$("$@" 2>&1)" || ls_rc=$?
    if [ "${ls_rc}" -ne 0 ]; then
        echo "cleanup-lib: existence query for ${name} failed: ${listed}" >&2
        return 1
    fi
    while IFS= read -r line; do
        [ "${line}" = "${name}" ] && return 1
    done <<<"${listed}"
    return 0
}

# nvm_volume_remove <name> <run-token>
# Only a volume whose read-back label still carries this run's token is removed;
# a volume with someone else's token is neither touched nor counted as this
# run's cleanup.
nvm_volume_remove() {
    local name="$1" token="$2"
    local label="" inspect_rc=0
    label="$(docker volume inspect -f '{{index .Labels "nvm.ci.run"}}' "${name}" 2>&1)" || inspect_rc=$?
    if [ "${inspect_rc}" -ne 0 ]; then
        # A failed inspect splits into "the volume is already gone" (proven by
        # a listing) and "the query is broken" (not cleanup).
        if nvm_volume_absent "${name}"; then
            return 0
        fi
        echo "cleanup-lib: cannot inspect volume ${name}: ${label}" >&2
        return 1
    fi
    if [ "${label}" != "${token}" ]; then
        echo "cleanup-lib: volume ${name} does not carry this run's token; refusing to remove" >&2
        return 1
    fi
    local out="" rm_rc=0
    out="$(docker volume rm "${name}" 2>&1)" || rm_rc=$?
    if [ "${rm_rc}" -ne 0 ]; then
        if nvm_volume_absent "${name}"; then
            return 0
        fi
        echo "cleanup-lib: docker volume rm ${name} rc=${rm_rc}: ${out}" >&2
        return 1
    fi
    if ! nvm_volume_absent "${name}"; then
        echo "cleanup-lib: volume ${name} survived docker volume rm" >&2
        return 1
    fi
}

# Remove a run-owned helper image only after a successful absence query.
nvm_image_absent() { # nvm_image_absent <tag>
    nvm_listing_absent "$1" docker image ls --format '{{.Repository}}:{{.Tag}}'
}

nvm_image_remove() { # nvm_image_remove <tag>
    local tag="$1" out="" rm_rc=0
    out="$(docker image rm -f "${tag}" 2>&1)" || rm_rc=$?
    if nvm_image_absent "${tag}"; then
        return 0
    fi
    echo "cleanup-lib: image ${tag} still exists after docker image rm (rc=${rm_rc}): ${out}" >&2
    return 1
}
