#!/usr/bin/env python3
"""Install a verified upstream DXVK pair as data, never as Wine builtin DLLs."""

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import tarfile
import sys
import shutil

ARCHIVE = "dxvk-2.7.1.tar.gz"
LICENSE = "dxvk-c3dd74b-LICENSE"
EXPECTED = {
    f"dxvk-2.7.1/{arch}/{interface}.dll": (arch, interface, bits)
    for arch, bits in (("x32", 32), ("x64", 64))
    for interface in ("d3d8", "d3d9", "d3d10core", "d3d11", "dxgi")
}


def verified_asset(manifest, assets, name):
    matches = [line.split("\t") for line in manifest.read_text().splitlines()
               if not line.startswith("#") and len(line.split("\t")) == 3
               and line.split("\t")[1] == name]
    if len(matches) != 1:
        raise ValueError(f"expected one asset manifest record: {name}")
    checksum, _, url = matches[0]
    path = assets / name
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"asset is not a regular file: {name}")
    with path.open("rb") as stream:
        actual = hashlib.file_digest(stream, "sha256").hexdigest()
    if actual != checksum:
        raise ValueError(f"asset checksum mismatch: {name}")
    return path, {"name": name, "sha256": actual, "url": url}


def install(root, assets, manifest):
    root, assets, manifest = Path(root).resolve(strict=True), Path(assets), Path(manifest)
    archive, archive_record = verified_asset(manifest, assets, ARCHIVE)
    license_path, license_record = verified_asset(manifest, assets, LICENSE)
    if not license_path.stat().st_size:
        raise ValueError("empty DXVK license")
    members = {}
    with tarfile.open(archive, "r:gz") as bundle:
        for member in bundle:
            if member.isdir() and member.name.rstrip("/") in ("dxvk-2.7.1", "dxvk-2.7.1/x32", "dxvk-2.7.1/x64"):
                continue
            if member.name not in EXPECTED or not member.isfile() or member.name in members:
                raise ValueError(f"unexpected, duplicate, or linked DXVK archive member: {member.name}")
            if not 0 < member.size <= 64 * 1024 * 1024:
                raise ValueError(f"invalid DXVK member size: {member.name}")
            with bundle.extractfile(member) as stream:
                members[member.name] = stream.read()
        if set(members) != set(EXPECTED):
            raise ValueError("DXVK archive lacks a required architecture/interface")
    spec = importlib.util.spec_from_file_location("guest_pe_validator", Path(__file__).with_name("validate-windows-guest.py"))
    validator = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(validator)
    destination = root / "usr/share/tilo/dxvk"
    licenses = root / "usr/share/licenses/tilo-dxvk"
    for directory in (destination, licenses):
        parent = root
        for part in directory.parent.relative_to(root).parts:
            parent = parent / part
            if parent.is_symlink():
                raise ValueError(f"DXVK destination parent is a symlink: {parent}")
        if directory.exists() or directory.is_symlink():
            raise FileExistsError(f"DXVK destination already exists: {directory}")
    created = []
    try:
        for directory in (destination, licenses):
            directory.parent.mkdir(parents=True, exist_ok=True)
            directory.mkdir()  # Never replace a provider that appeared after preflight.
            created.append(directory)
        inventory = []
        for name in sorted(members):
            arch, interface, bits = EXPECTED[name]
            target = destination / arch / f"{interface}.dll"
            target.parent.mkdir(exist_ok=True)
            target.write_bytes(members[name])
            target.chmod(0o644)
            header = validator.pe_info(target, bits)
            inventory.append({"path": target.relative_to(root).as_posix(), **header,
                              "sha256": hashlib.sha256(members[name]).hexdigest()})
        (licenses / "LICENSE").write_bytes(license_path.read_bytes())
        record = {"version": "2.7.1", "archive": archive_record, "license": license_record,
                  "files": inventory,
                  "activation": "Prefix installation and native DLL overrides required; Wine builtins are unchanged."}
        (destination / "provenance.json").write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
        return record
    except BaseException:
        # Only remove directories this attempt created. Failed validation or
        # writes must not leave a partial provider that blocks a corrected retry.
        for directory in reversed(created):
            shutil.rmtree(directory)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("assets", type=Path)
    parser.add_argument("manifest", type=Path)
    args = parser.parse_args()
    try:
        install(args.root, args.assets, args.manifest)
    except (ValueError, OSError, tarfile.TarError) as error:
        print(f"DXVK import rejected: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
