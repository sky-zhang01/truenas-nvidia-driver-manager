#!/usr/bin/env bash
# Exercise the shared routing table without changing Git refs or contacting remotes.
set -Eeuo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 - "${ROOT}" <<'PY'
import importlib.util
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1])
spec = importlib.util.spec_from_file_location('checks', root / 'ci/checks.py')
checks = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checks)
rows = checks.read_table(root / 'ci/checks.tsv', private=True)
passed = 0
probes = 0

def probe(condition, detail=None):
    global probes
    assert condition, detail
    probes += 1

def case(path, expected, status='M'):
    global passed
    got = checks.select(rows, [(status, path)])
    actual = {k for k, v in got.items() if v and not k.startswith('control_') and k not in ('guards', 'locks')}
    assert actual == set(expected.split()), (path, status, expected, sorted(actual))
    print('PASS: route', status, path)
    passed += 1

cases = [
 ('docs/operations.en.md', 'leak'),
 ('README.md', 'compose leak'), ('README.zh-CN.md', 'compose leak'),
 ('docs/agent.en.md', 'web leak'), ('web/test/render.test.ts', 'web leak'),
 ('web/e2e/auth.spec.ts', 'e2e leak'), ('web/playwright.config.ts', 'e2e leak'),
 ('web/src/main.ts', 'web e2e image leak'), ('web/brand/mark.svg', 'web e2e image leak'),
 ('web/index.html', 'web e2e image leak'), ('web/tsconfig.json', 'controls web e2e image leak'),
 ('web/package.json', 'web e2e image leak'), ('web/package-lock.json', 'web e2e image osv leak'),
 ('web/.gitignore', 'leak'), ('Cargo.toml', 'rust e2e image leak'),
 ('Cargo.lock', 'rust e2e image osv leak'), ('src/auth.rs', 'rust web e2e image leak'),
 ('src/build_kit/packages.tsv', 'rust web e2e image build_kit leak'),
 ('tests/auth.rs', 'rust leak'), ('tests/common/mod.rs', 'rust e2e leak'),
 ('examples/fake_host_server.rs', 'rust e2e leak'), ('tools/gen-gpu-table.rs', 'rust leak'),
 ('docker/build-driver.sh', 'rust image build_kit leak'),
 ('third_party/gpu-unlock/kernel/gpu_unlock/main.c', 'rust image leak'),
 ('tools/build-kit.sh', 'controls build_kit leak'),
 ('tools/third-party-licenses.py', 'controls image leak'),
 ('tools/third-party-licenses-test.py', 'controls leak'),
 ('Dockerfile', 'controls image leak'), ('.dockerignore', 'image leak'),
 ('LICENSE', 'image leak'), ('THIRD_PARTY_NOTICES.md', 'image leak'),
 ('deploy/compose.yaml', 'compose image leak'), ('osv-scanner.toml', 'osv leak'),
 ('ci/osv-scan.sh', 'controls osv leak'), ('ci/min-rust.sh', 'rust leak'),
 ('ci/e2e-state.sh', 'e2e leak'), ('ci/container/test.sh', 'image leak'),
 ('ci/build-script/test.sh', 'build_kit leak'),
 ('ci/image-assert.py', 'controls image leak'),
 ('ci/job/web.sh', 'web leak'), ('ci/job/rust.sh', 'rust leak'),
 ('ci/job/controls.sh', 'controls leak'),
 ('ci/job/gpu-unlock.sh', 'leak'), ('ci/gpu-unlock-upstream.sh', 'leak'),
 ('third_party/gpu-unlock/UPSTREAM.tsv', 'rust image leak'),
]
# Paths that exist only in the internal repository route through the private
# table; the exported tree has neither the paths nor the table.
private_file = root / 'ci/checks-private.tsv'
if private_file.exists():
    cases += [
     ('renovate.json', ''), ('docs/release.en.md', ''), ('tools/maintenance/local-gc.py', ''),
     ('tools/release/release.py', 'controls'), ('ci/release.sh', 'controls'),
     ('ci/nas-e2e/run.py', 'controls'), ('ci/runner/Dockerfile', ''),
     ('export/rules', 'controls leak'), ('ci/post-merge.sh', 'controls leak'),
    ]
