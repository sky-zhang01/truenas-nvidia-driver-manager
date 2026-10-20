#!/usr/bin/env python3
"""Keep the copyable installation YAML in both READMEs equal to the template."""
import argparse
import difflib
from pathlib import Path
import re
import sys


def effective_lines(text):
    """Drop blank lines and YAML comments, preserving indentation and quoted #."""
    result = []
    for line in text.splitlines():
        quote = None
        index = 0
        while index < len(line):
            char = line[index]
            if quote == '"' and char == '\\':
                index += 2
                continue
            if quote == "'" and line[index:index + 2] == "''":
                index += 2
                continue
            if char == quote:
                quote = None
            elif quote is None:
                if char in "\"'":
                    quote = char
                elif char == '#' and (index == 0 or line[index - 1].isspace()):
                    line = line[:index]
                    break
            index += 1
        line = line.rstrip()
        if line:
            result.append(line)
    return result


def yaml_blocks(text):
    # Commented-out snippets are not instructions a reader can copy.
    blocks = []
    fence = None
    in_comment = False
    lines = []
    is_yaml = False
    for line in text.splitlines():
        if fence is None:
            # HTML is literal within code fences; only hide comments outside them.
            while in_comment or '<!--' in line:
                if in_comment:
                    end = line.find('-->')
                    if end < 0:
                        line = ''
                        break
                    line = line[end + 3:]
                    in_comment = False
                else:
                    start = line.index('<!--')
                    end = line.find('-->', start + 4)
                    if end < 0:
                        line = line[:start]
                        in_comment = True
                        break
                    line = line[:start] + line[end + 3:]
            match = re.fullmatch(r' {0,3}(`{3,}|~{3,})([^`~]*)', line)
            if match:
                fence = match[1]
                is_yaml = match[2].strip().lower() in ('yaml', 'yml')
                lines = []
        elif re.fullmatch(r' {0,3}' + re.escape(fence[0]) + '{' + str(len(fence)) + r',}\s*', line):
            if is_yaml:
                blocks.append('\n'.join(lines))
            fence = None
        else:
            lines.append(line)
    if fence is not None and is_yaml:
        raise ValueError('unclosed YAML fence')
    if len(blocks) != 1:
        raise ValueError(f'expected one visible YAML block, found {len(blocks)}')
    return blocks


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parent.parent)
    root = parser.parse_args().root
    try:
        expected = effective_lines((root / 'deploy/compose.yaml').read_text(encoding='utf-8'))
        if not expected:
            raise ValueError('compose template has no effective lines')
        for name in ('README.md', 'README.zh-CN.md'):
            for block in yaml_blocks((root / name).read_text(encoding='utf-8')):
                actual = effective_lines(block)
                if actual != expected:
                    diff = '\n'.join(difflib.unified_diff(expected, actual, fromfile='deploy/compose.yaml', tofile=name, lineterm=''))
                    raise ValueError(f'{name}: installation YAML differs\n{diff}')
    except (OSError, ValueError) as error:
        print(f'check-readme-compose: {error}', file=sys.stderr)
        return 1
    print('check-readme-compose: both README installation blocks match deploy/compose.yaml')
    return 0


if __name__ == '__main__':
    sys.exit(main())
