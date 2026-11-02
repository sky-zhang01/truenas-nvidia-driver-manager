#!/usr/bin/env bash
# changes.sh — select the union of checks that read the changed inputs.
#
#   changes.sh [BASE]                   write job switches (stdout or GITHUB_OUTPUT)
#   changes.sh --classify PATH [STATUS] print the jobs one changed path selects
#   changes.sh --hash JOB               content address of JOB's inputs
#   changes.sh --validate               every path is registered, every glob matches
#   changes.sh --controls               run the commands of the selected controls
#
# ci/checks.tsv (plus ci/checks-private.tsv in the internal tree) registers every
# repository path. Columns, tab separated: check, globs separated by ';', control
# command or '-', explanation, context (all|private). Globs use shell pattern
# rules in which '*' also matches '/'.
set -Eeuo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
JOBS="guards controls web compose rust e2e image build_kit osv leak"

# Changes to these files change what every job means, so they force every job.
# The list lives here, not in a table, so a pull request cannot weaken the rule
# it is judged by without editing this file, which is itself on the list and
# therefore visible in review.
FORCE_ALL_GLOBS='ci/checks.tsv;ci/checks-private.tsv;ci/changes.sh;ci/gate.sh;ci/job/*.sh;.github/workflows/*;.gitea/workflows/*'

die() { echo "checks: $*" >&2; exit 1; }

# Parallel arrays, one entry per table row.
R_CHECK=() R_GLOBS=() R_CMD=()
CONTROLS=()   # distinct control names, first appearance order

# glob_match PATH 'glob;glob' — true when any glob matches the whole path.
glob_match() {
    local value=$1 pattern
    local -a list
    IFS=';' read -ra list <<<"$2"
    for pattern in "${list[@]}"; do
        # shellcheck disable=SC2053
        [[ $value == $pattern ]] && return 0
    done
    return 1
}

# row_hit INDEX PATH — does row INDEX match PATH.
row_hit() { glob_match "$2" "${R_GLOBS[$1]}"; }

is_job() { case " ${JOBS} " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }

read_table() {
    local private=0 file line check globs command why context rest name seen
    local -a fields
    [ -e "${ROOT}/export/rules" ] && private=1
    for file in "${ROOT}/ci/checks.tsv" "${ROOT}/ci/checks-private.tsv"; do
        [ -f "${file}" ] || continue
        while IFS= read -r line || [ -n "${line}" ]; do
            case "${line}" in '' | '#'*) continue ;; esac
            case "${line}" in *[![:space:]]*) ;; *) continue ;; esac
            # Split on every tab, keeping empty fields (read would merge adjacent tabs).
            fields=()
            rest=${line}
            while :; do
                case "${rest}" in
                    *$'\t'*) fields+=("${rest%%$'\t'*}"); rest=${rest#*$'\t'} ;;
                    *) fields+=("${rest}"); break ;;
                esac
            done
            check=${fields[0]} globs=${fields[1]:-} command=${fields[2]:-} why=${fields[3]:-} context=${fields[4]:-}
            if [ "${#fields[@]}" -ne 5 ] || [ -z "${globs}" ] || [ -z "${why}" ] \
                || { [ "${context}" != all ] && [ "${context}" != private ]; }; then
                die 'checks.tsv requires check, globs, command, explanation and context'
            fi
            [ "${context}" = private ] && [ "${private}" = 0 ] && continue
            case "${check}" in
                '' | leak-skip | all) ;;
                controls:?*)
                    [ "${command}" != - ] || die "control has no command: ${check}"
                    case "${command}" in *[\'\"\\\$\`\|\&\<\>\;]*) die "control command needs plain words: ${check}" ;; esac
                    ;;
                *) is_job "${check}" || die "unknown check: ${check}" ;;
            esac
            R_CHECK+=("${check}")
            R_GLOBS+=("${globs}")
            R_CMD+=("${command}")
            case "${check}" in
                controls:*)
                    name=${check#controls:}
                    seen=0
                    local c
                    for c in "${CONTROLS[@]+"${CONTROLS[@]}"}"; do [ "${c}" = "${name}" ] && seen=1; done
                    [ "${seen}" = 1 ] || CONTROLS+=("${name}")
                    ;;
            esac
        done <"${file}"
    done
}