for path, want in cases:
    case(path, want)
all_jobs = set(checks.JOBS) - {'guards'}
for path in ('ci/checks.tsv', 'ci/changes.sh', 'ci/gate.sh', 'ci/checks.py',
             '.github/workflows/ci.yml', 'unregistered/file', 'ci/job/x.sh'):
    case(path, ' '.join(sorted(all_jobs)))
if private_file.exists():
    case('.gitea/workflows/checks.yml', ' '.join(sorted(all_jobs)))
    # Adding, deleting, renaming or retyping an exempt path still selects leak.
    for status in ('A', 'D', 'R', 'T'):
        case('tools/maintenance/local-gc.py', 'controls leak', status)
    probe(checks.select(rows, [('M', 'export/rules')])['control_release'])
# A second path unions its inputs, and any path-set change enables leak.
selected = checks.select(rows, [('M', 'ci/gate-test.sh'), ('M', 'web/test/render.test.ts')])
probe(selected['controls'] and selected['web'] and selected['leak'])
# Even a head table with its rust row removed cannot judge its own table edit narrowly.
weakened = [row for row in rows if row[0] != 'rust']
probe(all((checks.select(weakened, [('M', 'ci/checks.tsv')])[key] for key in checks.JOBS)))
probe(checks.select(rows, [('M', 'Dockerfile')])['control_changes'])
probe(checks.select(rows, [('M', 'web/tsconfig.json')])['control_changes'])
# The post-merge control is registered only in the private table.
if 'control_post_merge' in checks.full(rows):
    probe(checks.select(rows, [('M', 'ci/release-readback.js')])['control_post_merge'])
    probe(checks.select(rows, [('M', 'ci/test-lib.sh')])['control_post_merge'])
probe(checks.select(rows, [('M', 'ci/test-lib.sh')])['control_github'])
checks.validate(checks.read_table(root / 'ci/checks.tsv'), checks.repository_paths(root))
probes += 1
print('PASS: every repository path registered and every glob matches')
# The exported tree has no private table: it validates alone, without the
# private-only paths and private-context rows.
if private_file.exists():
    private_rows = checks.read_table(root / 'ci/checks.tsv', private=True, extra=private_file.read_text())
    only = [row for row in private_rows if row not in checks.read_table(root / 'ci/checks.tsv', private=True, extra='')]
    probe(only and all(row[0] in ('', 'leak-skip', 'all', 'leak') or row[0].startswith('controls:') for row in only))
    public_only_rows = checks.read_table(root / 'ci/checks.tsv', private=False, extra='')
    private_paths = {path for path in checks.repository_paths(root)
                     if any(checks.matches(path, row[1]) for row in only)
                     and not any(checks.matches(path, row[1]) for row in public_only_rows)}
    probe(private_paths)
    public_rows = checks.read_table(root / 'ci/checks.tsv', private=False, extra='')
    probe(private_paths and not any(checks.matches(path, row[1]) for path in private_paths for row in public_rows))
    checks.validate(public_rows, [p for p in checks.repository_paths(root) if p not in private_paths])
    for row in only:
        if row[0] not in ('leak-skip', 'all'):
            continue
        for path in checks.repository_paths(root):
            if not checks.matches(path, row[1]):
                continue
            got = checks.select(rows, [('M', path)])
            if row[0] == 'leak-skip':
                probe(not got['leak'], ('leak-skip edit selected leak', path))
            else:
                probe(all(got[key] for key in all_jobs), ('private all row did not force every job', path))
    print('PASS: private table merges and the public table validates without it')
