#!/usr/bin/env python3
"""Ship the system store's env-runtime without the host's own files twice.

    seed-env-runtime.py prune STORE HOSTROOT
    seed-env-runtime.py heal STORE WHOLE HOSTROOT

The OS image preinstalls the env-runtime environment image in its read-only
app store, so the first environment opens offline. It is built from the same
frozen RPM snapshot as the host, so most of its files are byte-identical to
the host's /usr; but the store is copied into the rootfs in its own layer, and
a hard link cannot reach a file in another layer, so every pull carried those
bytes twice (TIL-735).

`prune` runs on a copy of the store in a stage built on the rootfs's packages,
before the COPY that takes the store into the image. For every env-runtime in
STORE it removes each file under usr/ whose bytes equal the host file at the
same path under HOSTROOT, and lists it in the image's metadata as
`host-sources`: `tree-path<TAB>/host/path<TAB>sha256`. package-store reads
such a file when it imports the image for an environment, every byte checked
against the image's signed hash, as for a file in the tree.

`heal` runs at the end of the rootfs, after the steps that still change or
delete host files. Every listed host file must still be the bytes it stood in
for; any that is not gets its file back from WHOLE (the store as built,
unpruned) and leaves the list. It refuses to put back more than HEAL_LIMIT,
which would mean the seed has stopped matching the host it is shipped with.
"""

from __future__ import annotations

import hashlib
import os
import shutil
import stat
import sys
import tomllib
from pathlib import Path

HOST_SOURCES = "host-sources"
HEAL_LIMIT = 32 * 1024 * 1024
CHUNK = 1 << 20


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(CHUNK):
            digest.update(chunk)
    return digest.hexdigest()


def regular(path: Path) -> bool:
    try:
        return stat.S_ISREG(path.lstat().st_mode)
    except FileNotFoundError:
        return False


def runtimes(store: Path) -> list[tuple[Path, Path]]:
    """(tree, metadata directory) of every env-runtime in `store`."""
    found = []
    packages = store / "pkgs"
    for meta in sorted(packages.glob("*.pkg")):
        manifest = tomllib.loads((meta / "manifest.toml").read_text(encoding="utf-8"))
        if manifest.get("package", {}).get("kind") == "env-runtime":
            found.append((packages / meta.name.removesuffix(".pkg"), meta))
    return found


def prune(store: Path, host: Path) -> int:
    total = 0
    for tree, meta in runtimes(store):
        lines, saved = [], 0
        for directory, _, files in sorted(os.walk(tree / "usr")):
            for name in sorted(files):
                path = Path(directory) / name
                relative = path.relative_to(tree).as_posix()
                twin = host / relative
                if not regular(path) or not regular(twin):
                    continue
                if path.stat().st_size != twin.stat().st_size:
                    continue
                digest = sha256(path)
                if sha256(twin) != digest:
                    continue
                saved += path.stat().st_size
                # Only the name goes: a hard-linked twin elsewhere in the
                # tree (TIL-736) keeps its inode, its mode and its bytes.
                path.unlink()
                lines.append(f"{relative}\t/{relative}\t{digest}\n")
        if lines:
            (meta / HOST_SOURCES).write_text("".join(lines), encoding="utf-8")
        print(f"seed-env-runtime: {tree.name}: {len(lines)} files, {saved} bytes taken from the host")
        total += saved
    return total


def heal(store: Path, whole: Path, host: Path) -> int:
    restored = 0
    for tree, meta in runtimes(store):
        listing = meta / HOST_SOURCES
        if not listing.exists():
            continue
        kept, lines = [], listing.read_text(encoding="utf-8").splitlines()
        for line in lines:
            relative, host_path, digest = line.split("\t")
            twin = host / host_path.lstrip("/")
            if regular(twin) and sha256(twin) == digest:
                kept.append(line + "\n")
                continue
            source = whole / "pkgs" / tree.name / relative
            if not regular(source) or sha256(source) != digest:
                raise SystemExit(f"seed-env-runtime: {relative} is neither on the host nor in {whole}")
            target = tree / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
            restored += target.stat().st_size
            print(f"seed-env-runtime: {host_path} changed after the seed; {relative} ships in the tree")
        if len(kept) != len(lines):
            # Rewritten only when something changed: the listing is in the
            # store's layer, and an unchanged one need not be copied up.
            listing.write_text("".join(kept), encoding="utf-8")
    if restored > HEAL_LIMIT:
        raise SystemExit(f"seed-env-runtime: {restored} bytes no longer match the host (limit {HEAL_LIMIT})")
    return restored


def main(argv: list[str]) -> int:
    if len(argv) == 4 and argv[1] == "prune":
        prune(Path(argv[2]), Path(argv[3]))
        return 0
    if len(argv) == 5 and argv[1] == "heal":
        restored = heal(Path(argv[2]), Path(argv[3]), Path(argv[4]))
        print(f"seed-env-runtime: every host source checked; {restored} bytes put back")
        return 0
    print(__doc__.strip().splitlines()[2], file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
