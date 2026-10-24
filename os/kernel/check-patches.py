#!/usr/bin/env python3
"""Hold os/kernel/patches.toml to the patches the kernel build applies.

    check-patches.py RECORD PATCH_DIR [--fedora-srpm SRPM_RECORD [--carried]]

Every patch in PATCH_DIR has exactly one entry, saying where it stands
upstream. With --fedora-srpm (the buildroot's kernel-buildroot-srpm.txt, whose
first line is the SRPM's NAME-VERSION-RELEASE.src), a patch merged in a
mainline release that Fedora's kernel already is fails the build: Fedora
carries it now, so the patch and its entry go.

A backport a stable release carries says so with `stable`, the first release of
its series that has it (v7.2.11). Fedora's kernel of that series from then on
carries it, and the build leaves the patch out: `--carried` prints those patch
files, one per line. So a buildroot that moves Fedora's kernel onto that
release builds as the one before did, and the patch goes with its mainline
release, as any merged patch does.
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


def stable_release(text: str) -> tuple[int, int, int]:
    """(major, minor, patch) of a stable tag (v7.2.11) or a Fedora kernel (kernel-7.2.11-200.fc44.src)."""
    found = re.fullmatch(r"(?:v|kernel-)(\d+)\.(\d+)\.(\d+)(?:-.*)?", text)
    if found is None:
        raise ValueError(f"not a stable kernel release: {text}")
    return int(found.group(1)), int(found.group(2)), int(found.group(3))


def carried(record: dict, fedora: str) -> list[str]:
    """The patch files whose stable backport Fedora's kernel, of the same series, already has."""
    have = stable_release(fedora)
    return sorted(entry["file"] for entry in record.get("patch", [])
                  if "stable" in entry and have[:2] == stable_release(entry["stable"])[:2]
                  and have >= stable_release(entry["stable"]))


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
        if "stable" in entry:
            if "merged" not in entry:
                found.append(f"{name}: a stable backport names the mainline release it came from")
            else:
                try:
                    if stable_release(entry["stable"])[:2] >= release(entry["merged"]):
                        found.append(f"{name}: its stable release is not before mainline {entry['merged']}")
                except ValueError as error:
                    found.append(f"{name}: {error}")
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
    parser.add_argument("--carried", action="store_true",
                        help="print the patches Fedora's kernel already has as a stable backport")
    args = parser.parse_args()
    record = tomllib.loads(args.record.read_text())
    fedora = None
    if args.fedora_srpm is not None:
        fedora = args.fedora_srpm.read_text().splitlines()[0].strip().removesuffix(".src")
    if args.carried:
        if fedora is None:
            parser.error("--carried needs --fedora-srpm")
        for name in carried(record, fedora):
            print(name)
        return 0
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
    if fedora is not None:
        for name in carried(record, fedora):
            print(f"kernel patches: {fedora} has {name} as a stable backport; leaving it out")
    return 0


if __name__ == "__main__":
    sys.exit(main())
