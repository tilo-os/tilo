#!/usr/bin/env python3
"""Check a directory of shots for traces of the machine that rendered them.

This is the belt to a pair of braces, and it is worth being exact about
which. The load-bearing checks are in Rust, on the production path:

  shell/src/shell/search/providers/files.rs
      a walk cannot leave the root it was given, symlinks included
  shell/tests/demo_home_confines_the_pickers.rs
      pinning QOS_USER_HOME actually reaches the file chooser's scope
      and the palette's file provider

Those decide whether a frame *can* contain a real path. This script looks
at the artifacts afterwards for the residue that survives as text.

WHAT IT CANNOT SEE
------------------
Rendered glyphs are pixels. A PNG showing `/home/someone/tax-returns` in
a breadcrumb contains no such string, and nothing here will find it. So a
pass means "no real path leaked as text", never "no real path is on
screen" — the Rust tests above are what cover that, and this script does
not stand in for them.

What it does catch is the residue that has actually shown up: a filename
carrying the operator's account, a JSON state dump written beside the
frames, a PNG text chunk, a log the harness left in the output directory.

An empty or unreadable directory is a failure, not a pass. A check that
reports success when it could not look is worse than no check at all.

    python3 scripts/check-shot-privacy.py target/shots
    python3 scripts/check-shot-privacy.py target/shots --also hostname
"""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path

# Read once, so pointing the harness at a demo home does not blind the
# check to the account it is hiding.
REAL_USER = os.environ.get("SUDO_USER") or os.environ.get("USER") or ""
REAL_HOME = os.path.expanduser("~")


def needles(extra: list[str]) -> list[str]:
    """The strings whose presence in an artifact is a leak.

    The real home's immediate children matter as much as the account
    name: `ccsesions` or `gpp-rwx-lab` in a frame names the operator's
    machine just as surely, and a reviewer who only grepped for the
    username would have called those frames clean.
    """
    found = [n for n in (REAL_USER, REAL_HOME) if n and n not in ("/", "root")]
    try:
        for child in sorted(Path(REAL_HOME).iterdir()):
            name = child.name
            # Skip dotfiles and the standard XDG folders: every machine
            # has them, so they identify nobody, and `Documents` would
            # match the demo tree's own furniture.
            if name.startswith(".") or name in {
                "Desktop", "Documents", "Downloads", "Music", "Pictures",
                "Videos", "Templates", "Public",
            }:
                continue
            if len(name) >= 4:
                found.append(name)
    except OSError:
        # Cannot read the real home; the account name and path still apply.
        pass
    return found + [e for e in extra if e]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("directory", help="directory of rendered shots")
    ap.add_argument("--also", action="append", default=[],
                    help="an extra string that must not appear (repeatable)")
    args = ap.parse_args()

    root = Path(args.directory)
    if not root.is_dir():
        print(f"not a directory: {root}", file=sys.stderr)
        return 2

    files = [p for p in root.rglob("*") if p.is_file()]
    if not files:
        # Unobservable is a failure. A harness that wrote nothing and a
        # harness that wrote clean frames must not look alike here.
        print(f"no files under {root} — nothing was checked", file=sys.stderr)
        return 2

    wanted = needles(args.also)
    if not wanted:
        print("could not determine the operator's account or home; "
              "refusing to report a pass", file=sys.stderr)
        return 2

    hits: list[str] = []
    for path in files:
        rel = path.relative_to(root)
        for needle in wanted:
            if needle in str(rel):
                hits.append(f"{rel}: filename contains {needle!r}")
        try:
            blob = path.read_bytes()
        except OSError as e:
            print(f"could not read {rel}: {e}", file=sys.stderr)
            return 2
        for needle in wanted:
            if needle.encode() in blob:
                hits.append(f"{rel}: contents contain {needle!r}")

    if hits:
        print(f"{len(hits)} trace(s) of this machine in {root}:")
        for h in hits:
            print(f"  {h}")
        return 1

    print(f"checked {len(files)} file(s) under {root} against "
          f"{len(wanted)} marker(s): no real path or account name as text.")
    print("Rendered glyphs are pixels and are out of scope here — the Rust "
          "tests named in this file's docstring are what bound them.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
