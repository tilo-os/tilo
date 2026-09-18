#!/usr/bin/env python3
"""Inspect the Fedora Windows guest payload without executing guest programs.

Symlinks use guest-root semantics, including absolute targets. This validates
selected installation contracts, not dependency closure or runtime acceptance.
"""

import argparse
from collections import deque
import gzip
import hashlib
import json
from pathlib import Path, PurePosixPath
import stat
import struct
import sys
import zlib


ELF_FILES = {
    "usr/bin/wine32": (32, 3, True),
    "usr/bin/wineserver32": (32, 3, True),
    "usr/bin/wine64": (64, 62, True),
    "usr/bin/wineserver64": (64, 62, True),
    "usr/bin/python3": (64, 62, True),
    "usr/bin/localedef": (64, 62, True),
    "usr/bin/locale": (64, 62, True),
    "usr/sbin/ldconfig": (64, 62, True),
    "usr/lib/libvulkan.so.1": (32, 3, False),
    "usr/lib64/libvulkan.so.1": (64, 62, False),
    "usr/lib/dri/swrast_dri.so": (32, 3, False),
    "usr/lib64/dri/swrast_dri.so": (64, 62, False),
}
DXVK_INTERFACES = ("d3d8", "d3d9", "d3d10core", "d3d11", "dxgi")
PE_FILES = {
    f"usr/share/tilo/dxvk/{arch}/{interface}.dll": bits
    for arch, bits in (("x32", 32), ("x64", 64))
    for interface in DXVK_INTERFACES
}
LOCALE_FILES = (
    "usr/share/i18n/locales/en_US",
    "usr/share/i18n/charmaps/UTF-8.gz",
)


class InvalidGuest(ValueError):
    pass


def guest_file(root, name):
    """Resolve links component by component; never resolve a target on the host."""
    pending = deque(PurePosixPath(name).parts)
    resolved = []
    links = 0
    while pending:
        part = pending.popleft()
        if part == "/":
            resolved.clear()
            continue
        if part in ("", "."):
            continue
        if part == "..":
            if not resolved:
                raise InvalidGuest(f"{name}: path traverses above guest root")
            resolved.pop()
            continue
        if "\\" in part or ":" in part:
            raise InvalidGuest(f"{name}: invalid guest path component")
        path = root.joinpath(*resolved, part)
        try:
            mode = path.lstat().st_mode
        except OSError as error:
            raise InvalidGuest(f"{name}: missing guest path {path.relative_to(root)}") from error
        if stat.S_ISLNK(mode):
            links += 1
            if links > 40:
                raise InvalidGuest(f"{name}: excessive or cyclic symlinks")
            pending.extendleft(reversed(PurePosixPath(path.readlink().as_posix()).parts))
        else:
            if pending and not stat.S_ISDIR(mode):
                raise InvalidGuest(f"{name}: non-directory path component")
            resolved.append(part)
    path = root.joinpath(*resolved)
    if not stat.S_ISREG(path.lstat().st_mode):
        raise InvalidGuest(f"{name}: expected regular file")
    if path.stat().st_size == 0:
        raise InvalidGuest(f"{name}: empty file")
    return path


def elf_info(path, bits, machine, executable):
    with path.open("rb") as stream:
        header = stream.read(64)
    size = 64 if bits == 64 else 52
    kind = 2 if bits == 64 else 1
    if len(header) < size or header[:7] != b"\x7fELF" + bytes((kind, 1, 1)):
        raise InvalidGuest("incorrect ELF class, encoding, or truncated header")
    elf_type, actual_machine, version = struct.unpack_from("<HHI", header, 16)
    if actual_machine != machine or version != 1:
        raise InvalidGuest("incorrect ELF machine or version")
    if struct.unpack_from("<H", header, 52 if bits == 64 else 40)[0] != size:
        raise InvalidGuest("incorrect ELF header size")
    if elf_type not in ((2, 3) if executable else (3,)):
        raise InvalidGuest("incorrect ELF type")
    if executable and not path.stat().st_mode & 0o111:
        raise InvalidGuest("missing executable permission")
    return {"format": "ELF", "bits": bits, "machine": machine, "elf_type": elf_type}


def pe_info(path, bits):
    with path.open("rb") as stream:
        dos = stream.read(64)
        if len(dos) != 64 or dos[:2] != b"MZ":
            raise InvalidGuest("missing DOS/PE header")
        offset = struct.unpack_from("<I", dos, 60)[0]
        if offset < 64 or offset > path.stat().st_size - 26:
            raise InvalidGuest("invalid PE header offset")
        stream.seek(offset)
        coff = stream.read(26)
    if coff[:4] != b"PE\x00\x00":
        raise InvalidGuest("missing PE signature")
    machine, sections = struct.unpack_from("<HH", coff, 4)
    optional_size, flags, magic = struct.unpack_from("<HHH", coff, 20)
    expected_machine, expected_magic, minimum_size = (
        (0x8664, 0x20B, 112) if bits == 64 else (0x14C, 0x10B, 96)
    )
    if machine != expected_machine or magic != expected_magic:
        raise InvalidGuest("incorrect PE machine or optional-header bitness")
    if flags & 0x2002 != 0x2002:
        raise InvalidGuest("PE image is not an executable DLL")
    if not sections or optional_size < minimum_size:
        raise InvalidGuest("invalid PE optional header or section count")
    if offset + 24 + optional_size + sections * 40 > path.stat().st_size:
        raise InvalidGuest("truncated PE section table")
    return {"format": "PE", "bits": bits, "machine": machine}


def validate(root):
    root = Path(root).resolve(strict=True)
    files, errors = [], []
    for name in (*ELF_FILES, *PE_FILES, *LOCALE_FILES):
        try:
            path = guest_file(root, name)
            record = {"path": name, "resolved_path": path.relative_to(root).as_posix(),
                      "size": path.stat().st_size}
            if name in ELF_FILES:
                record.update(elf_info(path, *ELF_FILES[name]))
            elif name in PE_FILES:
                record.update(pe_info(path, PE_FILES[name]))
            elif name.endswith("UTF-8.gz"):
                with gzip.open(path, "rb") as stream:
                    # Bound decompression: this is a small locale source, not arbitrary archive input.
                    data = stream.read(16 * 1024 * 1024 + 1)
                if len(data) > 16 * 1024 * 1024 or b"CHARMAP" not in data or b"UTF-8" not in data:
                    raise InvalidGuest("invalid or oversized UTF-8 charmap")
            elif b"LC_IDENTIFICATION" not in path.read_bytes():
                raise InvalidGuest("invalid en_US locale source")
            with path.open("rb") as stream:
                record["sha256"] = hashlib.file_digest(stream, "sha256").hexdigest()
            files.append(record)
        except (InvalidGuest, OSError, EOFError, ValueError, zlib.error) as error:
            message = str(error)
            errors.append(message if message.startswith(f"{name}:") else f"{name}: {message}")
    if errors:
        raise InvalidGuest("\n".join(errors))
    return {
        "schema_version": 1,
        "validation": "selected-guest-installation-contracts",
        "files": sorted(files, key=lambda item: item["path"]),
        "limitations": [
            "Header checks do not establish ELF or PE dependency closure, package authenticity, or execution correctness.",
            "Locale source presence does not establish recursive locale-data closure or successful locale generation.",
            "DXVK payload presence does not establish Wine alternative selection, GPU compatibility, or rendered output.",
        ],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    args = parser.parse_args()
    try:
        result = validate(args.root)
    except (InvalidGuest, OSError) as error:
        print(f"Windows guest rejected:\n{error}", file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
