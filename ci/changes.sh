#!/usr/bin/env bash
# changes.sh — turn the changed-file list into CI switches.
#
# Output keys (appended to $GITHUB_OUTPUT when set, printed to stdout
# otherwise): rust, e2e, image, build_kit — which heavy jobs must run — and
# locks — a lockfile or manifest changed.
#
# A pull request is diffed against its base revision (argument or
# CHANGES_BASE). Everything else — pushes to main, tags, schedule,
# workflow_dispatch — is a full run. A missing base, a failed diff, an empty
# diff and any unregistered path are full runs too: the safe direction is
# more checks, never fewer.
set -Eeuo pipefail

classify_path() { # classify_path <path> — echo the switch set; empty = static only
    case "$1" in
        # The pipeline itself can affect every job.
        .gitea/*|.github/*|ci/*) echo "rust e2e image build_kit" ;;
        # The package closure is compiled into the binary and drives the
        # build container.
        src/build_kit/*) echo "rust e2e image build_kit" ;;
        # Sources are compiled into the binary and copied into the image.
        src/*) echo "rust e2e image" ;;
        tests/*|examples/*) echo "rust e2e" ;;
        tools/build-kit.sh) echo "rust e2e build_kit" ;;
        # License generation runs during the image build; tests always run in static.
        tools/third-party-licenses*.py) echo "image" ;;
        # gen-gpu-table is a cargo example and runs under cargo test.
        tools/*) echo "rust e2e" ;;
        # Both scripts are include_str!'d into the binary and run inside the
        # build container.
        docker/*) echo "rust e2e image build_kit" ;;
        Cargo.toml|Cargo.lock) echo "rust e2e image locks" ;;
        # Unit tests run in static; browser tests do not enter runtime assets.
        web/test/*) echo "" ;;
        web/e2e/*|web/playwright.config.ts) echo "e2e" ;;
        # Remaining web inputs affect browser behavior and image assets.
        web/package.json|web/package-lock.json) echo "e2e image locks" ;;
        web/*) echo "e2e image" ;;
        # Non-code inputs the image build consumes.
        Dockerfile|.dockerignore|LICENSE|THIRD_PARTY_NOTICES.md|deploy/*) echo "image" ;;
        # Documentation and repository metadata only.
        README*|CHANGELOG.md|CHANGELOG.zh-CN.md|CONTRIBUTING.md|CONTRIBUTING.zh-CN.md|SECURITY.md|SECURITY.zh-CN.md|CLAUDE.md|docs/*|skills/*|renovate.json|.gitignore|.gitleaks.toml|export/rules) echo "" ;;
        *) echo "unknown" ;;
    esac
}

usage() {
    echo "usage: changes.sh [--classify <path> | <base-revision>]" >&2
    exit 2
}

emit() { # emit <key> <value>
    if [ -n "${GITHUB_OUTPUT:-}" ]; then
        printf '%s=%s\n' "$1" "$2" >>"${GITHUB_OUTPUT}"
    else
        printf '%s=%s\n' "$1" "$2"
    fi
}

full_run() { # full_run <reason>
    echo "changes: full run ($1)" >&2
    for key in rust e2e image build_kit locks; do
        emit "${key}" true
    done
    exit 0
}

if [ "${1:-}" = "--classify" ]; then
    [ $# -eq 2 ] || usage
    classify_path "$2"
    exit 0
fi
[ $# -le 1 ] || usage

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || { echo "changes: not inside a Git repository" >&2; exit 1; }
cd "${repo_root}"

if [ -n "${GITHUB_EVENT_NAME:-}" ] && [ "${GITHUB_EVENT_NAME}" != "pull_request" ]; then
    full_run "event ${GITHUB_EVENT_NAME}"
fi

base="${1:-${CHANGES_BASE:-}}"
case "${base}" in
    ""|0000000000000000000000000000000000000000) full_run "no base revision" ;;
esac

# --no-renames counts both sides of a rename; the old path keeps its switches.
paths="$(git diff --name-only --no-renames "${base}" HEAD 2>/dev/null)" \
    || full_run "git diff against ${base} failed"
[ -n "${paths}" ] || full_run "the diff against ${base} is empty"

rust=false
e2e=false
image=false
build_kit=false
locks=false
while IFS= read -r path; do
    switches="$(classify_path "${path}")"
    if [ "${switches}" = "unknown" ]; then
        full_run "unregistered path ${path}"
    fi
    for name in ${switches}; do
        case "${name}" in
            rust) rust=true ;;
            e2e) e2e=true ;;
            image) image=true ;;
            build_kit) build_kit=true ;;
            locks) locks=true ;;
        esac
    done
done <<EOF
${paths}
EOF

for key in rust e2e image build_kit locks; do
    emit "${key}" "${!key}"
done
echo "changes: base ${base}: rust=${rust} e2e=${e2e} image=${image} build_kit=${build_kit} locks=${locks}" >&2
