#!/usr/bin/env python3
"""Content-address the frozen RPM snapshot inputs.

The fingerprint binds both architectures' package sets (from
derive-rpm-package-set.py), the digest-pinned BASE_REF, and every file that
shapes what snapshot-rpms publishes. Builds resolve
``registry.tilo.so/tilo/rpms:set-<fingerprint>`` instead of a hand-kept pin.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location(
    "rpm_packages", ROOT / "os/dev/derive-rpm-package-set.py"
)
parser = importlib.util.module_from_spec(spec)
spec.loader.exec_module(parser)

# Files whose bytes change what the published snapshot contains. Package-set
# sources are hashed through derive-rpm-package-set.py instead so a comment-only
# Containerfile edit that leaves the specs alone does not bust the fingerprint.
SHAPE_FILES = (
    "os/Containerfile.rpms",
    "os/dev/layer-rpm-snapshot.py",
    "os/dev/verify-rpm-snapshot-signatures.sh",
    "os/scripts/verify-no-cisco-openh264.sh",
)
BASE_ASSIGNMENT = re.compile(r"^BASE_REF=(registry\.tilo\.so/tilo/fedora-bootc@sha256:[0-9a-f]{64})$")


def base_ref(repo: Path) -> str:
    assignments = [
        line
        for line in (repo / "os/base-snapshot.env").read_text(encoding="utf-8").splitlines()
        if line and not line.startswith("#")
    ]
    matches = [BASE_ASSIGNMENT.fullmatch(line) for line in assignments]
    hits = [match.group(1) for match in matches if match]
    if len(hits) != 1:
        raise ValueError("os/base-snapshot.env must declare exactly one BASE_REF")
    return hits[0]


def package_set(repo: Path, arch: str) -> list[str]:
    packages: set[str] = set()
    for source in parser.SOURCES:
        packages.update(parser.packages(repo / source, arch))
    if not packages:
        raise ValueError(f"empty {arch} package set")
    return sorted(packages)


def canonical_document(repo: Path) -> bytes:
    """Stable bytes hashed into the fingerprint."""
    chunks: list[str] = [f"BASE_REF={base_ref(repo)}\n"]
    for arch in ("x86_64", "aarch64"):
        chunks.append(f"--- package-set {arch} ---\n")
        chunks.extend(f"{package}\n" for package in package_set(repo, arch))
    for relative in SHAPE_FILES:
        path = repo / relative
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        chunks.append(f"--- file {relative} sha256:{digest} ---\n")
    return "".join(chunks).encode("utf-8")


def fingerprint(repo: Path = ROOT) -> str:
    return hashlib.sha256(canonical_document(repo)).hexdigest()


def tag(repo: Path = ROOT) -> str:
    return f"set-{fingerprint(repo)}"


def reference(repo: Path = ROOT) -> str:
    return f"registry.tilo.so/tilo/rpms:{tag(repo)}"


def main() -> int:
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument(
        "--format",
        choices=("fingerprint", "tag", "reference", "document"),
        default="fingerprint",
        help="what to print (default: fingerprint)",
    )
    cli.add_argument(
        "--repo",
        type=Path,
        default=ROOT,
        help="repository root (default: this checkout)",
    )
    args = cli.parse_args()
    repo = args.repo.resolve()
    if args.format == "fingerprint":
        print(fingerprint(repo))
    elif args.format == "tag":
        print(tag(repo))
    elif args.format == "reference":
        print(reference(repo))
    else:
        print(canonical_document(repo).decode("utf-8"), end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
