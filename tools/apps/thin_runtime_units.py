#!/usr/bin/env python3
"""The build directory tools/apps/t-release.sh thin-runtime hands to the size checks.

The shared check builds the runtime, its engines and the account probe in the default
collection's thin graph, in a build directory of its own that stays warm between its runs. Its
outputs are that directory's thin-app profile, so it must hold exactly this build's units: what
earlier builds left there (older versions of a crate, apps a size check built) would only make
every size check download more.

Which units a build used is told by cargo itself: it reads the fingerprint of every unit in the
build's graph, fresh or not, before it decides what to rebuild. So `mark` sets the fingerprints'
access times far into the past before the build, and `keep` afterwards removes every unit whose
fingerprint nobody read since, and lists the files left for the checks that receive them.

A check receiving them finds each file written at the time t restored it, one after another in the
order of their paths, so a library can look newer than a unit built on it, and cargo would rebuild
that unit and everything above it. `stamp` gives every listed file one time, now: later than every
source file, which t wrote before the check started, and the same for a unit and its dependencies.

A unit is `.fingerprint/<name>-<hash>`; its outputs carry the same hash: `deps/<lib>name-<hash>.*`
and executables, `build/<name>-<hash>` (build scripts and their output), `incremental/...`. The
runtime and its engines are dynamic libraries, whose file names have no hash: they are kept by name.
"""

from __future__ import annotations

import argparse
import os
import shutil
import sys
import time
from pathlib import Path

# The access time `mark` gives fingerprints: long before anything was built.
MARK_SECS = 1


def fingerprints(profile: Path) -> list[Path]:
    directory = profile / ".fingerprint"
    return sorted(p for p in directory.iterdir() if p.is_dir()) if directory.is_dir() else []


def set_marked(file: Path) -> None:
    os.utime(file, ns=(MARK_SECS * 10**9, file.stat().st_mtime_ns))


def mark(profile: Path) -> int:
    units = fingerprints(profile)
    files = [file for unit in units for file in unit.iterdir()]
    for file in files:
        set_marked(file)
    # Whether reading a file here moves its access time at all: on a filesystem that keeps none
    # (`noatime`), every fresh unit would look unused and be removed.
    if files:
        files[0].read_bytes()
        if files[0].stat().st_atime_ns <= MARK_SECS * 10**9:
            raise SystemExit(f"thin_runtime_units: {profile} keeps no access times, so the units a build "
                             f"uses can't be told from the ones it left")
        set_marked(files[0])
    return len(units)


def read_since_mark(unit: Path) -> bool:
    # A unit whose fingerprint files cargo read (or wrote) has a recent access time on one of them.
    return any(file.stat().st_atime_ns > MARK_SECS * 10**9 for file in unit.iterdir())


def unit_hash(unit: str) -> str:
    return unit.rsplit("-", 1)[1]


def remove(path: Path) -> None:
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    elif path.exists() or path.is_symlink():
        path.unlink()


def keep(profile: Path, top: set[str]) -> list[Path]:
    units = fingerprints(profile)
    used = {unit.name for unit in units if read_since_mark(unit)}
    stale = {unit_hash(unit.name) for unit in units if unit.name not in used}
    # The fingerprint goes first, so a unit half removed is rebuilt rather than trusted.
    for unit in units:
        if unit.name not in used:
            remove(unit)
    for directory in ("build", "incremental"):
        for path in sorted((profile / directory).glob("*")) if (profile / directory).is_dir() else []:
            if "-" in path.name and unit_hash(path.name) in stale:
                remove(path)
    deps = profile / "deps"
    for path in sorted(deps.iterdir()) if deps.is_dir() else []:
        stem = path.name.split(".", 1)[0]
        if "-" in stem and unit_hash(stem) in stale:
            remove(path)
    # Cargo copies a program or dynamic library to the profile directory too; what is there besides
    # the shared parts is what an app's build left.
    for path in sorted(profile.iterdir()):
        if path.name in {".fingerprint", "build", "deps", "incremental", ".cargo-lock"} or path.name in top:
            continue
        remove(path)
    files = []
    for directory, _, names in os.walk(profile):
        files.extend(Path(directory, name) for name in names)
    return sorted(files)


def stamp(root: Path, listing: Path) -> int:
    files = [root / line for line in listing.read_text(encoding="utf-8").splitlines() if line]
    missing = [file for file in files if not file.is_file()]
    for file in missing[:20]:
        print(f"missing {file}", file=sys.stderr)
    if missing:
        raise SystemExit(f"thin_runtime_units: {len(missing)} of the {len(files)} files {listing} lists are not in {root}")
    now = time.time_ns()
    for file in files:
        os.utime(file, ns=(now, now), follow_symlinks=False)
    return len(files)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="mode", required=True)
    marking = sub.add_parser("mark", help="set the fingerprints' access times into the past, before the build")
    marking.add_argument("profile", type=Path, help="the profile directory, such as target/thin-app")
    keeping = sub.add_parser("keep", help="remove the units the build did not use; list the files left")
    keeping.add_argument("profile", type=Path)
    keeping.add_argument("--top", action="append", default=[],
                         help="a file of the profile directory itself to keep (a program or library)")
    keeping.add_argument("--list", type=Path, required=True,
                         help="where to write the files left, relative to the profile directory's parent")
    stamping = sub.add_parser("stamp", help="give the files a list names one time, now, after they were restored")
    stamping.add_argument("root", type=Path, help="the directory the list's paths are relative to")
    stamping.add_argument("--list", type=Path, required=True)
    args = parser.parse_args(argv)
    if args.mode == "stamp":
        print(f"stamped {stamp(args.root, args.list)} files in {args.root}")
        return 0
    if args.mode == "mark":
        print(f"marked {mark(args.profile)} units in {args.profile}")
        return 0
    files = keep(args.profile, set(args.top))
    root = args.profile.parent
    args.list.write_text("".join(f"{file.relative_to(root)}\n" for file in files), encoding="utf-8")
    print(f"kept {sum(1 for _ in fingerprints(args.profile))} units, {len(files)} files, "
          f"{sum(file.stat().st_size for file in files) >> 20} MiB in {args.profile}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
