#!/usr/bin/env python3
"""Copies of Tilo's early-boot helpers without their symbol tables, for the initramfs.

    strip-initramfs-helpers.py --tools ROOT --out DIR BINARY...

Release binaries keep their symbol table so a crash log names each frame's
function (Cargo.toml, `strip = "debuginfo"`). The initramfs copies of
tilo-rollback and tilo-sibling-unlock have no crash log to name frames in: no
crash-log hook, no RUST_BACKTRACE, and a panic there prints its message and
its file:line, which the symbol table does not provide. But they are in the
UKI, which a release reseals every time (116 MB per publication), so their
symbols were downloaded with every release. The rootfs keeps the full
binaries at /usr/bin: the same build, so an early-boot address is still
named against that copy out of band.

The image has no binutils (os/Containerfile keeps them in the throwaway
uki-tools stage), so `strip` runs from ROOT, a tree that has them, through
ROOT's own dynamic loader and libraries. Each copy is checked before it is
used: every section the program loads is byte-identical at the same address,
and the symbol tables are gone. A copy that fails either check fails the
build; an initramfs never carries a binary that differs from the rootfs's in
anything it runs.
"""

from __future__ import annotations

import argparse
import struct
import subprocess
import sys
from pathlib import Path

LOADERS = ("usr/lib64/ld-linux-x86-64.so.2", "usr/lib/ld-linux-aarch64.so.1", "usr/lib64/ld-linux-aarch64.so.1")
SHF_ALLOC = 0x2
SHT_NOBITS = 8
SYMBOL_TABLES = (".symtab", ".strtab")


def sections(data: bytes) -> dict[str, tuple[int, int, int, bytes]]:
    """Each section of an ELF64 little-endian file: name -> (flags, type, address, contents)."""
    if data[:6] != b"\x7fELF\x02\x01":
        raise ValueError("not a little-endian ELF64 file")
    shoff, = struct.unpack_from("<Q", data, 0x28)
    shentsize, shnum, shstrndx = struct.unpack_from("<HHH", data, 0x3A)
    headers = []
    for index in range(shnum):
        name, kind, flags, address, offset, size = struct.unpack_from("<IIQQQQ", data, shoff + index * shentsize)
        headers.append((name, kind, flags, address, offset, size))
    names = headers[shstrndx]
    table = data[names[4]:names[4] + names[5]]
    result = {}
    for name, kind, flags, address, offset, size in headers[1:]:
        label = table[name:table.index(b"\0", name)].decode()
        contents = b"" if kind == SHT_NOBITS else data[offset:offset + size]
        result[label] = (flags, kind, address, contents)
    return result


def segments(data: bytes) -> list[tuple[int, ...]]:
    """Each program header of an ELF64 little-endian file, without its file offset."""
    phoff, = struct.unpack_from("<Q", data, 0x20)
    phentsize, phnum = struct.unpack_from("<HH", data, 0x36)
    result = []
    for index in range(phnum):
        kind, flags, _offset, vaddr, paddr, filesz, memsz, align = struct.unpack_from(
            "<IIQQQQQQ", data, phoff + index * phentsize)
        result.append((kind, flags, vaddr, paddr, filesz, memsz, align))
    return result


def check(original: bytes, stripped: bytes) -> None:
    """Refuse a stripped copy that loads anything other than the original does, or kept its symbols."""
    # e_type, e_machine, e_version and e_entry.
    if original[0x10:0x20] != stripped[0x10:0x20] or segments(original) != segments(stripped):
        raise ValueError("the stripped copy has another type, machine, entry point or segments")
    full, thin = sections(original), sections(stripped)
    for name, (flags, kind, address, contents) in full.items():
        if flags & SHF_ALLOC and thin.get(name) != (flags, kind, address, contents):
            raise ValueError(f"the stripped copy's {name} differs from the original's")
    if any(name in thin for name in SYMBOL_TABLES):
        raise ValueError("the stripped copy still carries a symbol table")
    if not any(name in full for name in SYMBOL_TABLES):
        raise ValueError("the original has no symbol table: nothing to strip, so the build has changed")


def strip_command(tools: Path) -> list[str]:
    """`strip`, run by `tools`'s own loader with its own libraries."""
    loader = next((tools / path for path in LOADERS if (tools / path).exists()), None)
    if loader is None:
        raise SystemExit(f"{tools} has no dynamic loader")
    strip = tools / "usr/bin/strip"
    if not strip.is_file():
        raise SystemExit(f"{tools} has no strip")
    return [str(loader), "--library-path", f"{tools / 'usr/lib64'}:{tools / 'usr/lib'}", str(strip)]


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--tools", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("binaries", type=Path, nargs="+")
    args = parser.parse_args(argv)
    command = strip_command(args.tools)
    args.out.mkdir(parents=True, exist_ok=True)
    for binary in args.binaries:
        copy = args.out / binary.name
        subprocess.run([*command, "--strip-all", "--preserve-dates", "-o", str(copy), str(binary)], check=True)
        original, stripped = binary.read_bytes(), copy.read_bytes()
        try:
            check(original, stripped)
        except ValueError as error:
            raise SystemExit(f"{binary}: {error}") from error
        copy.chmod(binary.stat().st_mode & 0o7777)
        print(f"{binary}: {len(original)} -> {len(stripped)} bytes in the initramfs", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
