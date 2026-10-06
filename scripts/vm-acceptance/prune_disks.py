#!/usr/bin/env python3
"""Delete old dev disks build-disk.sh wrote, keeping the newest few.

usage: prune_disks.py [--keep N|all] [--protect PATH]... [--dry-run] DIR

Every build-disk.sh run writes a full qcow2 of about 15 GiB; the seeded rsync makes the
transfer a delta, but the copy shares almost no extents with its seed, so each disk costs
its full size. Without pruning, a day of experiments fills the laptop.

Only disks this script can prove build-disk.sh wrote are candidates: tilo-dev-*.qcow2 in
DIR with a <disk>.provenance.json whose built_by names build-disk.sh. Of those, the newest
--keep (by built_at; TILO_DISK_KEEP, default 3) stay, and so does any disk that is
  - named with --protect,
  - referenced from tilo-vm's vms.toml (a VM's image),
  - a backing file of another qcow2 in DIR or of a disk in vms.toml (a VM's copy-on-write
    child, or a hand-made overlay), or
  - open in any process (a running VM).
A pruned disk goes with its .provenance.json and .build.log.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path

BUILT_BY = "scripts/vm-acceptance/build-disk.sh"
DEFAULT_KEEP = 3
SIDECARS = (".provenance.json", ".build.log")


def built_at(disk: Path) -> dt.datetime | None:
    """When build-disk.sh wrote this disk, or None if it did not."""
    try:
        record = json.loads(Path(str(disk) + ".provenance.json").read_text())
    except (OSError, ValueError):
        return None
    if not isinstance(record, dict) or record.get("built_by") != BUILT_BY:
        return None
    try:
        return dt.datetime.fromisoformat(record["built_at"])
    except (KeyError, TypeError, ValueError):
        return dt.datetime.fromtimestamp(disk.stat().st_mtime, dt.UTC)


def vms_toml_paths() -> set[Path]:
    """Every string in vms.toml that names an existing file: images, disks, keys."""
    base = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
    path = base / "tilo-vm" / "vms.toml"
    try:
        doc = tomllib.loads(path.read_text())
    except FileNotFoundError:
        return set()
    found: set[Path] = set()

    def walk(value: object) -> None:
        if isinstance(value, dict):
            for v in value.values():
                walk(v)
        elif isinstance(value, list):
            for v in value:
                walk(v)
        elif isinstance(value, str) and ("/" in value or value.startswith("~")):
            p = Path(value).expanduser()
            if p.is_file():
                found.add(p.resolve())

    walk(doc)
    return found


def backing_files(disk: Path) -> set[Path]:
    """The backing chain below a qcow2 (not the disk itself)."""
    out = subprocess.run(
        ["qemu-img", "info", "-U", "--backing-chain", "--output=json", "--", str(disk)],
        capture_output=True, text=True, check=False,
    )
    if out.returncode != 0:
        return set()
    chain = json.loads(out.stdout)
    found = set()
    for layer in chain if isinstance(chain, list) else [chain]:
        backing = layer.get("full-backing-filename") or layer.get("backing-filename")
        if backing:
            p = Path(backing)
            if not p.is_absolute():
                p = Path(layer["filename"]).parent / p
            found.add(p.resolve())
    return found


def open_files() -> set[Path]:
    """Files any process this user can inspect holds open."""
    found = set()
    for fd_dir in Path("/proc").glob("[0-9]*/fd"):
        try:
            fds = list(fd_dir.iterdir())
        except OSError:
            continue
        for fd in fds:
            try:
                target = os.readlink(fd)
            except OSError:
                continue
            if target.startswith("/"):
                found.add(Path(target))
    return found


def plan(directory: Path, keep: int | None, protect: set[Path]) -> tuple[list[Path], list[Path]]:
    """(disks to delete, disks kept) among build-disk.sh's disks in directory."""
    built = []
    for disk in directory.glob("tilo-dev-*.qcow2"):
        when = built_at(disk)
        if when is not None:
            built.append((when, disk.resolve()))
    built.sort(reverse=True)
    if keep is None:
        return [], [d for _, d in built]

    in_use = {p.resolve() for p in protect} | vms_toml_paths() | open_files()
    for qcow in [*directory.glob("*.qcow2"), *[p for p in in_use if p.suffix == ".qcow2"]]:
        in_use |= backing_files(qcow)

    delete, kept = [], []
    for i, (_, disk) in enumerate(built):
        (kept if i < keep or disk in in_use else delete).append(disk)
    return delete, kept


def parse_keep(value: str) -> int | None:
    if value == "all":
        return None
    n = int(value)
    if n < 0:
        raise argparse.ArgumentTypeError("--keep is a count of disks or 'all'")
    return n


def main() -> int:
    ap = argparse.ArgumentParser(description="Delete old dev disks build-disk.sh wrote.")
    ap.add_argument("dir", type=Path)
    ap.add_argument("--keep", type=parse_keep,
                    default=parse_keep(os.environ.get("TILO_DISK_KEEP") or str(DEFAULT_KEEP)),
                    help=f"disks to keep, newest first, or 'all' (TILO_DISK_KEEP, default {DEFAULT_KEEP})")
    ap.add_argument("--protect", type=Path, action="append", default=[],
                    help="a disk to keep regardless of age (repeatable)")
    ap.add_argument("--dry-run", action="store_true", help="list what would go, delete nothing")
    args = ap.parse_args()

    if args.keep is not None and shutil.which("qemu-img") is None:
        print("prune: qemu-img is missing, so backing files cannot be told apart; nothing pruned",
              file=sys.stderr)
        return 0
    delete, kept = plan(args.dir.expanduser(), args.keep, set(args.protect))
    freed = 0
    for disk in delete:
        freed += disk.stat().st_blocks * 512
        print(f"prune: {'would remove' if args.dry_run else 'removed'} {disk}")
        if not args.dry_run:
            disk.unlink()
            for suffix in SIDECARS:
                Path(str(disk) + suffix).unlink(missing_ok=True)
    verb = "would free" if args.dry_run else "freed"
    print(f"prune: kept {len(kept)} dev disk(s), {verb} {freed / 2**30:.1f} GiB")
    return 0


if __name__ == "__main__":
    sys.exit(main())
