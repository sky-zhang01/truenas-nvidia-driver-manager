#!/usr/bin/env bash
# Controls use existing Git objects and a deterministic tree stub; no commits are created.
set -Eeuo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/test-lib.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
EXPORT="${SCRIPT_DIR}/export-public.sh"
REPO="$(git -C "${SCRIPT_DIR}" rev-parse --show-toplevel)"
SOURCE="$(git -C "${REPO}" rev-parse HEAD)"
mkdir "${WORK}/bin" "${WORK}/fixture"
cat >"${WORK}/bin/git" <<'STUB'
#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
const entries = JSON.parse(fs.readFileSync(process.env.EXPORT_FIXTURE, 'utf8'));
if (args[0] === 'rev-parse') {
  console.log(args.includes('--show-toplevel') ? process.env.EXPORT_ROOT : 'a'.repeat(40));
} else if (args[0] === 'ls-tree') {
  process.stdout.write(entries.map((e, i) => `${e.mode || '100644'} ${e.mode === '160000' ? 'commit' : 'blob'} ${String(i).padStart(40, '0')}\t${e.path}\0`).join(''));
} else if (args[0] === 'ls-files') {
  process.stdout.write(entries.map(e => `${e.path}\0`).join(''));
} else if (args[0] === 'cat-file') {
  process.stdout.write(entries[Number(args[2])].content || '');
} else { process.exit(1); }
STUB
chmod +x "${WORK}/bin/git"
export EXPORT_FIXTURE="${WORK}/fixture.json" EXPORT_ROOT="${WORK}/fixture"
reset_case() {
    rm -rf "${WORK}/out" "${WORK}/manifest.json"
    printf '%s\n' '[{"path":"safe.txt","content":"safe\n"},{"path":"private.txt","content":"excluded\n"}]' >"${EXPORT_FIXTURE}"
    printf 'include safe.txt\nexclude private.txt\n' >"${WORK}/rules"
    printf 'safe\n' >"${WORK}/fixture/safe.txt"
    printf 'excluded\n' >"${WORK}/fixture/private.txt"
}
run_export() {
    (cd "${WORK}/fixture" && PATH="${WORK}/bin:${PATH}" bash "${EXPORT}" --source HEAD --output "${WORK}/out" --manifest "${EXPORT_MANIFEST:-${WORK}/manifest.json}" --rules "${WORK}/rules" "$@") >"${WORK}/stdout" 2>"${WORK}/stderr"
}
reject() {
    local description="$1"; shift
    if run_export "$@"; then fail "${description}"; else pass "${description}"; fi
}

