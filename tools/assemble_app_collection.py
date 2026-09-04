#!/usr/bin/env python3
"""Assemble a deterministic app collection from reusable qpkg artifacts."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import sys


ARCHITECTURES = {"x86_64", "aarch64"}


def assemble(plan: dict, architecture: str, qpkg_dir: Path, source: str) -> dict:
    if architecture not in ARCHITECTURES:
        raise ValueError(f"unsupported architecture: {architecture}")
    entries = []
    expected = set()
    for app in plan.get("catalog", []):
        name = app["name"]
        version = app["version"]
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]*", name):
            raise ValueError(f"unsafe app name: {name}")
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]*", version):
            raise ValueError(f"unsafe app version: {version}")
        filename = f"{name}-{version}-{architecture}.qpkg"
        path = qpkg_dir / filename
        if not path.is_file() or path.is_symlink():
            raise ValueError(f"missing regular qpkg: {filename}")
        expected.add(filename)
        entries.append(
            {
                **app,
                "file": filename,
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            }
        )
    actual = {path.name for path in qpkg_dir.glob("*.qpkg")}
    if actual != expected:
        extra = ", ".join(sorted(actual - expected)) or "none"
        missing = ", ".join(sorted(expected - actual)) or "none"
        raise ValueError(f"qpkg set mismatch (missing: {missing}; extra: {extra})")
    return {"format": 1, "architecture": architecture, "source": source, "apps": entries}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--architecture", required=True)
    parser.add_argument("--qpkg-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--source", required=True)
    args = parser.parse_args()
    try:
        plan = json.loads(args.plan.read_text(encoding="utf-8"))
        result = assemble(plan, args.architecture, args.qpkg_dir, args.source)
        args.output.write_text(
            json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
    except (KeyError, OSError, ValueError, json.JSONDecodeError) as error:
        print(f"app collection assembly: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
