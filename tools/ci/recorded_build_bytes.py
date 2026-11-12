#!/usr/bin/env python3
"""Record what a test job's build writes, per package, from a real target tree.

affected_packages.py bounds a test shard by the bytes its build writes, so a
whole-workspace shard stops filling a runner's store (the mold link fails with
"Disk full?") and each shard fits the class's memory. This tool produces that
record from a target directory that a `cargo test --tests --no-run` build of
the packages left behind:

    cargo test --locked --tests --no-run <the packages to measure>
    python3 tools/ci/recorded_build_bytes.py target > tools/ci/recorded_build_bytes.json

Each artifact in `target/debug/deps` is attributed through its `.d` dep-info
file to the package directory its sources live in, so the record follows the
build rather than file-name conventions. Artifacts a build keeps from earlier
compilations of the same package (a second feature set, an older commit) count
toward it too: a shard is bounded above, never below.

`baseline` is what every test job writes before workspace code: the external
dependency tree, build-script output and fingerprints. `unrecorded` is the
upper quartile of the measured packages, given to a package the record does not
know, the way a new package gets the upper quartile of recorded test seconds.

Measure it the way check.yml builds: that workflow sets
CARGO_PROFILE_DEV_DEBUG=0 and CARGO_PROFILE_TEST_DEBUG=0, so the tree's own
`line-tables-only` profile builds larger artifacts than CI does. A record from a
developer build therefore bounds the store from above, which is the direction a
shard budget may err in.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from ci.workspace_graph import cargo_metadata, package_maps

HASHED = re.compile(r"^(.*)-[0-9a-f]{16}$")
RUST_SOURCE = re.compile(r"[^\s:;]+\.rs\b")


def artifact_stem(entry: str) -> str:
    """The crate or target a deps file belongs to, without its hash or extension."""
    stem = entry.split(".", 1)[0]
    match = HASHED.match(stem)
    if match:
        stem = match.group(1)
    return stem[3:] if stem.startswith("lib") else stem


def owners(deps: Path, directories: list[str]) -> dict[str, str]:
    """Map each crate/target to the package whose sources its `.d` file names."""
    found: dict[str, str] = {}
    for entry in os.listdir(deps):
        if not entry.endswith(".d"):
            continue
        try:
            text = (deps / entry).read_text(encoding="utf-8", errors="replace")[:8000]
        except OSError:
            continue
        owner = None
        for path in RUST_SOURCE.findall(text):
            path = path.replace("\\", "/")
            for directory in directories:
                if path.startswith(directory + "/") or f"/{directory}/" in path:
                    owner = directory
                    break
            if owner:
                break
        found[artifact_stem(entry[:-2])] = owner or "external"
    return found


LIBRARY_SUFFIXES = (".rlib", ".rmeta", ".so")


def sizes(root: Path) -> tuple[int, dict[str, list[int]]]:
    """Bytes the build wrote outside workspace packages, and per package.

    A package's rlib, rmeta and dylib are what a dependent's build links; its
    test binaries are what a shard that tests it links. The two are recorded
    apart: a shard writes the first for everything it depends on, the second
    only for the packages it tests.
    """
    deps = root / "target/debug/deps"
    by_dir = package_maps(cargo_metadata(root), root)[1]
    names = {str(directory): name for directory, name in by_dir.items() if str(directory)}
    directories = sorted(names, key=len, reverse=True)
    owned = owners(deps, directories)
    shared = 0
    packages: dict[str, list[int]] = {}
    for entry in os.listdir(deps):
        if entry.endswith(".d"):
            # Dep-info records which sources a target read; it is not output.
            continue
        path = deps / entry
        try:
            written = os.lstat(path).st_size
        except OSError:
            continue
        if not path.is_file():
            continue
        owner = owned.get(artifact_stem(entry))
        if owner not in names:
            shared += written
            continue
        parts = packages.setdefault(names[owner], [0, 0])
        parts[0 if entry.endswith(LIBRARY_SUFFIXES) else 1] += written
    for extra in ("target/debug/build", "target/debug/.fingerprint"):
        for directory, _, files in os.walk(root / extra):
            for name in files:
                try:
                    shared += os.lstat(Path(directory) / name).st_size
                except OSError:
                    pass
    return shared, packages


def record(root: Path) -> dict:
    shared, packages = sizes(root)
    if not packages:
        raise SystemExit(f"{root}: no test build found; run cargo test --tests --no-run first")
    tests = sorted(parts[1] for parts in packages.values())
    libraries = sorted(parts[0] for parts in packages.values())
    # Upper quartile: a package without a measurement is assumed heavier than
    # three quarters of the measured ones, for each of its two parts.
    unrecorded = {"libraries": max(1, libraries[(len(libraries) * 3) // 4]),
                  "tests": max(1, tests[(len(tests) * 3) // 4])}
    return {"baseline": shared, "unrecorded": unrecorded,
            "packages": {name: {"libraries": parts[0], "tests": parts[1]}
                         for name, parts in sorted(packages.items())}}


def dumps(record: dict) -> str:
    """One package per line, so a refresh's diff shows which packages moved."""
    lines = ["{",
             f' "baseline": {record["baseline"]},',
             ' "unrecorded": ' + json.dumps(record["unrecorded"], separators=(", ", ": ")) + ",",
             ' "packages": {']
    entries = [f'  {json.dumps(name)}: ' + json.dumps(parts, separators=(", ", ": "))
               for name, parts in record["packages"].items()]
    lines.append(",\n".join(entries))
    lines += [" }", "}"]
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    parser.add_argument("target", type=Path, help="the target directory of the build to record")
    parser.add_argument("--output", type=Path, help="write the record here instead of stdout")
    args = parser.parse_args(argv)
    text = dumps(record(args.target))
    if args.output:
        args.output.write_text(text, encoding="utf-8")
        print(f"{args.output}: {len(json.loads(text)['packages'])} packages", file=sys.stderr)
    else:
        print(text, end="")
    return 0


if __name__ == "__main__":
    sys.exit(main())