# Validate the boundaries that justify web test-only routing.
import json, shlex
probe(json.loads((root / 'web/tsconfig.json').read_text())['include'] == ['src/**/*.ts'])
parts = [shlex.split(line, comments=True) for line in (root/'Dockerfile').read_text().replace('\\\n',' ').splitlines()]
parts = [p for p in parts if p]
last = max(i for i,p in enumerate(parts) if p[0].upper() == 'FROM')
web = [p for p in parts[last+1:] if p[0].upper() == 'COPY' and any('web' in a for a in p[1:])]
probe(web == [['COPY', '--from=web', '/build/web/dist/', '/usr/local/share/truenas-nvidia-driver-manager/web/']], web)
# A fake git supplies real command-boundary failure cases; it writes no index or refs.
# Its diff names docs/operations.en.md: a path the public export keeps, so this test
# behaves the same in the source repository and in the exported public tree.
with tempfile.TemporaryDirectory() as work:
    work = pathlib.Path(work)
    fake = work/'git'
    fake.write_text('#!/bin/sh\ncase "$1" in\nshow) case "$2" in *private*) exit 128;; *) cat "$TEST_ROOT/ci/checks.tsv";; esac;;\nrev-parse) printf "%s\\n" "$TEST_ROOT";;\nmerge-base) [ "$TEST_MODE" != bad-base ] || exit 1; printf "base\\n";;\ndiff) [ "$TEST_MODE" != bad-diff ] || exit 1; [ "$TEST_MODE" != empty ] || exit 0; printf ":100644 100644 a b M\\000docs/operations.en.md\\000";;\n*) exit 2;;\nesac\n')
    fake.chmod(0o755)
    import os
    env = dict(os.environ, PATH=str(work)+os.pathsep+os.environ['PATH'], TEST_ROOT=str(root), GITHUB_EVENT_NAME='pull_request')
    for key in ('GITHUB_OUTPUT','CHANGES_SCOPE','CHANGES_FORCE','CHANGES_BASE','CHANGES_HEAD'):
        env.pop(key, None)
    for mode in ('normal','bad-base','bad-diff','empty'):
        env['TEST_MODE'] = mode
        out = subprocess.check_output(['bash', str(root/'ci/changes.sh'), 'base'], env=env, text=True)
        got = dict(line.split('=',1) for line in out.splitlines())
        probe(got['rust'] == ('false' if mode == 'normal' else 'true'), (mode, got))
    # Excluded symlink target edits still affect export safety checks.
    normal_script=fake.read_text()
    fake.write_text(normal_script.replace(':100644 100644 a b M', ':120000 120000 a b M'))
    env['TEST_MODE']='normal'
    out=subprocess.check_output(['bash',str(root/'ci/changes.sh'),'base'],env=env,text=True)
    symlink=dict(line.split('=',1) for line in out.splitlines())
    probe(symlink['leak'] == 'true' and symlink['control_changes'] == 'true')
    fake.write_text(normal_script)
    for scope in ('all','osv','web','build-script'):
        env['CHANGES_SCOPE'] = scope
        out = subprocess.check_output(['bash', str(root/'ci/changes.sh')],env=env,text=True)
        got = dict(line.split('=',1) for line in out.splitlines())
        probe(got['guards'] == 'true')
        probe(got['build_kit' if scope == 'build-script' else 'rust' if scope == 'all' else scope] == 'true')
    env['CHANGES_SCOPE']='web'
    env['GITHUB_OUTPUT']=str(work/'outputs')
    probe(subprocess.check_output(['bash', str(root / 'ci/changes.sh')], env=env, text=True) == '')
    probe('web=true\n' in (work / 'outputs').read_text())
