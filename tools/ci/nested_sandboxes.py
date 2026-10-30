#!/usr/bin/env python3
"""Print, one per line, the test packages that start sandboxes of their own.

    nested_sandboxes.py              every package tools/ci/nested-sandboxes.toml lists
    nested_sandboxes.py NAME...      those of NAME... it lists (`-p` arguments are skipped)
"""
from __future__ import annotations

from pathlib import Path
import sys
import tomllib

LIST = Path(__file__).resolve().with_name("nested-sandboxes.toml")


def packages(path: Path = LIST) -> list[str]:
    names = tomllib.loads(path.read_text(encoding="utf-8")).get("packages")
    if not isinstance(names, list) or not names or not all(isinstance(name, str) for name in names):
        raise SystemExit(f"{path}: `packages` must be a non-empty list of package names")
    return sorted(set(names))


def main(argv: list[str]) -> int:
    listed = packages()
    asked = [name for name in argv if name not in ("-p", "--package")]
    for name in listed:
        if not asked or name in asked:
            print(name)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
