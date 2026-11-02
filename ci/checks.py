#!/usr/bin/env python3
"""Shared input routing, control selection and image content identity."""
import csv
import fnmatch
import hashlib
import os
from pathlib import Path
import shlex
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
JOBS = ('guards', 'controls', 'web', 'compose', 'rust', 'e2e', 'image', 'build_kit', 'osv', 'leak')



def read_table(path, private=None, content=None, extra=None):
    """Read the exported table plus the private-only table when one exists.

    `extra` is the private table's text; None reads the sibling file if present.
    """
    if private is None:
        private = (path.parent.parent / "export/rules").exists()
    if extra is None:
        sibling = path.with_name('checks-private.tsv')
        extra = sibling.read_text() if content is None and sibling.exists() else ''
    rows = []
    for text in (path.read_text() if content is None else content, extra):
        for line in text.splitlines():
            if not line.strip() or line.startswith('#'):
                continue
            row = next(csv.reader([line], delimiter='\t'))
            if len(row) != 5 or not row[1] or not row[3] or row[4] not in ('all', 'private'):
                raise ValueError('checks.tsv requires check, globs, command, explanation and context')
            check, patterns, command, why, context = row
            if context == 'private' and not private:
                continue
            if check not in (*JOBS, '', 'leak-skip', 'all') and not check.startswith('controls:'):
                raise ValueError('unknown check: ' + check)
            if check.startswith('controls:') and command == '-':
                raise ValueError('control has no command: ' + check)
            rows.append((check, patterns.split(';'), command, why))
    return rows


def matches(path, patterns):
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in patterns)


def full(rows):
    result = {key: True for key in JOBS}
    result.update({'control_' + check.split(':')[1]: True for check, _, _, _ in rows if check.startswith('controls:')})
    result['locks'] = True
    return result


def select(rows, changes):
    result = {key: False for key in full(rows)}
    result['guards'] = True
    if not changes:
        return full(rows)
    for status, path in changes:
        if any(check == 'all' and matches(path, patterns) for check, patterns, _, _ in rows):
            return full(rows)
        hits = [row for row in rows if matches(path, row[1])]
        if not hits:
            print('changes: unregistered path ' + path + '; selecting all jobs', file=sys.stderr)
            return full(rows)
        for check, _, _, _ in hits:
            if check in JOBS:
                result[check] = True
            elif check.startswith('controls:'):
                result['controls'] = result['control_' + check.split(':')[1]] = True
        # Content edits to excluded, unread files are the only leak exemption.
        if status != 'M':
            result['controls'] = result['control_changes'] = True
        if status != 'M' or not any(row[0] == 'leak-skip' for row in hits):
            result['leak'] = True
    result['locks'] = result['osv']
    return result


def git(*args):
    return subprocess.check_output(['git', *args], cwd=os.getcwd(), stderr=subprocess.PIPE)


def repository_paths(root):
    output = subprocess.check_output(['git', 'ls-files', '--cached', '--others', '--exclude-standard', '-z'], cwd=root)
    return sorted({os.fsdecode(path) for path in output.split(b'\0') if path and ((root / os.fsdecode(path)).exists() or (root / os.fsdecode(path)).is_symlink())})


def validate(rows, paths):
    errors = []
    for path in paths:
        if not any(matches(path, row[1]) for row in rows):
            errors.append('unregistered path: ' + path)
    for _, patterns, _, _ in rows:
        for pattern in patterns:
            if not any(matches(path, [pattern]) for path in paths):
                errors.append('glob matches no repository path: ' + pattern)
    if errors:
        raise ValueError('\n'.join(errors))