# Content identities include only the selected inputs, their names and modes.
with tempfile.TemporaryDirectory() as directory:
    fixture = pathlib.Path(directory)
    (fixture/'ci').mkdir()
    (fixture/'ci/checks.py').write_bytes((root/'ci/checks.py').read_bytes())
    (fixture/'ci/changes.sh').write_bytes((root/'ci/changes.sh').read_bytes())
    (fixture/'ci/checks.tsv').write_text('image\truntime*;helper\t-\tFixture image inputs.\tall\n')
    (fixture/'runtime').write_text('runtime bytes')
    (fixture/'helper').write_text('test helper')
    (fixture/'unrelated').write_text('documentation')
    (fixture/'paths').write_bytes(b'runtime\0helper\0unrelated\0')
    (fixture/'git').write_text('#!/bin/sh\n[ "$1" = ls-files ] || exit 2\ncat "$HASH_FIXTURE/paths"\n')
    (fixture/'git').chmod(0o755)
    env = dict(os.environ, PATH=str(fixture)+os.pathsep+os.environ['PATH'], HASH_FIXTURE=str(fixture))
    def identity():
        return subprocess.check_output(['bash', str(fixture/'ci/changes.sh'), '--hash', 'image'],env=env,text=True).strip()
    original=identity()
    probe(len(original) == 64)
    (fixture/'unrelated').write_text('changed documentation')
    probe(identity() == original)
    (fixture/'helper').write_text('changed helper')
    changed=identity()
    probe(changed != original)
    (fixture/'helper').chmod(0o755)
    mode_hash=identity()
    probe(mode_hash != changed)
    (fixture/'runtime').rename(fixture/'runtime-renamed')
    (fixture/'paths').write_bytes(b'runtime-renamed\0helper\0unrelated\0')
    renamed=identity()
    probe(renamed != mode_hash)
    (fixture/'ci/checks.tsv').write_text('image\thelper\t-\tNarrowed fixture image inputs.\tall\n')
    probe(identity() != renamed)
    selected=subprocess.check_output(['bash', str(fixture/'ci/changes.sh'), '--inputs', 'image'],env=env)
    probe(selected == b'helper\x00')
    print('PASS: image hash covers content, modes, names and selected membership only')
# A head table may remove its own all row; the merge-base table still forces all.
with tempfile.TemporaryDirectory() as directory:
    fixture=pathlib.Path(directory)
    (fixture/'ci').mkdir()
    for name in ('checks.py','changes.sh'):
        (fixture/'ci'/name).write_bytes((root/'ci'/name).read_bytes())
    (fixture/'ci/checks.tsv').write_text(''.join(line for line in (root/'ci/checks.tsv').read_text().splitlines(keepends=True) if not line.startswith('all\t')))
    (fixture/'git').write_text('#!/bin/sh\ncase "$1" in\nmerge-base) printf "base\\n";;\nshow) case "$2" in *private*) exit 128;; *) cat "$TEST_ROOT/ci/checks.tsv";; esac;;\ndiff) printf ":100644 100644 a b M\\000ci/checks.tsv\\000";;\n*) exit 2;;\nesac\n')
    (fixture/'git').chmod(0o755)
    env=dict(os.environ,PATH=str(fixture)+os.pathsep+os.environ['PATH'],TEST_ROOT=str(root),GITHUB_EVENT_NAME='pull_request')
    for key in ('GITHUB_OUTPUT','CHANGES_SCOPE','CHANGES_BASE','CHANGES_HEAD'):
        env.pop(key,None)
    out=subprocess.check_output(['bash',str(fixture/'ci/changes.sh'),'base'],env=env,text=True)
    selected=dict(line.split('=',1) for line in out.splitlines())
    probe(all((selected[key] == 'true' for key in checks.JOBS)))
