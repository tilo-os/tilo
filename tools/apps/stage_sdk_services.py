#!/usr/bin/env python3
"""Declare the current SDK identity protocol before a fresh app is signed.

This is a build-time operation on a staged manifest, never normalization of an
installed or downloaded manifest. Explicit app requirements remain authoritative.
"""
import argparse
import copy
from pathlib import Path
import re
import tomllib


# The identity service is part of the runtime interface and speaks its one
# version, the one os/usr/share/tilo/system-interfaces.toml serves
# (docs/architecture/compatibility.md; tools/tests/test_stage_sdk_services.py
# holds the two equal).
IDENTITY_REQUIREMENT = "optional@8"


def stage(manifest: Path) -> None:
    source = manifest.read_text()
    original = tomllib.loads(source)
    if original.get("package", {}).get("kind") not in {"app", "service"}:
        raise ValueError("SDK service staging requires an app or service manifest")
    runtime = original["package"].get("runtime", "")
    if not re.fullmatch(r"tilo-runtime/[0-9A-Za-z.-]+\+native\.[0-9a-f]{32}", runtime):
        raise ValueError("SDK service staging requires the exact linked native runtime")
    services = original.get("sandbox", {}).get("services", {})
    if "identity" in services:
        # A deliberately retained protocol contract must not be upgraded behind
        # its author's back. The launcher validates its version at launch.
        return
    expected = copy.deepcopy(original)
    expected.setdefault("sandbox", {}).setdefault("services", {})["identity"] = IDENTITY_REQUIREMENT
    section = re.search(r"(?m)^\[sandbox\.services\][ \t]*(?:#[^\n]*)?$", source)
    declaration = f'identity = "{IDENTITY_REQUIREMENT}"\n'
    if section:
        split = section.end()
        updated = source[:split] + "\n" + declaration + source[split:].lstrip("\n")
    elif "services" not in original.get("sandbox", {}):
        updated = source.rstrip("\n") + "\n\n[sandbox.services]\n" + declaration
    else:
        raise ValueError("SDK services must use a [sandbox.services] table")
    if tomllib.loads(updated) != expected:
        raise ValueError("SDK service staging changed unrelated manifest fields")
    manifest.write_text(updated)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    args = parser.parse_args()
    stage(args.manifest)


if __name__ == "__main__":
    main()
