#!/usr/bin/env python3
"""Compare resolved Kconfig values, including omitted disabled symbols."""
import argparse
import re
from pathlib import Path


def settings(text):
    values = {}
    for line in text.splitlines():
        match = re.fullmatch(r'(CONFIG_[A-Za-z0-9_]+)=(.+)', line)
        disabled = re.fullmatch(r'# (CONFIG_[A-Za-z0-9_]+) is not set', line)
        if match:
            name, value = match.groups()
        elif disabled:
            name, value = disabled[1], 'n'
        else:
            continue
        if name in values and values[name] != value:
            raise ValueError(f'conflicting settings for {name}')
        values[name] = value
    return values


def mismatches(resolved, required):
    # Kconfig omits invisible/removed disabled symbols. An omitted y/m/string
    # requirement must still fail, as must any enabled value requested disabled.
    return [(name, want, resolved.get(name, 'n'))
            for name, want in required.items()
            if resolved.get(name, 'n') != want]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('config', type=Path)
    parser.add_argument('--fragment', type=Path)
    parser.add_argument('--expect', action='append', default=[])
    args = parser.parse_args()
    required = settings(args.fragment.read_text() if args.fragment else '')
    required.update(settings('\n'.join(args.expect)))
    if not required:
        parser.error('at least one setting is required')
    errors = mismatches(settings(args.config.read_text()), required)
    for name, want, actual in errors:
        print(f'MISSING {name}={want} (resolved to: {actual})')
    return bool(errors)


if __name__ == '__main__':
    raise SystemExit(main())