# Exercise the actual controls consumer with routed subprocesses, not switches alone.
with tempfile.TemporaryDirectory() as directory:
    fixture=pathlib.Path(directory)
    (fixture/'ci/job').mkdir(parents=True)
    for name in ('checks.py','changes.sh'):
        (fixture/'ci'/name).write_bytes((root/'ci'/name).read_bytes())
    (fixture/'ci/job/controls.sh').write_bytes((root/'ci/job/controls.sh').read_bytes())
    (fixture/'ci/checks.tsv').write_text(
        'controls:alpha\talpha\tbash ci/record.sh alpha\tSelected public control.\tall\n'
        'controls:beta\tbeta\tbash ci/record.sh beta\tUnselected public control.\tall\n'
        'controls:private\talpha\tbash ci/record.sh private\tPrivate-only control.\tprivate\n')
    (fixture/'ci/record.sh').write_text('#!/bin/sh\nprintf "%s\\n" "$1" >>"$CONTROL_LOG"\n[ "$1" != alpha ] || exit "${CONTROL_FAIL:-0}"\n')
    (fixture/'git').write_text('#!/bin/sh\ncase "$1" in\nmerge-base) printf "base\\n";;\nshow) cat "$CONTROL_ROOT/ci/checks.tsv";;\ndiff) printf ":100644 100644 a b M\\000alpha\\000";;\n*) exit 2;;\nesac\n')
    (fixture/'git').chmod(0o755)
    logfile=fixture/'executed'
    env=dict(os.environ,PATH=str(fixture)+os.pathsep+os.environ['PATH'],CONTROL_ROOT=str(fixture),CONTROL_LOG=str(logfile),GITHUB_EVENT_NAME='pull_request',CHANGES_BASE='base')
    for key in ('CHANGES_SCOPE','CHANGES_HEAD','CONTROL_FAIL'):
        env.pop(key,None)
    def controls():
        return subprocess.run(['bash',str(fixture/'ci/job/controls.sh')],env=env,capture_output=True,text=True)
    result=controls()
    probe(result.returncode == 0 and logfile.read_text().splitlines() == ['alpha'], (result, logfile.read_text()))
    print('PASS: actual controls entry executes only the selected public command')
    (fixture/'export').mkdir()
    (fixture/'export/rules').write_text('private context marker\n')
    logfile.unlink()
    result=controls()
    probe(result.returncode == 0 and logfile.read_text().splitlines() == ['alpha', 'private'], (result, logfile.read_text()))
    print('PASS: private commands execute only when the private context is present')
    logfile.unlink()
    env['CONTROL_FAIL']='17'
    result=controls()
    probe(result.returncode != 0 and logfile.read_text().splitlines() == ['alpha'] and ('17' in result.stderr), (result, logfile.read_text()))
    print('PASS: control failure propagates and stops later selected commands')
