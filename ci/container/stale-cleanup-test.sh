#!/usr/bin/env bash
# Run deterministic cleanup controls against a local Docker stub, never a daemon.
set -Eeuo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
python3 - "${SCRIPT_DIR}/stale-cleanup.py" <<'PY'
import contextlib
import datetime
import importlib.util
import io
import json
import os
import subprocess
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
SCRIPT = Path(sys.argv.pop())
NOW = datetime.datetime(2026, 10, 17, 12, tzinfo=datetime.timezone.utc)

STUB = '''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
p = Path(os.environ['CLEANUP_STUB_STATE'])
s = json.loads(p.read_text())
args = sys.argv[1:]
kind, op = args[:2]
s['calls'].append(args)
def save(): p.write_text(json.dumps(s))
if s.get('fail') == kind + ':' + op:
    save(); print('private daemon error', file=sys.stderr); sys.exit(7)
rows = s[kind]
if op == 'ls':
    if s.get('fail_listing_after_inspect') and s.get('inspect_attempted'):
        save(); sys.exit(8)
    if s.get('readback_fail') and s.get('removed'):
        save(); sys.exit(8)
    expected = {'container': '{{.ID}}\\t{{.Names}}', 'network': '{{.ID}}\\t{{.Name}}', 'volume': '{{.Name}}'}[kind]
    # The exact field-only contract prevents accidental full-row rendering.
    assert args[args.index('--format') + 1] == expected
    if s.get('bad_listing'):
        sys.stdout.buffer.write(b'broken\\tnvmct-invalid\\textra\\n')
    else:
        extra = s.get('listing_lines', {}).get(kind, [])
        for line in extra:
            sys.stdout.buffer.write(bytes.fromhex(line) + b'\\n')
        for r in rows:
            name = r['Name'].lstrip('/')
            line = name if kind == 'volume' else r.get('Id', '') + '\\t' + name
            sys.stdout.buffer.write(line.encode('utf-8') + b'\\n')
    if s.get('partial_listing_failure') == kind:
        save(); print('private daemon error', file=sys.stderr); sys.exit(17)
elif op == 'inspect':
    if s.get('inspect_race') == [kind, args[2]]:
        s['inspect_attempted'] = True
        if s.get('vanish'):
            s[kind] = [r for r in rows if args[2] not in (r.get('Id'), r['Name'].lstrip('/'))]
        if s.get('replacement'):
            s[kind].append(s['replacement'])
        save(); print('private inspect error', file=sys.stderr); sys.exit(7)
    if s.get('bad_inspect_by_identity', {}).get(args[2]) is not None:
        save(); sys.stdout.buffer.write(bytes.fromhex(s['bad_inspect_by_identity'][args[2]])); sys.exit(0)
    if s.get('bad_inspect'):
        save(); print(s['bad_inspect']); sys.exit(0)
    sys.stdout.buffer.write(json.dumps([r for r in rows if args[2] in (r.get('Id'), r['Name'].lstrip('/'))], ensure_ascii=False).encode('utf-8'))
elif op == 'rm':
    assert len(args) == 3, 'cleanup must not force deletion'
    if not s.get('survive'):
        s[kind] = [r for r in rows if args[2] not in (r.get('Id'), r['Name'].lstrip('/'))]
    s['removed'] = True
else:
    save(); sys.exit(9)
save()
'''


def resource(kind, name, age=7, project=None, status='exited'):
    labels = {'com.docker.compose.project': project} if project else {}
    item = {'Name': '/' + name if kind == 'container' else name,
            'CreatedAt' if kind == 'volume' else 'Created':
                (NOW - datetime.timedelta(hours=age)).isoformat(), 'Labels': labels}
    if kind != 'volume':
        item['Id'] = name + '-id'
    if kind == 'container':
        item['Config'] = {'Labels': labels}
        item['State'] = {'Status': status}
    return item


