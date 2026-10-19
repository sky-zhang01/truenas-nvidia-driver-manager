#!/usr/bin/env python3
"""Read the newest setup code from the program's structured App log."""
import json
import re
import sys


def main():
    newest = None
    pattern = re.compile(r"^SETUP CODE: ([0-9A-HJKMNP-TV-Z]{4}(?:-[0-9A-HJKMNP-TV-Z]{4}){3})\. ")
    for line in sys.stdin:
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        if not isinstance(entry, dict) or entry.get("event") != "setup_code_issued":
            continue
        message = entry.get("msg")
        match = pattern.match(message) if isinstance(message, str) else None
        if match is None or entry.get("valid_hours") != 24:
            print("setup-code: malformed setup_code_issued event", file=sys.stderr)
            return 1
        newest = match.group(1)
    if newest is None:
        print("setup-code: no setup_code_issued message found", file=sys.stderr)
        return 1
    print(newest)
    return 0


if __name__ == "__main__":
    sys.exit(main())
