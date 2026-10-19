#!/usr/bin/env python3
"""Hold os/kernel/patches.toml to the patches the kernel build applies.

    check-patches.py RECORD PATCH_DIR [--fedora-srpm SRPM_RECORD]

Every patch in PATCH_DIR has exactly one entry, saying where it stands
upstream. With --fedora-srpm (the buildroot's kernel-buildroot-srpm.txt, whose
first line is the SRPM's NAME-VERSION-RELEASE.src), a patch merged in a
mainline release that Fedora's kernel already is fails the build: Fedora
carries it now, so the patch and its entry go.
"""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path


def release(text: str) -> tuple[int, int]:
    """(major, minor) of a mainline tag (v7.3) or a Fedora kernel (kernel-7.3.1-200.fc44.src)."""
    found = re.fullmatch(r"(?:v|kernel-)(\d+)\.(\d+)(?:[.-].*)?", text)
    if found is None:
        raise ValueError(f"not a kernel release: {text}")
    return int(found.group(1)), int(found.group(2))


def problems(record: dict, patches: list[str], fedora: str | None) -> list[str]:
    found = []
    if not isinstance(record.get("fedora_kernel"), str):
        found.append("fedora_kernel, the Fedora kernel the statuses were checked on, is missing")
    entries = record.get("patch", [])
    files = [entry.get("file") for entry in entries]
    for name in sorted(set(patches) - set(files)):
        found.append(f"{name} has no entry: say where it stands upstream")
    for name in sorted(set(files) - set(patches)):
        found.append(f"{name} has an entry but is not in the patch directory")
    for name in sorted({f for f in files if files.count(f) > 1}):
        found.append(f"{name} has more than one entry")
    for entry in entries:
        name = entry.get("file")
        if not entry.get("upstream"):
            found.append(f"{name}: upstream is missing")
        if ("merged" in entry) != ("commit" in entry):
            found.append(f"{name}: a merged patch names both its release and its mainline commit")
            continue
        if "merged" not in entry:
            continue
        if not re.fullmatch(r"[0-9a-f]{40}", str(entry["commit"])):
            found.append(f"{name}: commit is not a full commit id")
        try:
            merged = release(entry["merged"])
        except ValueError as error:
            found.append(f"{name}: {error}")
            continue
        if fedora is not None and release(fedora) >= merged:
            found.append(f"{name} is in mainline {entry['merged']}, and so in {fedora}: "
                         "drop the patch and its entry")
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
        fedora = args.fedora_srpm.read_text().splitlines()[0].strip().removesuffix(".src")
    patches = sorted(path.name for path in args.patch_dir.glob("*.patch"))
    found = problems(record, patches, fedora)
    for problem in found:
        print(f"kernel patches: {problem}", file=sys.stderr)
    if found:
        return 1
    if fedora is not None and fedora != record["fedora_kernel"]:
        print(f"kernel patches: building on {fedora}; their upstream statuses were checked on "
              f"{record['fedora_kernel']}")
    print(f"kernel patches: {len(patches)} recorded")
    return 0


if __name__ == "__main__":
    sys.exit(main())
