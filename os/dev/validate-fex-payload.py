#!/usr/bin/env python3
"""Validate the pinned Tilo FEX installation layout, without executing payloads.

This checks packaging, not functional acceptance or dependency/license completeness.
The pinned upstream builds no i386 Vulkan guest thunk; passing does not imply it.
"""

import argparse
import hashlib
import json
from pathlib import Path
import stat
import struct
import sys


COMMON = ("GL", "EGL", "wayland-client", "cuda")
GROUPS = {
    "usr/lib64/fex-emu/HostThunks": (2, 183, COMMON + ("vulkan", "asound", "drm"), "host"),
    "usr/lib64/fex-emu/HostThunks_32": (2, 183, COMMON + ("vulkan",), "host"),
    "usr/share/fex-emu/GuestThunks": (2, 62, COMMON + ("vulkan", "asound", "drm", "VDSO"), "guest"),
    "usr/share/fex-emu/GuestThunks_32": (1, 3, COMMON + ("VDSO",), "guest"),
}
MACHINES = {183: "aarch64", 62: "x86_64", 3: "i386"}


class InvalidPayload(ValueError):
    pass


def regular_file(root, relative):
    path = root / relative
    try:
        # A staging payload must be self-contained; never hash outside its root.
        path.resolve(strict=True).relative_to(root)
        if not stat.S_ISREG(path.stat().st_mode):
            raise ValueError("not a regular file")
    except (OSError, ValueError, RuntimeError) as error:
        raise InvalidPayload(f"{relative}: missing, invalid, or escaping file") from error
    return path


def elf_header(path, relative):
    with path.open("rb") as stream:
        header = stream.read(64)
    if len(header) < 52 or header[:4] != b"\x7fELF" or header[4] not in (1, 2):
        raise InvalidPayload(f"{relative}: invalid ELF header")
    if header[5:7] != b"\x01\x01":
        raise InvalidPayload(f"{relative}: expected little-endian ELF version 1")
    elf_class = header[4]
    expected_size = 64 if elf_class == 2 else 52
    if len(header) < expected_size:
        raise InvalidPayload(f"{relative}: truncated ELF header")
    elf_type, machine, version = struct.unpack_from("<HHI", header, 16)
    size_offset = 52 if elf_class == 2 else 40
    if version != 1 or struct.unpack_from("<H", header, size_offset)[0] != expected_size:
        raise InvalidPayload(f"{relative}: malformed ELF header")
    return elf_class, machine, elf_type


def validate(root):
    root = Path(root).resolve(strict=True)
    inventory = {}

    def record(relative, expected=None, executable=False):
        path = regular_file(root, relative)
        info = {"path": relative, "size": path.stat().st_size}
        if not info["size"]:
            raise InvalidPayload(f"{relative}: empty file")
        if expected:
            elf_class, machine, elf_type = elf_header(path, relative)
            if (elf_class, machine) != expected or elf_type not in ((2, 3) if executable else (3,)):
                raise InvalidPayload(f"{relative}: wrong ELF architecture or type")
            if executable and not path.stat().st_mode & 0o111:
                raise InvalidPayload(f"{relative}: missing executable permission")
            info.update(elf_class=32 if elf_class == 1 else 64, elf_machine=machine,
                        architecture=MACHINES.get(machine, "unknown"), elf_type=elf_type)
        with path.open("rb") as stream:
            info["sha256"] = hashlib.file_digest(stream, "sha256").hexdigest()
        if path.is_symlink():
            info["symlink_target"] = path.readlink().as_posix()
        inventory[relative] = info

    for name in ("FEX", "FEXServer", "FEXOfflineCompiler"):
        record(f"usr/bin/{name}", (2, 183), executable=True)
    # The image ships no Qt; FEX's Qt configuration GUI must not be built.
    if (root / "usr/bin/FEXConfig").exists() or (root / "usr/bin/FEXConfig").is_symlink():
        raise InvalidPayload("usr/bin/FEXConfig: Qt frontend present; build with BUILD_FEXCONFIG=OFF")
    for directory, (elf_class, machine, families, suffix) in GROUPS.items():
        for family in families:
            record(f"{directory}/lib{family}-{suffix}.so", (elf_class, machine))
        # Validate optional/test/new thunks too, rather than silently ignoring them.
        for path in sorted((root / directory).glob("*.so*")):
            record(path.relative_to(root).as_posix(), (elf_class, machine))
    metadata = "usr/share/fex-emu/ThunksDB.json"
    record(metadata)
    try:
        database = json.loads(regular_file(root, metadata).read_text())
        if not isinstance(database, dict) or not database:
            raise ValueError("expected nonempty object")
    except (ValueError, UnicodeError) as error:
        raise InvalidPayload(f"{metadata}: invalid thunk database") from error
    license_root = "usr/share/licenses/tilo-fex"
    record(f"{license_root}/LICENSE")
    for directory in ("usr/share/fex-emu", license_root):
        for path in sorted((root / directory).rglob("*")):
            if path.is_symlink():
                regular_file(root, path.relative_to(root).as_posix())
            if path.is_file():
                relative = path.relative_to(root).as_posix()
                if relative not in inventory:
                    record(relative)
    return {
        "schema_version": 1,
        "validation": "installation-layout-only",
        "limitations": [
            "The pinned upstream does not build an i386 Vulkan guest thunk; 32-bit Vulkan thunk support is not established.",
            "ELF layout and license-file presence do not establish dependency closure, license completeness, or runtime acceptance.",
        ],
        "files": [inventory[key] for key in sorted(inventory)],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, help="staged filesystem containing usr/")
    args = parser.parse_args()
    try:
        result = validate(args.root)
    except (InvalidPayload, OSError) as error:
        print(f"FEX payload rejected: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