# A table-only edit must execute baseline controls even after deletion or rewriting.
with tempfile.TemporaryDirectory() as directory:
    fixture=pathlib.Path(directory)
    (fixture/'ci').mkdir()
    for name in ('checks.py','changes.sh'):
        (fixture/'ci'/name).write_bytes((root/'ci'/name).read_bytes())
    baseline=(
        'all\tci/checks.tsv\t-\tTable edits force all controls.\tall\n'
        'controls:gate\tci/gate.sh\tbash ci/gate-test.sh\tBaseline gate control.\tall\n'
        'controls:kept\tkept\tbash ci/record.sh kept\tUnchanged control.\tall\n'
        'controls:private\tprivate\tbash ci/record.sh private\tPrivate baseline control.\tprivate\n')
    (fixture/'baseline.tsv').write_text(baseline)
    (fixture/'ci/gate-test.sh').write_text('#!/bin/sh\nprintf "baseline-gate\\n" >>"$CONTROL_LOG"\nexit "${CONTROL_FAIL:-0}"\n')
    (fixture/'ci/record.sh').write_text('#!/bin/sh\nprintf "%s\\n" "$1" >>"$CONTROL_LOG"\n')
    (fixture/'git').write_text('#!/bin/sh\ncase "$1" in\nmerge-base) printf "base\\n";;\nshow) cat "$CONTROL_ROOT/baseline.tsv";;\ndiff) printf ":100644 100644 a b M\\000ci/checks.tsv\\000";;\n*) exit 2;;\nesac\n')
    (fixture/'git').chmod(0o755)
    logfile=fixture/'executed'
    env=dict(os.environ,PATH=str(fixture)+os.pathsep+os.environ['PATH'],CONTROL_ROOT=str(fixture),CONTROL_LOG=str(logfile),GITHUB_EVENT_NAME='pull_request',CHANGES_BASE='base')
    for key in ('GITHUB_OUTPUT','CHANGES_SCOPE','CHANGES_HEAD','CONTROL_FAIL'):
        env.pop(key,None)
    failures=[]
    for mode in ('deleted','true','changed'):
        lines=[line for line in baseline.splitlines(keepends=True) if not line.startswith('controls:private\t')]
        if mode == 'deleted':
            lines=[line for line in lines if not line.startswith('controls:gate\t')]
        else:
            command='true' if mode == 'true' else 'bash ci/record.sh head-gate'
            lines=[line.replace('bash ci/gate-test.sh',command) for line in lines]
        lines.append('controls:new\tnew\tbash ci/record.sh new\tNew head control.\tall\n')
        (fixture/'ci/checks.tsv').write_text(''.join(lines))
        for private in (False,True):
            marker=fixture/'export/rules'
            if private:
                marker.parent.mkdir(exist_ok=True)
                marker.write_text('private context marker\n')
            elif marker.exists():
                marker.unlink()
            logfile.write_text('')
            result=subprocess.run(['bash',str(fixture/'ci/changes.sh'),'--controls'],env=env,capture_output=True,text=True)
            actual=logfile.read_text().splitlines()
            expected=['baseline-gate','kept','new'] + (['head-gate'] if mode == 'changed' else []) + (['private'] if private else [])
            if result.returncode != 0 or sorted(actual) != sorted(expected):
                failures.append((mode,private,result.returncode,actual,result.stderr))
            else:
                probes += 1
            print(('PASS' if sorted(actual) == sorted(expected) and result.returncode == 0 else 'FAIL') + f': baseline controls survive {mode}, private={private}: {actual}')
        out=subprocess.check_output(['bash',str(fixture/'ci/changes.sh')],env=env,text=True)
        selected=dict(line.split('=',1) for line in out.splitlines())
        if selected.get('control_gate') != 'true':
            failures.append((mode,'missing baseline control switch',selected))
        else:
            probes += 1
    probe(not failures, failures)
    logfile.write_text('')
    env['CONTROL_FAIL']='17'
    result=subprocess.run(['bash',str(fixture/'ci/changes.sh'),'--controls'],env=env,capture_output=True,text=True)
    probe(result.returncode != 0 and 'baseline-gate' in logfile.read_text().splitlines() and '17' in result.stderr, result)
    print('PASS: a retained baseline control failure rejects the controls run')
    env.pop('CONTROL_FAIL')
    # Removing a control means deleting both its row and the file it runs.
    gate_test=(fixture/'ci/gate-test.sh').read_bytes()
    (fixture/'ci/gate-test.sh').unlink()
    removed=[line for line in baseline.splitlines(keepends=True) if not line.startswith(('controls:private\t','controls:gate\t'))]
    (fixture/'ci/checks.tsv').write_text(''.join(removed))
    logfile.write_text('')
    result=subprocess.run(['bash',str(fixture/'ci/changes.sh'),'--controls'],env=env,capture_output=True,text=True)
    probe(result.returncode == 0 and 'baseline-gate' not in logfile.read_text().splitlines()
          and 'controls: gate removed with ci/gate-test.sh; not run' in result.stdout, result)
    print('PASS: a control removed with its file is not run from the baseline')
    kept=[line for line in baseline.splitlines(keepends=True) if not line.startswith('controls:private\t')]
    (fixture/'ci/checks.tsv').write_text(''.join(kept))
    result=subprocess.run(['bash',str(fixture/'ci/changes.sh'),'--controls'],env=env,capture_output=True,text=True)
    probe(result.returncode != 0, result)
    print('PASS: a registered control whose file is deleted fails')
    (fixture/'ci/gate-test.sh').write_bytes(gate_test)
print(f'changes-test: {passed} routing cases and {probes} assertions/validation probes passed')
PY