reset_case
check 'clean export succeeds' run_export
check 'excluded files are absent' test ! -e "${WORK}/out/private.txt"
check 'manifest is external and hashes exported bytes' node -e 'const fs=require("fs"),c=require("crypto"),p=process.argv[1];const m=JSON.parse(fs.readFileSync(p+"/manifest.json"));if(m.files.length!==1||m.files[0].sha256!==c.createHash("sha256").update(fs.readFileSync(p+"/out/safe.txt")).digest("hex"))process.exit(1)' "${WORK}"
reset_case
printf 'include safe.txt\n' >"${WORK}/rules"
reject 'unclassified paths fail'
reset_case
printf 'include absent.txt\n' >>"${WORK}/rules"
reject 'rules matching no paths fail'
check 'explicit local exception is recorded' run_export --worktree --allow-unmatched-rule absent.txt
check 'receipt identifies worktree exception' node -e 'const m=require(process.argv[1]);if(m.mode!=="worktree"||m.unmatchedRuleExceptions.join()!=="absent.txt")process.exit(1)' "${WORK}/manifest.json"
reset_case
rm "${WORK}/fixture/safe.txt"
printf 'include private.txt\n' >"${WORK}/rules"
check 'worktree deletes cached paths from rehearsal inputs' run_export --worktree
check 'deleted cached files are absent from the receipt' node -e 'const m=require(process.argv[1]);if(m.files.length!==1||m.files[0].path!=="private.txt")process.exit(1)' "${WORK}/manifest.json"
reset_case
printf '%s\n' '[{"path":"synthetic-private-marker.txt","content":"safe"}]' >"${EXPORT_FIXTURE}"
reject 'unclassified private filenames fail without echoing input'
check 'unclassified filename is absent from output' node -e 'const fs=require("fs"),p=process.argv[1];if((fs.readFileSync(p+"/stdout","utf8")+fs.readFileSync(p+"/stderr","utf8")).includes("synthetic-private-marker"))process.exit(1)' "${WORK}"
reset_case
printf 'include synthetic-private-marker.txt\n' >>"${WORK}/rules"
reject 'unmatched private rules fail without echoing input'
check 'unmatched rule is absent from output' node -e 'const fs=require("fs"),p=process.argv[1];if((fs.readFileSync(p+"/stdout","utf8")+fs.readFileSync(p+"/stderr","utf8")).includes("synthetic-private-marker"))process.exit(1)' "${WORK}"
check 'private rule exceptions can be recorded externally' run_export --worktree --allow-unmatched-rule synthetic-private-marker.txt
check 'accepted rule exception is absent from output' node -e 'const fs=require("fs"),p=process.argv[1];if((fs.readFileSync(p+"/stdout","utf8")+fs.readFileSync(p+"/stderr","utf8")).includes("synthetic-private-marker"))process.exit(1)' "${WORK}"
reset_case
printf 'include *.txt\n' >"${WORK}/rules"
reject 'glob rules fail'
reset_case
printf '%s\n' '[{"path":"safe.txt","mode":"160000"}]' >"${EXPORT_FIXTURE}"
printf 'exclude safe.txt\n' >"${WORK}/rules"
reject 'excluded gitlinks also fail'
reset_case
printf '%s\n' '[{"path":"safe.txt","mode":"120000","content":"../outside"}]' >"${EXPORT_FIXTURE}"
printf 'include safe.txt\n' >"${WORK}/rules"
reject 'links outside the export fail'
reset_case
printf '%s\n' '[{"path":"safe.txt","mode":"120000","content":"private.txt"},{"path":"private.txt","content":"hidden"}]' >"${EXPORT_FIXTURE}"
reject 'links to excluded files fail'
reset_case
printf '%s\n' '[{"path":"safe.txt","mode":"120000","content":"target.txt"},{"path":"target.txt","mode":"120000","content":"safe.txt"}]' >"${EXPORT_FIXTURE}"
printf 'include safe.txt\ninclude target.txt\n' >"${WORK}/rules"
reject 'symbolic link cycles fail'
reset_case
printf '%s\n' '[{"path":"safe.txt","content":"safe"},{"path":"private.txt","mode":"120000","content":"/outside"}]' >"${EXPORT_FIXTURE}"
reject 'excluded links outside the source also fail'
reset_case
printf '%s\n' '[{"path":"safe.txt","mode":"120000","content":"target.txt"},{"path":"target.txt","content":"safe"}]' >"${EXPORT_FIXTURE}"
printf 'include safe.txt\ninclude target.txt\n' >"${WORK}/rules"
check 'links within the export succeed' run_export
check 'export preserves symbolic links' test -L "${WORK}/out/safe.txt"
reset_case
printf '%s\n' '[{"path":"safe.txt","mode":"100755","content":"safe"},{"path":"private.txt","content":"private"}]' >"${EXPORT_FIXTURE}"
check 'executable export succeeds' run_export
check 'executable mode is preserved' test -x "${WORK}/out/safe.txt"
reset_case
mkdir "${WORK}/out"
printf 'preserve\n' >"${WORK}/out/preserve"
reject 'nonempty destinations fail'
check 'existing files are preserved' test -f "${WORK}/out/preserve"
reset_case
EXPORT_MANIFEST="${WORK}/out/manifest.json" reject 'manifest inside the export fails'
reset_case
reject 'invalid commits fail' --source --invalid
reset_case
reject 'rule exceptions require explicit worktree mode' --allow-unmatched-rule absent.txt
reset_case
reject 'matched rules cannot be waived' --worktree --allow-unmatched-rule safe.txt
reset_case
printf 'include ../safe.txt\n' >"${WORK}/rules"
reject 'parent traversal in rules fails'
reset_case
printf '%s\n' '[{"path":"../safe.txt","content":"unsafe"}]' >"${EXPORT_FIXTURE}"
reject 'parent traversal in source paths fails'
reset_case
printf 'include safe.txt\ninclude private.txt\nexclude private.txt\n' >"${WORK}/rules"
check 'exclude wins over overlapping include' run_export
check 'overlap does not publish excluded bytes' test ! -e "${WORK}/out/private.txt"
reset_case
mkdir "${WORK}/out"
check 'existing empty output directories are accepted' run_export
reset_case
printf 'preserve\n' >"${WORK}/manifest.json"
reject 'existing manifests cannot be overwritten'
check 'existing manifest bytes are preserved' test "$(cat "${WORK}/manifest.json")" = preserve

# Verify the normal path against a real local commit, regardless of pending changes.
git -C "${REPO}" ls-tree -r --name-only "${SOURCE}" | sed 's/^/include /' >"${WORK}/actual.rules"
if (cd "${REPO}" && bash "${EXPORT}" --source "${SOURCE}" --output "${WORK}/actual" --manifest "${WORK}/actual.json" --rules "${WORK}/actual.rules") >"${WORK}/actual.stdout" 2>"${WORK}/actual.stderr"; then
    pass 'a real existing commit exports without creating a commit'
    check 'source bytes match the specified commit' cmp "${WORK}/actual/README.md" <(git -C "${REPO}" show "${SOURCE}:README.md")
else
    cat "${WORK}/actual.stderr" >&2
    fail 'a real existing commit exports without creating a commit'
fi
if (cd "${REPO}" && bash "${EXPORT}" --source invalid-export-commit --output "${WORK}/invalid" --manifest "${WORK}/invalid.json" --rules "${WORK}/actual.rules") >"${WORK}/invalid.stdout" 2>"${WORK}/invalid.stderr"; then
    fail 'an unknown real Git commit fails'
else
    pass 'an unknown real Git commit fails'
fi
# This uses the commit's own policy, including its required public inputs.
export_head_policy() {
    (cd "${REPO}" && bash "${EXPORT}" --source HEAD --output "${WORK}/head-tree" --manifest "${WORK}/head-manifest.json")
}
COMMITTED_RULES="$(git -C "${REPO}" ls-tree --name-only "${SOURCE}" -- export/rules)"
if [ -n "${COMMITTED_RULES}" ] || [ -f "${REPO}/.gitea/workflows/ci.yml" ]; then
    check 'HEAD exports under its committed rules' export_head_policy
else
    echo 'committed export policy control: private export/rules is not present in the exported tree'
fi
finish_tests "export-public-test: ${FAILURES} controls failed" "export-public-test: ${PASS} controls passed"
