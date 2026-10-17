#!/usr/bin/env python3
"""Every non-empty line of stdin must be an ASCII-only JSON object.

Used on `docker logs` output. A scalar or array is not a JSON object, so it is
rejected together with bad JSON and non-ASCII lines; empty lines are skipped.

The ASCII check runs on the raw line, before any stripping: `str.strip()`
removes non-ASCII whitespace such as U+00A0, which would let a non-ASCII line
pass the check. Only ASCII whitespace is skipped around an otherwise
ASCII line.
"""
import json
import sys

ASCII_WHITESPACE = " \t\r\n\v\f"


def main() -> int:
    for number, line in enumerate(sys.stdin, 1):
        raw = line.rstrip("\n")
        if any(ord(c) > 127 for c in raw):
            print("line %d is not ASCII-only: %s" % (number, raw), file=sys.stderr)
            return 1
        stripped = raw.strip(ASCII_WHITESPACE)
        if not stripped:
            continue
        try:
            value = json.loads(stripped)
        except Exception as exc:
            print("line %d is not JSON: %s (%s)" % (number, stripped, exc), file=sys.stderr)
            return 1
        if not isinstance(value, dict):
            print("line %d is not a JSON object: %s" % (number, stripped), file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
