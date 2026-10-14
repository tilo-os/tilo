#!/usr/bin/env python3
"""Every vendored directory keeps its licence and records where it came from.

Code under ``vendor/`` is someone else's, carried with Tilo's patches. Each
top-level directory there must hold:

* at least one licence file (a name starting ``LICENSE``, ``LICENCE`` or
  ``COPYING``, any case): the upstream text, byte for byte;
* a ``PROVENANCE.md`` naming the upstream source (registry checksum, or git URL
  and commit), the licence and what Tilo changed. When the directory has a
  ``Cargo.toml``, the record must name the version that manifest declares, so
  a version bump cannot leave a stale record behind.

A nested workspace is checked at its top-level directory only. ``vendor/PATCHES.md``
stays the change log; a ``PROVENANCE.md`` points at it. See docs/policies.md,
"Legal and interoperability".
"""

from __future__ import annotations

from pathlib import Path
import re
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[2]
LICENCE = re.compile(r"^(licen[cs]e|copying)", re.IGNORECASE)
PROVENANCE = "PROVENANCE.md"


def manifest_version(manifest: Path) -> str | None:
    """The version a vendored manifest declares, from ``[package]`` or, for a
    virtual workspace, ``[workspace.package]``."""
    data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    version = data.get("package", {}).get("version")
    if isinstance(version, str):
        return version
    version = data.get("workspace", {}).get("package", {}).get("version")
    return version if isinstance(version, str) else None


def names_version(text: str, version: str) -> bool:
    """Whether ``text`` names ``version`` as a whole version, so ``0.1.0`` is not
    satisfied by ``10.1.0``, ``0.1.0.1`` or ``0.1.0-alpha``."""
    return re.search(rf"(?<![\d.]){re.escape(version)}(?![\d-]|\.\d)", text) is not None


def check(root: Path = ROOT) -> list[str]:
    """Return one line per vendored directory that breaks the rule."""
    vendor = root / "vendor"
    directories = (
        sorted(p for p in vendor.iterdir() if p.is_dir() and not p.name.startswith("."))
        if vendor.is_dir()
        else []
    )
    if not directories:
        return ["vendor/ holds no vendored directories; nothing to check is an error, not a pass"]

    problems = []
    for directory in directories:
        name = f"vendor/{directory.name}"
        if not any(p.is_file() and LICENCE.match(p.name) for p in directory.iterdir()):
            problems.append(
                f"{name}: no licence file (LICENSE*, LICENCE* or COPYING*); "
                "copy the upstream text byte for byte"
            )
        record = directory / PROVENANCE
        if not record.is_file():
            problems.append(f"{name}: no {PROVENANCE} recording upstream version and source")
            continue
        manifest = directory / "Cargo.toml"
        if not manifest.is_file():
            continue
        try:
            version = manifest_version(manifest)
        except tomllib.TOMLDecodeError as error:
            problems.append(f"{name}/Cargo.toml does not parse: {error}")
            continue
        if version is None:
            problems.append(f"{name}/Cargo.toml declares no package version")
        elif not names_version(record.read_text(encoding="utf-8"), version):
            problems.append(
                f"{name}/{PROVENANCE} does not name version {version} from its Cargo.toml"
            )
    return problems


def main() -> int:
    problems = check()
    if problems:
        print("Vendored code keeps its licence and records its upstream version (docs/policies.md):")
        for line in problems:
            print(f"  {line}")
        return 1
    print("Every vendored directory has a licence file and a PROVENANCE.md naming its version.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
