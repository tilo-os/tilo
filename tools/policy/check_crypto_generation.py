#!/usr/bin/env python3
"""Keep Tilo's own crates on the RustCrypto generation iroh uses.

Two RustCrypto generations are in the lock: the current one, which iroh uses,
and the previous one, which third-party crates (openmls, frost-ed25519,
authenticode, smithay, ...) still pull in. Tilo's own crates depend directly
on the current one only, so the old generation leaves the lock when its last
upstream migrates instead of gaining new direct users here.

The check reads ``Cargo.lock``: every package without a ``source`` is a local
one, and its ``dependencies`` list is exactly its direct (normal, dev and
build) dependencies at their resolved versions, renames included. Vendored
upstream crates under ``vendor/``, the cargo-hakari package and
workspace-unify (which name crates only to unify their features) are exempt. ``SANCTIONED`` lists the
direct edges whose newest dependent release still requires the old generation;
each needs a reason, and a sanctioned edge that no longer exists fails too, so
the list only shrinks.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[2]

# Crate name -> the version prefix of the current generation.
CURRENT = {
    "ed25519-dalek": "3.",
    "x25519-dalek": "3.",
    "rand": "0.10.",
    "rand_core": "0.10.",
    "rand_chacha": "0.10.",
    "sha2": "0.11.",
    "hmac": "0.13.",
    "chacha20poly1305": "0.11.",
}

# (local package, dependency, version prefix) -> why the old generation is admitted.
SANCTIONED = {
    ("tilo-crypto", "sha2", "0.10."): (
        "authenticode has no release on digest 0.11, so its digest input comes "
        "from the matching sha2 (`authenticode-sha2`)"
    ),
}

HAKARI_PACKAGE = "workspace-hack"
# tools/ci/workspace_unify.py's package: every library the workspace builds, old generation too.
UNIFY_PACKAGE = "workspace-unify"


@dataclass(frozen=True)
class Violation:
    package: str
    dependency: str
    version: str

    def __str__(self) -> str:
        wanted = CURRENT[self.dependency].rstrip(".")
        return (
            f"{self.package} depends directly on {self.dependency} {self.version}; "
            f"use {self.dependency} {wanted} (the generation iroh uses)"
        )


def vendored_packages(root: Path) -> set[str]:
    names = set()
    for manifest in sorted((root / "vendor").glob("**/Cargo.toml")):
        name = tomllib.loads(manifest.read_text(encoding="utf-8")).get("package", {}).get("name")
        if name:
            names.add(name)
    return names


def _sanctioned(package: str, dependency: str, version: str) -> tuple[str, str, str] | None:
    for key in SANCTIONED:
        if key[0] == package and key[1] == dependency and version.startswith(key[2]):
            return key
    return None


def check(root: Path = ROOT) -> tuple[list[Violation], list[tuple[str, str, str]]]:
    """Return the old-generation direct edges and the sanctioned edges no longer present."""
    lock = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))
    packages = lock.get("package", [])
    versions: dict[str, list[str]] = {}
    for package in packages:
        versions.setdefault(package["name"], []).append(package["version"])
    exempt = vendored_packages(root) | {HAKARI_PACKAGE, UNIFY_PACKAGE}

    violations = []
    used = set()
    for package in packages:
        if "source" in package or package["name"] in exempt:
            continue
        for entry in package.get("dependencies", []):
            # An entry names the version only when the lock holds several.
            name, *rest = entry.split()
            if name not in CURRENT:
                continue
            version = rest[0] if rest else versions[name][0]
            if version.startswith(CURRENT[name]):
                continue
            sanctioned = _sanctioned(package["name"], name, version)
            if sanctioned:
                used.add(sanctioned)
            else:
                violations.append(Violation(package["name"], name, version))
    stale = sorted(set(SANCTIONED) - used)
    return violations, stale


def main() -> int:
    violations, stale = check()
    # An edge is the resolved graph's; a stale entry is this file's.
    for violation in violations:
        print(f"Cargo.lock: {violation}")
    for package, dependency, version in stale:
        print(
            f"tools/policy/check_crypto_generation.py: {package} no longer depends on {dependency} "
            f"{version.rstrip('.')}: "
            "remove its SANCTIONED entry in tools/policy/check_crypto_generation.py"
        )
    if violations or stale:
        print("Tilo's own crates use one RustCrypto generation (AGENTS.md, docs/policies.md).",
              file=sys.stderr)
        return 1
    print("Tilo's own crates are on the current RustCrypto generation.", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
