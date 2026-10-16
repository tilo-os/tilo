#!/usr/bin/env python3
"""Sort a frozen RPM repository into content-stable layer slots.

    layer-rpm-snapshot.py REPO SLOTS

os/Containerfile.rpms publishes the snapshot as one image layer per slot
directory under SLOTS, rather than one layer holding every package. A registry
and a puller skip a layer whose digest they already have, so a refresh then
re-downloads only the slots whose packages changed, not all 2.3 GB.

A package's slot depends only on its name and whether it is large, never on its
version or on which other packages exist, so an update of one package changes
its slot alone:

  big/NN    an RPM of BIG_BYTES or more, by the SHA-256 of its name, one of
            BIG_SLOTS. A few dozen packages are most of the snapshot's bytes.
  small/NN  every other RPM, likewise, one of SMALL_SLOTS.
  meta      everything else: repodata and the signature, checksum and
            package-set records, which change with every snapshot.

Each slot holds `repo/<file>`, so the slots overlay into the one /repo the
image always had. Every file is written with mode 0644, directories 0755, and
modification time 0: a copy's layer then depends on its bytes only, not on when
a mirror delivered them.
"""

from __future__ import annotations

import hashlib
import os
import shutil
import subprocess
import sys
from pathlib import Path

BIG_BYTES = 8 * 1024 * 1024
BIG_SLOTS = 64
SMALL_SLOTS = 40


def slot_names() -> list[str]:
    """Every slot, in the order os/Containerfile.rpms copies them."""
    return ([f"big/{n:02d}" for n in range(BIG_SLOTS)]
            + [f"small/{n:02d}" for n in range(SMALL_SLOTS)] + ["meta"])


def package_name(rpm: Path) -> str:
    result = subprocess.run(["rpm", "-qp", "--nosignature", "--nodigest", "--qf", "%{NAME}", str(rpm)],
                            check=True, capture_output=True, text=True)
    name = result.stdout.strip()
    if not name:
        raise ValueError(f"{rpm.name}: rpm reports no package name")
    return name


def slot_for(name: str, size: int) -> str:
    bucket = int.from_bytes(hashlib.sha256(name.encode()).digest()[:8], "big")
    if size >= BIG_BYTES:
        return f"big/{bucket % BIG_SLOTS:02d}"
    return f"small/{bucket % SMALL_SLOTS:02d}"


def normalize(path: Path, mode: int) -> None:
    os.chmod(path, mode)
    os.utime(path, (0, 0), follow_symlinks=False)


def layer(repo: Path, slots: Path, name_of=package_name) -> dict[str, list[str]]:
    placed: dict[str, list[str]] = {slot: [] for slot in slot_names()}
    for source in sorted(repo.rglob("*")):
        if source.is_symlink():
            raise ValueError(f"{source}: the snapshot may hold only regular files")
        if source.is_dir():
            continue
        if not source.is_file():
            raise ValueError(f"{source}: the snapshot may hold only regular files")
        relative = source.relative_to(repo)
        if source.suffix == ".rpm" and len(relative.parts) == 1:
            slot = slot_for(name_of(source), source.stat().st_size)
        else:
            slot = "meta"
        target = slots / slot / "repo" / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        normalize(target, 0o644)
        placed[slot].append(str(relative))
    for slot in slot_names():
        (slots / slot / "repo").mkdir(parents=True, exist_ok=True)
    # Directories last, deepest first, so creating a child cannot touch a
    # parent's time after it was set.
    for directory in sorted((p for p in slots.rglob("*") if p.is_dir()), key=lambda p: -len(p.parts)):
        normalize(directory, 0o755)
    normalize(slots, 0o755)
    return placed


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    repo, slots = Path(argv[1]), Path(argv[2])
    if slots.exists() and any(slots.iterdir()):
        print(f"{slots} is not empty", file=sys.stderr)
        return 1
    placed = layer(repo, slots)
    rpms = sum(len(files) for slot, files in placed.items() if slot != "meta")
    if rpms == 0:
        print(f"{repo} holds no RPMs", file=sys.stderr)
        return 1
    used = sum(1 for slot, files in placed.items() if files and slot != "meta")
    print(f"layer-rpm-snapshot: {rpms} RPMs in {used} of {len(placed) - 1} package slots, "
          f"{len(placed['meta'])} metadata files")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
