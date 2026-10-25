#!/usr/bin/env python3
"""Print, one per line, the packages a `test` family of t.toml runs with nested sandboxes.

Those packages' tests start namespaces of their own, which t's job sandbox allows
only as uid 1000; every other package is tested as uid 0. The nightly workspace
run (tools/ci/nightly-workspace-test.sh) splits the workspace the same way.
"""
from __future__ import annotations

from pathlib import Path
import sys
import tomllib

T_TOML = Path(__file__).resolve().parents[2] / "t.toml"
FAMILIES = ("test", "test-x86_64")


def sandboxed(path: Path = T_TOML) -> list[str]:
    config = tomllib.loads(path.read_text(encoding="utf-8"))
    names: set[str] = set()
    for family in config.get("cargo", []):
        if family.get("name") in FAMILIES:
            nested = family.get("nested_sandboxes", [])
            if not isinstance(nested, list):
                raise SystemExit(f"t.toml: {family['name']} names no packages in nested_sandboxes")
            names.update(nested)
    if not names:
        raise SystemExit("t.toml: no test family runs packages with nested sandboxes")
    return sorted(names)


def main() -> int:
    print("\n".join(sandboxed()))
    return 0


if __name__ == "__main__":
    sys.exit(main())
