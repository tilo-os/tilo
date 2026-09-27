#!/usr/bin/env python3
"""Point the path dependencies inside vendor/git at the other vendored forks.

`cargo vendor` keeps each fork's in-repository paths, such as
`../../accesskit`. A directory source ignores them, but an app outside the
workspace patches the forks in by path, or from Tilo's public git repository,
and Cargo then follows them. This rewrites each path to the vendored copy of
that crate. A path to a crate that was not vendored is dropped from a normal
dependency, whose version then resolves from crates.io, and a dev-dependency
on one is removed, because dev-dependencies of a dependency are never built.
The Cargo.toml checksum Cargo verifies for the directory source is refreshed.
"""

import hashlib
import json
import pathlib
import re
import sys
import tomllib

DEPENDENCY_TABLE = re.compile(
    r"^\[(?P<section>(?:target\.[^\]]+\.)?(?P<kind>dependencies|dev-dependencies|build-dependencies))\.(?P<key>[^\]]+)\]$"
)
PATH_LINE = re.compile(r'^path = ".*"$')


def dependency_packages(manifest: dict) -> dict[str, str]:
    """Map each dependency table header to the package it names."""
    tables = {}

    def collect(prefix: str, section: dict) -> None:
        for kind in ("dependencies", "dev-dependencies", "build-dependencies"):
            for key, spec in section.get(kind, {}).items():
                if isinstance(spec, dict) and "path" in spec:
                    tables[f"{prefix}{kind}.{key}"] = spec.get("package", key)

    collect("", manifest)
    for target, section in manifest.get("target", {}).items():
        collect(f"target.{target}.", section)
    return tables


def relink(crate: pathlib.Path, vendored: dict[str, str]) -> bool:
    manifest_path = crate / "Cargo.toml"
    source = manifest_path.read_text()
    tables = dependency_packages(tomllib.loads(source))
    own_name = tomllib.loads(source)["package"]["name"]
    out: list[str] = []
    current: tuple[str, str] | None = None
    dropping = False
    for line in source.splitlines(keepends=True):
        stripped = line.strip()
        if stripped.startswith("["):
            dropping = False
            current = None
            match = DEPENDENCY_TABLE.match(stripped)
            header = None
            if match:
                header = f"{match['section']}.{match['key'].strip(chr(34))}"
            if header in tables:
                current = (match["kind"], tables[header])
                package = current[1]
                if current[0] == "dev-dependencies" and package not in vendored and package != own_name:
                    dropping = True
        if dropping:
            continue
        if current and PATH_LINE.match(stripped):
            package = current[1]
            if package == own_name:
                out.append('path = "."\n')
            elif package in vendored:
                out.append(f'path = "../{vendored[package]}"\n')
            continue
        out.append(line)
    updated = "".join(out)
    if updated == source:
        return False
    manifest_path.write_text(updated)
    checksum_path = crate / ".cargo-checksum.json"
    checksums = json.loads(checksum_path.read_text())
    checksums["files"]["Cargo.toml"] = hashlib.sha256(updated.encode()).hexdigest()
    checksum_path.write_text(json.dumps(checksums, separators=(",", ":")))
    return True


def main() -> None:
    root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "vendor/git")
    crates = sorted(path for path in root.iterdir() if (path / "Cargo.toml").is_file())
    vendored = {
        tomllib.loads((crate / "Cargo.toml").read_text())["package"]["name"]: crate.name
        for crate in crates
    }
    for crate in crates:
        if relink(crate, vendored):
            print(f"relinked {crate}")


if __name__ == "__main__":
    main()
