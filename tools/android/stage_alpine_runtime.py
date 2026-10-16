#!/usr/bin/env python3
"""Materialize a private ATL runtime from a pinned, authenticated Alpine export.

Acquisition must use apk's signature verification in an isolated container.
This tool performs no download, package installation or code execution.
"""
import argparse
import json
from pathlib import Path, PurePosixPath
import posixpath
import shutil
import tarfile
import tempfile

from qualify_apk import ARCHES, digest, elf_machine, pinned, runtime_files, safe_relative

PREFIXES = ("lib/", "usr/lib/", "usr/share/", "etc/fonts/")
BINARY = "usr/bin/android-translation-layer"


def archive_name(name):
    if name.startswith("./"):
        name = name[2:]
    name = name.rstrip("/")
    safe_relative(name)
    return name


def resolve_member(name, members):
    visited = set()
    while True:
        if name in visited or len(visited) >= 40:
            raise ValueError("cyclic or excessive runtime archive links")
        visited.add(name)
        member = members.get(name)
        if member is None:
            raise ValueError("runtime link targets a missing archive member")
        if member.isfile() or member.isdir():
            return member
        if member.issym():
            target = member.linkname
            name = (target.lstrip("/") if target.startswith("/") else
                    posixpath.normpath(posixpath.join(posixpath.dirname(name), target)))
        elif member.islnk():
            name = member.linkname.lstrip("/")
        else:
            raise ValueError("runtime contains a special archive member")
        safe_relative(name)


def stage(args):
    pinned(args.rootfs, args.rootfs_sha256)
    pinned(args.entry, args.entry_sha256)
    with args.entry.open("rb") as stream:
        if elf_machine(stream.read(20)) != ARCHES[args.architecture][1]:
            raise ValueError("entrypoint ELF architecture mismatch")
    if args.receipt.resolve().is_relative_to(args.output.resolve()):
        raise ValueError("receipt must remain outside the runtime payload")
    if args.output.exists():
        raise ValueError("runtime output already exists")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".atl-stage-", dir=args.output.parent) as temp:
        root = Path(temp) / "runtime"
        root.mkdir()
        with tarfile.open(args.rootfs) as archive:
            all_members = archive.getmembers()
            if len(all_members) > 100000 or sum(m.size for m in all_members) > 4 * 1024**3:
                raise ValueError("runtime export exceeds staging limits")
            members = {}
            for member in all_members:
                name = archive_name(member.name)
                if name in members:
                    raise ValueError("duplicate runtime archive member")
                members[name] = member
            directory_aliases = []
            for name, original in members.items():
                if name != BINARY and not name.startswith(PREFIXES):
                    continue
                member = resolve_member(name, members)
                target = root.joinpath(*PurePosixPath(name).parts)
                if member.isdir():
                    if original.issym() or original.islnk():
                        directory_aliases.append((target, archive_name(member.name)))
                        continue
                    target.mkdir(parents=True, exist_ok=True)
                    continue
                target.parent.mkdir(parents=True, exist_ok=True)
                source = archive.extractfile(member)
                if source is None:
                    raise ValueError("missing runtime file data")
                with source, target.open("wb") as output:
                    shutil.copyfileobj(source, output)
                target.chmod(0o755 if member.mode & 0o111 else 0o644)
            for target, source_name in directory_aliases:
                source = root.joinpath(*PurePosixPath(source_name).parts)
                if source.is_relative_to(target) or target.is_relative_to(source):
                    raise ValueError("recursive runtime directory alias")
                if not source.is_dir():
                    raise ValueError(f"directory alias source not staged: {source_name}")
                shutil.copytree(source, target)
        binary = root / BINARY
        with binary.open("rb") as stream:
            if elf_machine(stream.read(20)) != ARCHES[args.architecture][1]:
                raise ValueError("ATL ELF architecture mismatch")
        icu_data = list((root / "usr/share/icu").glob("*/icudt*l.dat"))
        if len(icu_data) != 1:
            raise ValueError("runtime requires exactly one little-endian ICU data file")
        private_icu = root / "usr/share/icu/tilo"
        private_icu.mkdir()
        shutil.copyfile(icu_data[0], private_icu / icu_data[0].name)
        (root / "bin").mkdir()
        shutil.copyfile(args.entry, root / "bin/android-translation-layer")
        (root / "bin/android-translation-layer").chmod(0o755)
        (root / "etc/fonts").mkdir(parents=True, exist_ok=True)
        (root / "etc/fonts/tilo-private.conf").write_text(
            '<?xml version="1.0"?>\n<!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">\n'
            '<fontconfig><dir prefix="relative">../../usr/share/fonts</dir>'
            '<cachedir prefix="xdg">fontconfig</cachedir></fontconfig>\n')
        (root / "licenses").mkdir()
        # Preserve the complete supplied acquisition/license/provenance record;
        # it is part of the runtime hash map, never a claim of runtime acceptance.
        shutil.copyfile(args.provenance, root / "licenses/acquisition.txt")
        receipt = {"schema": 1, "architecture": args.architecture,
                   "rootfs_sha256": args.rootfs_sha256, "entry_sha256": args.entry_sha256,
                   "files": runtime_files(root)}
        receipt_path = Path(temp) / "runtime-receipt.json"
        receipt_path.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
        # Keep the receipt outside the payload so its map covers every payload byte.
        if args.receipt.exists():
            raise ValueError("receipt output already exists")
        args.receipt.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(receipt_path, args.receipt)
        root.rename(args.output)
    print(json.dumps({"runtime_root": str(args.output), "receipt": str(args.receipt),
                      "receipt_sha256": digest(args.receipt), "files": len(receipt["files"])}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rootfs", type=Path, required=True)
    parser.add_argument("--rootfs-sha256", required=True)
    parser.add_argument("--entry", type=Path, required=True)
    parser.add_argument("--entry-sha256", required=True)
    parser.add_argument("--architecture", choices=ARCHES, required=True)
    parser.add_argument("--provenance", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--receipt", type=Path, required=True)
    args = parser.parse_args()
    try:
        stage(args)
    except (OSError, ValueError, tarfile.TarError) as error:
        parser.exit(2, f"runtime staging refused: {error}\n")


if __name__ == "__main__":
    main()