# Switch state lives in S_<name> variables: jobs, control_<name> and locks.
reset_switches() {
    local job name
    for job in ${JOBS}; do printf -v "S_${job}" false; done
    for name in "${CONTROLS[@]+"${CONTROLS[@]}"}"; do printf -v "S_control_${name}" false; done
    S_locks=false
}

# set_all [osv]: every job and control; osv only when the argument is 'osv'.
set_all() {
    local job name
    for job in ${JOBS}; do printf -v "S_${job}" true; done
    for name in "${CONTROLS[@]+"${CONTROLS[@]}"}"; do printf -v "S_control_${name}" true; done
    S_locks=true
    if [ "${1:-}" != osv ]; then S_osv=false; S_locks=false; fi
}

set_true() { printf -v "S_$1" true; }

# select STATUS PATH...: the loop below consumes alternating status and path.
select_changes() {
    local status path i hit force=0 osv=0 leak_skip
    reset_switches
    S_guards=true
    if [ "$#" -eq 0 ]; then set_all osv; S_locks=true; return; fi
    while [ "$#" -ge 2 ]; do
        status=$1 path=$2
        shift 2
        hit=0 leak_skip=0
        if glob_match "${path}" "${FORCE_ALL_GLOBS}"; then force=1; fi
        for i in "${!R_CHECK[@]}"; do
            row_hit "${i}" "${path}" || continue
            hit=1
            case "${R_CHECK[$i]}" in
                all) force=1 ;;
                leak-skip) leak_skip=1 ;;
                controls:*)
                    S_controls=true
                    set_true "control_${R_CHECK[$i]#controls:}"
                    ;;
                osv) S_osv=true; osv=1 ;;
                '') ;;
                *) set_true "${R_CHECK[$i]}" ;;
            esac
        done
        if [ "${hit}" = 0 ]; then
            echo "changes: unregistered path ${path}; selecting all jobs" >&2
            force=1
        fi
        # Content edits to excluded, unread files are the only leak exemption.
        if [ "${status}" != M ]; then
            S_controls=true
            set_true control_changes
        fi
        if [ "${status}" != M ] || [ "${leak_skip}" = 0 ]; then S_leak=true; fi
    done
    if [ "${force}" = 1 ]; then
        # The vulnerability database moves with time, so a CI-definition change
        # must not make a pull request fail on an advisory published meanwhile.
        if [ "${osv}" = 1 ]; then set_all osv; else set_all; fi
    fi
    S_locks=${S_osv}
}