def routed(rows, base=None):
    """Return switches and baseline rows whose controls must also execute."""
    scope = os.environ.get('CHANGES_SCOPE', '')
    if scope:
        scope = {'build-script': 'build_kit', 'public-leak': 'leak'}.get(scope, scope)
        if scope == 'all':
            return full(rows), []
        if scope not in JOBS:
            raise ValueError('unknown CHANGES_SCOPE: ' + scope)
        result = {key: False for key in full(rows)}
        result['guards'] = result[scope] = True
        if scope == 'controls':
            result.update({key: True for key in result if key.startswith('control_')})
        result['locks'] = result['osv']
        return result, []
    if os.environ.get('GITHUB_EVENT_NAME', 'pull_request') != 'pull_request':
        return full(rows), []
    base = base or os.environ.get('CHANGES_BASE', '')
    if not base or base == '0' * 40:
        return full(rows), []
    head = os.environ.get('CHANGES_HEAD') or 'HEAD'
    try:
        # A base-branch tip is not a diff base when the branches diverged.
        ancestor = git('merge-base', base, head).decode().strip()
        raw = git('diff', '--raw', '--no-renames', '-z', ancestor, head).split(b'\0')
        # Base and head use the same table schema. A PR cannot delete or narrow
        # its own full-run rule and then rely on the weakened head table.
        try:
            old_private = git('show', ancestor + ':ci/checks-private.tsv').decode()
        except subprocess.CalledProcessError:
            old_private = ''
        previous = read_table(ROOT / 'ci/checks.tsv', content=git('show', ancestor + ':ci/checks.tsv').decode(), extra=old_private)
        changes = []
        for i in range(0, len(raw) - 1, 2):
            metadata = raw[i].decode().split()
            if len(metadata) != 5 or not metadata[0].startswith(':'):
                raise ValueError('unreadable diff metadata')
            status = metadata[4]
            if status == 'M' and (metadata[0][1:] != metadata[1] or metadata[1] not in ('100644', '100755')):
                status = 'T'
            changes.append((status, os.fsdecode(raw[i + 1])))
        if any(check == 'all' and matches(path, patterns)
               for _, path in changes for check, patterns, _, _ in previous):
            return full(rows + previous), previous
        return select(rows, changes), []
    except (subprocess.CalledProcessError, ValueError) as error:
        print('changes: unreadable diff; selecting all jobs (' + str(error) + ')', file=sys.stderr)
        return full(rows), []


def main():
    rows = read_table(ROOT / 'ci/checks.tsv')
    args = sys.argv[1:]
    if args and args[0] == '--classify':
        if len(args) not in (2, 3):
            raise ValueError('usage: changes.sh --classify PATH [STATUS]')
        selected = select(rows, [(args[2] if len(args) == 3 else 'M', args[1])])
        print(' '.join(key for key in JOBS if selected[key]))
    elif args and args[0] in ('--inputs', '--hash'):
        if len(args) != 2 or args[1] not in JOBS:
            raise ValueError('usage: changes.sh --inputs|--hash JOB')
        paths = [p for p in repository_paths(ROOT) if any(check == args[1] and matches(p, patterns) for check, patterns, _, _ in rows)]
        if not paths:
            raise ValueError('no inputs for ' + args[1])
        if args[0] == '--inputs':
            sys.stdout.buffer.write(b'\0'.join(os.fsencode(p) for p in paths) + b'\0')
        else:
            digest = hashlib.sha256()
            for path in paths:
                file = ROOT / path
                mode = '120000' if file.is_symlink() else ('100755' if file.stat().st_mode & 0o111 else '100644')
                data = os.fsencode(os.readlink(file)) if file.is_symlink() else file.read_bytes()
                digest.update(os.fsencode(path) + b'\0' + mode.encode() + b'\0' + str(len(data)).encode() + b'\0' + data)
            print(digest.hexdigest())
    elif args == ['--validate']:
        validate(rows, repository_paths(ROOT))
    elif args == ['--controls']:
        selected, previous = routed(rows)
        commands = {}
        for table in (previous, rows):
            table_commands = {}
            for check, _, command, _ in table:
                if check.startswith('controls:'):
                    name = check.split(':')[1]
                    if name in table_commands and table_commands[name] != command:
                        raise ValueError('conflicting control command: ' + name)
                    table_commands[name] = command
            # Deduplicate identical commands, while retaining a changed head
            # command alongside its baseline command under the same control.
            commands.update({(name, command): None for name, command in table_commands.items()})
        current = {check.split(':')[1] for check, _, _, _ in rows if check.startswith('controls:')}
        for name, command in commands:
            if not selected['control_' + name]:
                continue
            # A change that removes a control from the table and deletes the file
            # it runs has removed that control; any other baseline control runs.
            missing = [arg for arg in shlex.split(command)[1:] if '/' in arg and not arg.startswith('-') and not (ROOT / arg).exists()]
            if name not in current and missing:
                print('controls: ' + name + ' removed with ' + ', '.join(missing) + '; not run', flush=True)
                continue
            print('controls: ' + name, flush=True)
            subprocess.run(shlex.split(command), cwd=ROOT, check=True)
    else:
        if len(args) > 1 or (args and args[0].startswith('--')):
            raise ValueError('usage: changes.sh [BASE]')
        selected, _ = routed(rows, args[0] if args else None)
        output = ''.join(key + '=' + str(value).lower() + '\n' for key, value in selected.items())
        output += 'scope=' + os.environ.get('CHANGES_SCOPE', '') + '\n'
        if os.environ.get('GITHUB_OUTPUT'):
            with open(os.environ['GITHUB_OUTPUT'], 'a') as stream:
                stream.write(output)
        else:
            print(output, end='')


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print('checks: ' + str(error), file=sys.stderr)
        sys.exit(1)
