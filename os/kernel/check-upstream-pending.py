#!/usr/bin/env python3
"""Check the upstream-pending kernel patches against their record.

    check-upstream-pending.py RECORD PATCH_DIR [--fedora-srpm SRPM_RECORD]

RECORD is os/kernel/upstream-pending.toml. Every patch it lists must exist in
PATCH_DIR with the recorded sha256 and Message-ID, so a carried patch is the
posted one, unmodified. With --fedora-srpm (the buildroot's
/usr/lib/tilo/kernel-buildroot-srpm.txt, whose first line is the SRPM's
NAME-VERSION-RELEASE.src), the Fedora kernel being built must also be the one
each series was verified against: a Fedora kernel update fails here until the
series is dropped (merged upstream) or re-verified on the new version.
"""

from __future__ import annotations

import argparse
import hashlib
import re
import sys
import tomllib
from pathlib import Path

REQUIRED = ("title", "revision", "cover", "status", "drop_when", "fedora_kernel", "patch")


def problems(record: dict, patch_dir: Path, fedora: str | None) -> list[str]:
    found: list[str] = []
    series_list = record.get("series", [])
    for series in series_list:
        title = series.get("title", "<untitled series>")
        missing = [key for key in REQUIRED if not series.get(key)]
        if missing:
            found.append(f"{title}: missing {', '.join(missing)}")
            continue
        for patch in series["patch"]:
            path = patch_dir / patch["file"]
            if not path.is_file():
                found.append(f"{title}: {patch['file']} is not in {patch_dir}")
                continue
            data = path.read_bytes()
            digest = hashlib.sha256(data).hexdigest()
            if digest != patch["sha256"]:
                found.append(
                    f"{title}: {patch['file']} has sha256 {digest}, the record says {patch['sha256']}; "
                    "a carried patch is the posted one, unmodified"
                )
            header = re.search(rb"^Message-ID: <([^>]+)>$", data, re.MULTILINE)
            if header is None or header.group(1).decode() != patch["message_id"]:
                found.append(f"{title}: {patch['file']} does not carry Message-ID <{patch['message_id']}>")
        if fedora is not None and series["fedora_kernel"] != fedora:
            found.append(
                f"{title} ({series['revision']}, <{series['cover']}>) was verified on "
                f"{series['fedora_kernel']}, but this build is {fedora}.\n"
                f"  Drop it when {series['drop_when']}: remove its patches and its entry in "
                "os/kernel/upstream-pending.toml.\n"
                "  Otherwise check for a newer revision of the series, re-test it on the new kernel, "
                f"and set fedora_kernel = \"{fedora}\"."
            )
    return found


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("record", type=Path)
    parser.add_argument("patch_dir", type=Path)
    parser.add_argument("--fedora-srpm", type=Path)
    args = parser.parse_args()
    record = tomllib.loads(args.record.read_text())
    fedora = None
    if args.fedora_srpm is not None:
        first = args.fedora_srpm.read_text().splitlines()[0].strip()
        fedora = first.removesuffix(".src")
    found = problems(record, args.patch_dir, fedora)
    for problem in found:
        print(f"upstream-pending kernel patches: {problem}", file=sys.stderr)
    if not found:
        count = sum(len(series["patch"]) for series in record.get("series", []))
        suffix = f" on {fedora}" if fedora else ""
        print(f"upstream-pending kernel patches: {count} carried as posted{suffix}")
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main())