# routed [BASE]: choose the switches for the current event.
routed() {
    local scope=${CHANGES_SCOPE:-} base head ancestor name raw status meta path
    local -a changes=()
    read_table_once
    if [ -n "${scope}" ]; then
        case "${scope}" in build-script) scope=build_kit ;; public-leak) scope=leak ;; esac
        reset_switches
        if [ "${scope}" = all ]; then set_all osv; return; fi
        is_job "${scope}" || die "unknown CHANGES_SCOPE: ${scope}"
        S_guards=true
        printf -v "S_${scope}" true
        if [ "${scope}" = controls ]; then
            for name in "${CONTROLS[@]+"${CONTROLS[@]}"}"; do printf -v "S_control_${name}" true; done
        fi
        S_locks=${S_osv}
        return
    fi
    if [ "${GITHUB_EVENT_NAME:-pull_request}" != pull_request ]; then reset_switches; set_all osv; return; fi
    base=${1:-${CHANGES_BASE:-}}
    if [ -z "${base}" ] || [ "${base}" = 0000000000000000000000000000000000000000 ]; then
        reset_switches; set_all osv; return
    fi
    head=${CHANGES_HEAD:-HEAD}
    # A base-branch tip is not a diff base when the branches diverged.
    if ! ancestor=$(git merge-base "${base}" "${head}" 2>/dev/null); then
        unreadable 'git could not find the merge base'; return
    fi
    # Read the NUL-separated diff directly: a path may contain any byte but NUL.
    local diff_file meta path status
    diff_file=$(mktemp "${TMPDIR:-/tmp}/changes-diff.XXXXXX")
    if ! git diff --raw --no-renames -z "${ancestor}" "${head}" >"${diff_file}"; then
        rm -f -- "${diff_file}"; unreadable 'git could not produce the diff'; return
    fi
    while IFS= read -r -d '' meta; do
        if ! IFS= read -r -d '' path; then
            rm -f -- "${diff_file}"; unreadable 'unreadable diff metadata'; return
        fi
        # shellcheck disable=SC2206
        local -a parts=(${meta})
        if [ "${#parts[@]}" -ne 5 ] || [ "${parts[0]:0:1}" != : ]; then
            rm -f -- "${diff_file}"; unreadable 'unreadable diff metadata'; return
        fi
        status=${parts[4]}
        if [ "${status}" = M ]; then
            case "${parts[0]:1}:${parts[1]}" in
                100644:100644 | 100755:100755) ;;
                *) status=T ;;
            esac
        fi
        changes+=("${status}" "${path}")
    done <"${diff_file}"
    rm -f -- "${diff_file}"
    # read leaves an unterminated last record in meta.
    if [ -n "${meta}" ]; then unreadable 'unreadable diff metadata'; return; fi
    select_changes "${changes[@]+"${changes[@]}"}"
}

unreadable() {
    echo "changes: unreadable diff; selecting all jobs ($1)" >&2
    reset_switches
    set_all osv
}

TABLE_READ=0
read_table_once() { [ "${TABLE_READ}" = 1 ] || { read_table; TABLE_READ=1; }; }

repository_paths() {
    local path
    (cd "${ROOT}" && git ls-files --cached --others --exclude-standard -z) | LC_ALL=C sort -zu | while IFS= read -r -d '' path; do
        if [ -e "${ROOT}/${path}" ] || [ -L "${ROOT}/${path}" ]; then printf '%s\0' "${path}"; fi
    done
}

# job_inputs JOB: NUL-separated repository paths whose row names JOB.
job_inputs() {
    local path i
    while IFS= read -r -d '' path; do
        for i in "${!R_CHECK[@]}"; do
            if [ "${R_CHECK[$i]}" = "$1" ] && row_hit "${i}" "${path}"; then printf '%s\0' "${path}"; break; fi
        done
    done < <(repository_paths)
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum | cut -d' ' -f1; else shasum -a 256 | cut -d' ' -f1; fi
}

file_mode() {
    if [ -L "$1" ]; then echo 120000; return; fi
    case "$(ls -ld -- "$1")" in
        ???[xs]* | ??????[xs]* | ?????????[xt]*) echo 100755 ;;
        *) echo 100644 ;;
    esac
}

hash_inputs() {
    local path file mode target
    while IFS= read -r -d '' path; do
        file=${ROOT}/${path}
        mode=$(file_mode "${file}")
        if [ -L "${file}" ]; then
            # Keep the target's exact bytes: -n adds no newline (GNU and BSD), and the
            # sentinel stops command substitution from stripping the target's own.
            target=$(readlink -n "${file}" && printf x) || die "cannot read the link ${path}"
            target=${target%x}
            printf '%s\0%s\0%s\0%s' "${path}" "${mode}" "$(printf %s "${target}" | wc -c | tr -d ' ')" "${target}"
        else
            printf '%s\0%s\0%s\0' "${path}" "${mode}" "$(wc -c <"${file}" | tr -d ' ')"
            cat "${file}"
        fi
    done | sha256
}