class CleanupControls(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location('stale_cleanup', SCRIPT)
        self.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.module)
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name)
        docker = self.path / 'docker'
        docker.write_text(STUB)
        docker.chmod(0o755)
        self.state = {kind: [] for kind in ('container', 'volume', 'network')}
        self.state['calls'] = []

    def run_cleanup(self):
        state_file = self.path / 'state.json'
        state_file.write_text(json.dumps(self.state))
        stdout, stderr = io.StringIO(), io.StringIO()
        with patch.dict(os.environ, {'PATH': str(self.path) + os.pathsep + os.environ['PATH'],
                                      'CLEANUP_STUB_STATE': str(state_file)}), \
             patch.object(self.module, 'datetime') as clock, \
             contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            clock.now.return_value = NOW
            clock.fromisoformat.side_effect = datetime.datetime.fromisoformat
            result = self.module.main()
        self.state = json.loads(state_file.read_text())
        self.output = stdout.getvalue() + stderr.getvalue()
        return result

    def removals(self):
        return [call for call in self.state['calls'] if call[1] == 'rm']

    def test_only_old_exact_prefix_and_dependency_order(self):
        for kind in ('container', 'volume', 'network'):
            self.state[kind] = [resource(kind, 'nvmct-old'), resource(kind, 'nvmct-recent', 1),
                                resource(kind, 'nvmct-boundary', 6), resource(kind, 'foreign-old'),
                                resource(kind, 'foreign-nvmct-middle')]
        self.assertEqual(self.run_cleanup(), 0)
        self.assertEqual(self.removals(), [['container', 'rm', 'nvmct-old-id'],
                                          ['volume', 'rm', 'nvmct-old'],
                                          ['network', 'rm', 'nvmct-old-id']])
        self.assertNotIn('nvmct-', self.output)
        self.assertIn('containers=1 volumes=1 networks=1', self.output)

    def test_active_and_recent_container_protect_whole_project(self):
        for index, status in enumerate(('running', 'restarting', 'paused', 'exited')):
            project = 'nvmct-project-' + str(index)
            # Foreign container names must also protect their compose project.
            self.state['container'].append(resource('container', 'foreign-' + str(index),
                                                    1 if status == 'exited' else 8, project, status))
            for kind in ('container', 'volume', 'network'):
                self.state[kind].append(resource(kind, 'nvmct-sibling-' + str(index), 8, project))
        self.assertEqual(self.run_cleanup(), 0)
        self.assertEqual(self.removals(), [])

    def test_active_standalone_container_is_preserved(self):
        self.state['container'] = [resource('container', 'nvmct-active', 8, status='running')]
        self.assertEqual(self.run_cleanup(), 0)
        self.assertEqual(self.removals(), [])

    def test_preflight_errors_never_delete(self):
        for error in ('container:ls', 'volume:ls', 'network:ls', 'container:inspect',
                      'volume:inspect', 'network:inspect'):
            with self.subTest(error=error):
                self.state = {kind: [resource(kind, 'nvmct-old')] for kind in ('container', 'volume', 'network')}
                self.state.update(calls=[], fail=error)
                self.assertNotEqual(self.run_cleanup(), 0)
                self.assertEqual(self.removals(), [])
                self.assertNotIn('private daemon error', self.output)
                self.assertNotIn('nvmct-', self.output)

    def test_invalid_listing_and_timestamp_fail_closed(self):
        self.state['network'] = [resource('network', 'nvmct-old')]
        self.state['bad_listing'] = True
        self.assertNotEqual(self.run_cleanup(), 0)
        self.assertEqual(self.removals(), [])
        self.state.pop('bad_listing')
        self.state['network'][0]['Created'] = 'yesterday'
        self.assertNotEqual(self.run_cleanup(), 0)
        self.assertEqual(self.removals(), [])

    def test_empty_inspect_and_invalid_metadata_fail_closed(self):
        for bad in ('[]', 'null', '{}', 'invalid json'):
            with self.subTest(bad=bad):
                self.state = {'container': [], 'volume': [],
                              'network': [resource('network', 'nvmct-old')],
                              'calls': [], 'bad_inspect': bad}
                self.assertNotEqual(self.run_cleanup(), 0)
                self.assertEqual(self.removals(), [])
        for field, value in (('Created', '2026-10-17T01:00:00'), ('Labels', [])):
            with self.subTest(field=field):
                item = resource('network', 'nvmct-old')
                item[field] = value
                self.state = {'container': [], 'volume': [], 'network': [item], 'calls': []}
                self.assertNotEqual(self.run_cleanup(), 0)
                self.assertEqual(self.removals(), [])

    def test_fractional_seconds_and_timezone_offsets(self):
        self.state['network'] = [resource('network', 'nvmct-old')]
        self.state['network'][0]['Created'] = '2026-10-17T13:59:59.999999999+09:00'
        self.assertEqual(self.run_cleanup(), 0)
        self.assertEqual(self.removals(), [['network', 'rm', 'nvmct-old-id']])

    def test_failed_container_removal_stops_before_dependent_resources(self):
        self.state = {kind: [resource(kind, 'nvmct-old')] for kind in ('container', 'volume', 'network')}
        self.state.update(calls=[], fail='container:rm')
        self.assertNotEqual(self.run_cleanup(), 0)
        self.assertEqual(self.removals(), [['container', 'rm', 'nvmct-old-id']])

    def test_concurrently_vanished_resources_are_skipped_after_exact_readback(self):
        for kind, name in (('container', 'foreign-short-lived'), ('container', 'nvmct-old'),
                           ('volume', 'nvmct-old'), ('network', 'nvmct-old')):
            with self.subTest(kind=kind, name=name):
                item = resource(kind, name)
                identity = item.get('Id', name)
                # A similar name or ID must not be mistaken for this exact resource.
                sibling = resource(kind, name + '-sibling', 1)
                self.state = {'container': [], 'volume': [], 'network': [], 'calls': [],
                              'inspect_race': [kind, identity], 'vanish': True}
                self.state[kind] = [item, sibling]
                self.assertEqual(self.run_cleanup(), 0)
                self.assertEqual(self.removals(), [])
                self.assertNotIn('private inspect error', self.output)

    def test_inspect_race_requires_successful_readback_and_exact_absence(self):
        for condition in ('listing-failure', 'still-present', 'same-name-replacement', 'same-id-renamed'):
            with self.subTest(condition=condition):
                item = resource('container', 'nvmct-short-lived')
                self.state = {'container': [item], 'volume': [], 'network': [], 'calls': [],
                              'inspect_race': ['container', item['Id']], 'vanish': condition != 'still-present'}
                if condition == 'listing-failure':
                    self.state['fail_listing_after_inspect'] = True
                if condition == 'same-name-replacement':
                    self.state['replacement'] = {**item, 'Id': item['Id'] + '-replacement'}
                if condition == 'same-id-renamed':
                    self.state['replacement'] = {**item, 'Name': item['Name'] + '-renamed'}
                self.assertNotEqual(self.run_cleanup(), 0)
                self.assertEqual(self.removals(), [])
                self.assertNotIn('nvmct-short-lived', self.output)

    def test_cli_entrypoint_empty_daemon(self):
        state_file = self.path / 'state.json'
        state_file.write_text(json.dumps(self.state))
        result = subprocess.run([sys.executable, str(SCRIPT)], capture_output=True, text=True,
                                env={**os.environ, 'PATH': str(self.path) + os.pathsep + os.environ['PATH'],
                                     'CLEANUP_STUB_STATE': str(state_file)})
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, 'stale cleanup: containers=0 volumes=0 networks=0\n')
        self.assertEqual(result.stderr, '')

    def test_removal_error_survivor_and_readback_error_are_failures(self):
        for injected in ({'fail': 'volume:rm'}, {'survive': True}, {'readback_fail': True}):
            with self.subTest(injected=injected):
                self.state = {'container': [], 'network': [],
                              'volume': [resource('volume', 'nvmct-old')], 'calls': [], **injected}
                self.assertNotEqual(self.run_cleanup(), 0)
                self.assertNotIn('nvmct-', self.output)
                self.assertNotIn('volumes=1', self.output)


    def test_foreign_listing_anomalies_do_not_abort_cleanup(self):
        for kind in ('container', 'volume', 'network'):
            lines = ([b'foreign-id\tforeign-\xff', b'foreign-id\tforeign-' + b'x' * 10000,
                      b'\tforeign-no-id', b'foreign-id\tforeign-extra\textra', b'unrecognizable']
                     if kind != 'volume' else
                     [b'foreign-\xff', b'foreign-' + b'x' * 10000, b'foreign-extra\textra'])
            for line in lines:
                with self.subTest(kind=kind, line=line[:40]):
                    self.state = {k: [resource(k, 'nvmct-old')] for k in ('container', 'volume', 'network')}
                    self.state.update(calls=[], listing_lines={kind: [line.hex()]})
                    self.assertEqual(self.run_cleanup(), 0)
                    self.assertEqual(len(self.removals()), 3)
                    self.assertNotIn('foreign-', self.output)

    def test_foreign_invalid_inspect_is_ignored(self):
        for bad in (b'[]', b'null', b'{}', b'not json', b'\xff', b'[{' + b'x' * 10000):
            with self.subTest(bad=bad[:30]):
                foreign = resource('container', 'foreign-invalid')
                self.state = {kind: [resource(kind, 'nvmct-old')] for kind in ('container', 'volume', 'network')}
                self.state['container'].append(foreign)
                self.state.update(calls=[], bad_inspect_by_identity={foreign['Id']: bad.hex()})
                self.assertEqual(self.run_cleanup(), 0)
                self.assertEqual(len(self.removals()), 3)
                self.assertNotIn('foreign-invalid', self.output)
        # Invalid metadata on foreign containers must also be isolated.
        for field, value in (('Created', 'yesterday'), ('Config', []), ('State', None)):
            with self.subTest(field=field):
                foreign = resource('container', 'foreign-invalid')
                foreign[field] = value
                self.state = {'container': [foreign], 'volume': [resource('volume', 'nvmct-old')],
                              'network': [], 'calls': []}
                self.assertEqual(self.run_cleanup(), 0)
                self.assertEqual(self.removals(), [['volume', 'rm', 'nvmct-old']])

    def test_foreign_failed_inspect_is_ignored(self):
        foreign = resource('container', 'foreign-invalid')
        self.state = {'container': [foreign], 'volume': [resource('volume', 'nvmct-old')],
                      'network': [], 'calls': [], 'inspect_race': ['container', foreign['Id']]}
        self.assertEqual(self.run_cleanup(), 0)
        self.assertEqual(self.removals(), [['volume', 'rm', 'nvmct-old']])
        self.assertNotIn('private inspect error', self.output)

    def test_candidate_invalid_listing_never_deletes(self):
        for kind in ('container', 'volume', 'network'):
            lines = ([b'id\tnvmct-\xff', b'\tnvmct-missing-id', b'nvmct-missing-name',
                      b'id\tnvmct-extra\textra']
                     if kind != 'volume' else
                     [b'nvmct-\xff', b'nvmct-extra\textra'])
            for line in lines:
                with self.subTest(kind=kind, line=line[:40]):
                    self.state = {k: [resource(k, 'nvmct-old')] for k in ('container', 'volume', 'network')}
                    self.state.update(calls=[], listing_lines={kind: [line.hex()]})
                    self.assertNotEqual(self.run_cleanup(), 0)
                    self.assertEqual(self.removals(), [])
                    self.assertNotIn('nvmct-', self.output)

    def test_candidate_duplicate_identity_or_name_never_deletes(self):
        for kind in ('container', 'volume', 'network'):
            for same in ('name', 'id') if kind != 'volume' else ('name',):
                with self.subTest(kind=kind, same=same):
                    first = resource(kind, 'nvmct-first')
                    second = resource(kind, 'nvmct-second')
                    if same == 'name':
                        second['Name'] = first['Name']
                    else:
                        second['Id'] = first['Id']
                    self.state = {'container': [], 'volume': [], 'network': [], 'calls': []}
                    self.state[kind] = [first, second]
                    self.assertNotEqual(self.run_cleanup(), 0)
                    self.assertEqual(self.removals(), [])

    def test_foreign_identity_collision_with_candidate_fails_closed(self):
        for kind in ('container', 'network'):
            with self.subTest(kind=kind):
                owned = resource(kind, 'nvmct-old')
                foreign = resource(kind, 'foreign-old')
                foreign['Id'] = owned['Id']
                self.state = {'container': [], 'volume': [], 'network': [], 'calls': []}
                self.state[kind] = [foreign, owned]
                self.assertNotEqual(self.run_cleanup(), 0)
                self.assertEqual(self.removals(), [])

    def test_candidate_inspect_identity_mismatch_fails_closed(self):
        for kind in ('container', 'volume', 'network'):
            for changed in ('Name', 'Id') if kind != 'volume' else ('Name',):
                with self.subTest(kind=kind, changed=changed):
                    owned = resource(kind, 'nvmct-old')
                    identity = owned.get('Id', 'nvmct-old')
                    altered = {**owned, changed: 'foreign-replacement'}
                    self.state = {'container': [], 'volume': [], 'network': [], 'calls': [],
                                  'bad_inspect_by_identity': {
                                      identity: json.dumps([altered]).encode('utf-8').hex()}}
                    self.state[kind] = [owned]
                    self.assertNotEqual(self.run_cleanup(), 0)
                    self.assertEqual(self.removals(), [])

    def test_listing_partial_failure_reports_rc_without_stderr_or_deletion(self):
        for kind in ('container', 'volume', 'network'):
            with self.subTest(kind=kind):
                self.state = {k: [resource(k, 'nvmct-old')] for k in ('container', 'volume', 'network')}
                self.state.update(calls=[], partial_listing_failure=kind)
                self.assertNotEqual(self.run_cleanup(), 0)
                self.assertEqual(self.removals(), [])
                self.assertIn('17', self.output)
                self.assertNotIn('private daemon error', self.output)
                self.assertNotIn('nvmct-', self.output)

    def test_candidate_bad_inspect_bytes_fail_closed(self):
        for kind in ('container', 'volume', 'network'):
            with self.subTest(kind=kind):
                item = resource(kind, 'nvmct-invalid')
                identity = item.get('Id', 'nvmct-invalid')
                self.state = {'container': [], 'volume': [], 'network': [], 'calls': [],
                              'bad_inspect_by_identity': {identity: b'\xff'.hex()}}
                self.state[kind] = [item]
                self.assertNotEqual(self.run_cleanup(), 0)
                self.assertEqual(self.removals(), [])

    def test_utf8_decoding_does_not_depend_on_ascii_locale(self):
        state_file = self.path / 'state.json'
        # Valid UTF-8 inspect metadata must protect siblings under an ASCII locale.
        foreign = resource('container', 'foreign-valid', 1, 'nvmct-caf\u00e9')
        sibling = resource('volume', 'nvmct-old', 8, 'nvmct-caf\u00e9')
        self.state.update(container=[foreign], volume=[sibling])
        state_file.write_text(json.dumps(self.state))
        code = """import importlib.util, sys
from datetime import datetime, timezone
spec = importlib.util.spec_from_file_location('cleanup', sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
class Clock:
    @staticmethod
    def now(tz):
        return datetime(2026, 10, 17, 12, tzinfo=timezone.utc)
    fromisoformat = staticmethod(datetime.fromisoformat)
module.datetime = Clock
sys.exit(module.main())
"""
        result = subprocess.run([sys.executable, '-c', code, str(SCRIPT)], capture_output=True,
                                env={**os.environ, 'PATH': str(self.path) + os.pathsep + os.environ['PATH'],
                                     'CLEANUP_STUB_STATE': str(state_file), 'LC_ALL': 'C',
                                     'PYTHONUTF8': '0', 'PYTHONCOERCECLOCALE': '0'})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.state = json.loads(state_file.read_text())
        self.assertEqual(self.removals(), [])
        self.assertIn(['container', 'inspect', foreign['Id']], self.state['calls'])


unittest.main(verbosity=2)
PY
