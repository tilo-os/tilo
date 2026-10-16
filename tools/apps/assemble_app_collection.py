#!/usr/bin/env python3
"""Assemble a deterministic app collection from reusable tapp artifacts."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import sys


ARCHITECTURES = {"x86_64", "aarch64"}


def assemble(
    plan: dict,
    architecture: str,
    tapp_dir: Path,
    runtime_dir: Path,
    source: str,
) -> dict:
    if architecture not in ARCHITECTURES:
        raise ValueError(f"unsupported architecture: {architecture}")
    publisher_file = runtime_dir / "runtime-bundle-publisher.hex"
    publisher = publisher_file.read_text(encoding="utf-8").strip()
    if not re.fullmatch(r"[0-9a-fA-F]{64}", publisher):
        raise ValueError("runtime bundle publisher must be a 64-hex Ed25519 key")
    bundle_root = runtime_dir / "tilo-runtime"
    versions = sorted(
        path.name
        for path in bundle_root.iterdir()
        if path.is_dir() and not path.is_symlink() and (path / "bundle.toml").is_file()
    )
    if len(versions) != 1:
        raise ValueError("collection must contain exactly one signed runtime version")
    runtime_version = versions[0]
    # Every member links this exact runtime build, and its signed manifest
    # names it in place of the release its authored manifest declares.
    if not re.fullmatch(r"[0-9A-Za-z.-]+\+native\.[0-9a-f]{32}", runtime_version):
        raise ValueError(f"runtime {runtime_version!r} is not named by its ABI digest")
    runtime_id = f"tilo-runtime/{runtime_version}"
    catalog = plan.get("catalog")
    if not isinstance(catalog, list) or not catalog:
        raise ValueError("collection requires a nonempty app catalog")
    entries = []
    expected = set()
    names, identities = set(), set()
    for app in catalog:
        name = app["name"]
        version = app["version"]
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]*", name):
            raise ValueError(f"unsafe app name: {name}")
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]*", version):
            raise ValueError(f"unsafe app version: {version}")
        identity = app.get("id")
        if name in names or (identity is not None and identity in identities):
            raise ValueError("duplicate collection app name or identity")
        names.add(name)
        identities.add(identity)
        filename = f"{name}-{version}-{architecture}.tapp"
        path = tapp_dir / filename
        if not path.is_file() or path.is_symlink():
            raise ValueError(f"missing regular tapp: {filename}")
        expected.add(filename)
        release = runtime_id.split("+native.", 1)[0]
        if app.get("runtime") not in (release, runtime_id):
            raise ValueError(f"{name} declares runtime {app.get('runtime')!r}, not {release!r}")
        entries.append(
            {
                **app,
                "runtime": runtime_id,
                "file": filename,
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            }
        )
    actual = {path.name for path in tapp_dir.glob("*.tapp")}
    if actual != expected:
        extra = ", ".join(sorted(actual - expected)) or "none"
        missing = ", ".join(sorted(expected - actual)) or "none"
        raise ValueError(f"tapp set mismatch (missing: {missing}; extra: {extra})")
    return {
        "format": 1,
        "architecture": architecture,
        "source": source,
        "runtime": {"version": runtime_version, "publisher": publisher.lower()},
        "apps": entries,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--architecture", required=True)
    parser.add_argument("--tapp-dir", type=Path, required=True)
    parser.add_argument("--runtime-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--source", required=True)
    args = parser.parse_args()
    try:
        plan = json.loads(args.plan.read_text(encoding="utf-8"))
        result = assemble(
            plan, args.architecture, args.tapp_dir, args.runtime_dir, args.source
        )
        args.output.write_text(
            json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
    except (KeyError, OSError, ValueError, json.JSONDecodeError) as error:
        print(f"app collection assembly: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