validate() {
    local errors=() path i pattern found
    local -a all list=()
    while IFS= read -r -d '' path; do all+=("${path}"); done < <(repository_paths)
    for path in "${all[@]}"; do
        found=0
        for i in "${!R_CHECK[@]}"; do
            if row_hit "${i}" "${path}"; then found=1; break; fi
        done
        [ "${found}" = 1 ] || errors+=("unregistered path: ${path}")
    done
    for i in "${!R_CHECK[@]}"; do
        IFS=';' read -ra list <<<"${R_GLOBS[$i]}"
        for pattern in "${list[@]}"; do
            found=0
            for path in "${all[@]}"; do
                # shellcheck disable=SC2053
                if [[ ${path} == ${pattern} ]]; then found=1; break; fi
            done
            [ "${found}" = 1 ] || errors+=("glob matches no repository path: ${pattern}")
        done
    done
    if [ "${#errors[@]}" -gt 0 ]; then
        printf 'checks: %s\n' "${errors[@]}" >&2
        exit 1
    fi
}

run_controls() {
    local name i command argv sel
    local -a seen=()
    routed
    for name in "${CONTROLS[@]+"${CONTROLS[@]}"}"; do
        command=
        for i in "${!R_CHECK[@]}"; do
            [ "${R_CHECK[$i]}" = "controls:${name}" ] || continue
            if [ -n "${command}" ] && [ "${command}" != "${R_CMD[$i]}" ]; then die "conflicting control command: ${name}"; fi
            command=${R_CMD[$i]}
        done
        sel=S_control_${name}
        [ "${!sel}" = true ] || continue
        echo "controls: ${name}"
        read -ra argv <<<"${command}"
        local rc=0
        (cd "${ROOT}" && "${argv[@]}") || rc=$?
        [ "${rc}" = 0 ] || die "control ${name} failed with exit status ${rc}"
    done
}

emit_switches() {
    local out="" job name var
    for job in ${JOBS}; do var=S_${job}; out+="${job}=${!var}"$'\n'; done
    for name in "${CONTROLS[@]+"${CONTROLS[@]}"}"; do var=S_control_${name}; out+="control_${name}=${!var}"$'\n'; done
    out+="locks=${S_locks}"$'\n'"scope=${CHANGES_SCOPE:-}"$'\n'
    if [ -n "${GITHUB_OUTPUT:-}" ]; then printf %s "${out}" >>"${GITHUB_OUTPUT}"; else printf %s "${out}"; fi
}

main() {
    case "${1:-}" in
        --classify)
            [ "$#" -eq 2 ] || [ "$#" -eq 3 ] || die 'usage: changes.sh --classify PATH [STATUS]'
            read_table_once
            select_changes "${3:-M}" "$2"
            local job var out=()
            for job in ${JOBS}; do var=S_${job}; [ "${!var}" = true ] && out+=("${job}"); done
            echo "${out[*]}"
            ;;
        --hash)
            [ "$#" -eq 2 ] && is_job "$2" || die 'usage: changes.sh --hash JOB'
            read_table_once
            TMP=$(mktemp)
            trap 'rm -f "${TMP}"' EXIT
            job_inputs "$2" >"${TMP}"
            [ -s "${TMP}" ] || die "no inputs for $2"
            hash_inputs <"${TMP}"
            ;;
        --validate)
            [ "$#" -eq 1 ] || die 'usage: changes.sh --validate'
            read_table_once
            validate
            ;;
        --controls)
            [ "$#" -eq 1 ] || die 'usage: changes.sh --controls'
            run_controls
            ;;
        *)
            if [ "$#" -gt 1 ] || case "${1:-}" in --*) true ;; *) false ;; esac; then die 'usage: changes.sh [BASE]'; fi
            routed "$@"
            emit_switches
            ;;
    esac
}

main "$@"
