#!/usr/bin/env python3
"""Fail-closed contract for the temporary supervised installer bootc."""

import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import struct


SOURCE_TAG = "v1.16.10"
SOURCE_COMMIT = "3e76c16556c55e6d15d31bd47602b231e2131cb2"
PATCH_SHA256 = "b8bd058f6a10010421d7847cd8ad36f91b224d7f8758b986ca8abab6c0fc2a3e"
SOURCE_CARGO_LOCK_SHA256 = "2f5ac1f48ea43b8fd6d1f5cfa031a85c3c4ed812ea7ccbae71b8ffe3683eebaf"
RUST_TOOLCHAIN = "1.97.1"
BUILD_IMAGE = ("code.sandbox.example/qos/tilo-builder@sha256:"
               "568fca1f8b4a4b1700c871cca033d2d291a5eb02c4b3be9681410f456b5a7bc9")
BOOTC_VERSION = "1.16.10"
BUILD_COMMAND = "cargo build --locked --release -p bootc"
PURPOSE = "tilo-installer-bootc-helper"
RECEIPT_KEYS = {
    "schema", "purpose", "architecture", "bootc_version", "source_tag",
    "source_commit", "patch_sha256", "binary_sha256", "rustc_version",
    "cargo_version", "build_target", "build_command", "binary_build_id",
    "source_cargo_lock_sha256", "rust_toolchain", "build_image",
}
MACHINES = {"x86_64": 62, "aarch64": 183}
TARGETS = {
    "x86_64": "x86_64-unknown-linux-gnu",
    "aarch64": "aarch64-unknown-linux-gnu",
}


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _protected_regular(path):
    info = path.lstat()
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_gid != 0
            or stat.S_IMODE(info.st_mode) & 0o022 or not info.st_mode & stat.S_IXUSR):
        raise ValueError("installer bootc is not a protected root-owned executable")


def elf_architecture(path):
    with path.open("rb") as stream:
        header = stream.read(20)
    if (len(header) != 20 or header[:4] != b"\x7fELF" or header[4] != 2
            or header[5] != 1):
        raise ValueError("installer bootc is not a little-endian ELF64 binary")
    machine = int.from_bytes(header[18:20], "little")
    for architecture, expected in MACHINES.items():
        if machine == expected:
            return architecture
    raise ValueError("installer bootc has an unsupported ELF architecture")


def elf_build_id(path):
    data = path.read_bytes()
    if len(data) < 64 or data[:4] != b"\x7fELF" or data[4:6] != b"\x02\x01":
        raise ValueError("installer bootc is not a little-endian ELF64 binary")
    phoff = struct.unpack_from("<Q", data, 32)[0]
    phentsize, phnum = struct.unpack_from("<HH", data, 54)
    if phentsize < 56 or phnum > 256 or phoff + phentsize * phnum > len(data):
        raise ValueError("installer bootc ELF program headers are invalid")
    for index in range(phnum):
        offset = phoff + index * phentsize
        if struct.unpack_from("<I", data, offset)[0] != 4:
            continue
        note_offset = struct.unpack_from("<Q", data, offset + 8)[0]
        note_size = struct.unpack_from("<Q", data, offset + 32)[0]
        end = note_offset + note_size
        if end > len(data):
            raise ValueError("installer bootc ELF note is out of bounds")
        while note_offset + 12 <= end:
            namesz, descsz, kind = struct.unpack_from("<III", data, note_offset)
            note_offset += 12
            name = data[note_offset:note_offset + namesz]
            note_offset += (namesz + 3) & ~3
            desc = data[note_offset:note_offset + descsz]
            note_offset += (descsz + 3) & ~3
            if note_offset > end:
                raise ValueError("installer bootc ELF note is truncated")
            if kind == 3 and name.rstrip(b"\0") == b"GNU" and desc:
                return desc.hex()
    raise ValueError("installer bootc lacks a GNU build ID")


def binary_identity(path, architecture):
    path = Path(path)
    _protected_regular(path)
    if elf_architecture(path) != architecture:
        raise ValueError("installer bootc architecture differs")
    return {"architecture": architecture, "sha256": sha256_file(path),
            "build_id": elf_build_id(path)}


def verify_artifact(directory, expected_sha256, architecture):
    if architecture not in MACHINES or not re.fullmatch(r"[0-9a-f]{64}", expected_sha256):
        raise ValueError("invalid installer bootc expectation")
    directory = Path(directory)
    binary, receipt_path = directory / "bootc", directory / "receipt.json"
    identity = binary_identity(binary, architecture)
    actual_sha256 = identity["sha256"]
    if actual_sha256 != expected_sha256:
        raise ValueError("installer bootc digest differs")
    try:
        receipt = json.loads(receipt_path.read_bytes())
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("installer bootc receipt is invalid") from error
    if not isinstance(receipt, dict) or set(receipt) != RECEIPT_KEYS:
        raise ValueError("installer bootc receipt has unexpected fields")
    expected = {
        "schema": 1, "purpose": PURPOSE, "architecture": architecture,
        "bootc_version": BOOTC_VERSION, "source_tag": SOURCE_TAG,
        "source_commit": SOURCE_COMMIT, "patch_sha256": PATCH_SHA256,
        "source_cargo_lock_sha256": SOURCE_CARGO_LOCK_SHA256,
        "rust_toolchain": RUST_TOOLCHAIN, "build_image": BUILD_IMAGE,
        "binary_sha256": actual_sha256, "build_target": TARGETS[architecture],
        "build_command": BUILD_COMMAND, "binary_build_id": identity["build_id"],
    }
    if any(receipt.get(key) != value for key, value in expected.items()):
        raise ValueError("installer bootc receipt differs from the pinned contract")
    for key in ("rustc_version", "cargo_version"):
        if not isinstance(receipt[key], str) or not re.fullmatch(r"[^\r\n]{3,160}", receipt[key]):
            raise ValueError("installer bootc toolchain receipt is invalid")
    return binary, dict(receipt)


def verify_bound_binary(path, expected_sha256, architecture, version=BOOTC_VERSION):
    path = Path(path)
    identity = binary_identity(path, architecture)
    if identity["sha256"] != expected_sha256:
        raise ValueError("bound installer bootc identity differs")
    result = subprocess.run(
        [str(path), "--version"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        timeout=10, env={"PATH": "/usr/bin:/bin", "HOME": "/root", "LANG": "C.UTF-8"})
    if result.returncode or result.stdout.strip() != f"bootc {version}".encode():
        raise ValueError("bound installer bootc version differs")
    return identity


def mountinfo_receipt(path, mountinfo=Path("/proc/self/mountinfo")):
    encoded = os.fsencode(str(path))
    matches = []
    for raw in mountinfo.read_bytes().splitlines():
        fields = raw.split(b" ")
        if len(fields) >= 10 and fields[4] == encoded and b" - " in raw:
            matches.append((raw, fields))
    if len(matches) != 1:
        raise ValueError("installer bootc mount identity is ambiguous")
    raw, fields = matches[0]
    separator = fields.index(b"-")
    options = fields[5].split(b",")
    super_options = fields[separator + 3].split(b",")
    if b"ro" not in options and b"ro" not in super_options:
        raise ValueError("installer bootc bind mount is writable")
    return {
        "mount_point": str(path), "read_only": True,
        "mount_id": fields[0].decode("ascii"),
        "parent_mount_id": fields[1].decode("ascii"),
        "major_minor": fields[2].decode("ascii"),
        "filesystem": fields[separator + 1].decode("ascii"),
        "mountinfo_sha256": hashlib.sha256(raw).hexdigest(),
    }


def write_runtime_receipt(path, binary_identity_value, mount):
    document = {"schema": 1, "purpose": "tilo-installer-bootc-runtime",
                "binary": binary_identity_value, "mount": mount}
    encoded = (json.dumps(document, sort_keys=True) + "\n").encode()
    path = Path(path)
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_gid != 0:
        raise ValueError("installer bootc runtime receipt destination is invalid")
    with path.open("wb") as stream:
        stream.write(encoded)
        stream.flush()
        os.fsync(stream.fileno())


def verify_runtime_receipt(path, expected_identity):
    try:
        document = json.loads(Path(path).read_bytes())
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("installer bootc runtime receipt is invalid") from error
    mount_keys = {"mount_point", "read_only", "mount_id", "parent_mount_id",
                  "major_minor", "filesystem", "mountinfo_sha256"}
    if (not isinstance(document, dict)
            or set(document) != {"schema", "purpose", "binary", "mount"}
            or document["schema"] != 1
            or document["purpose"] != "tilo-installer-bootc-runtime"
            or document["binary"] != expected_identity
            or not isinstance(document["mount"], dict)
            or set(document["mount"]) != mount_keys
            or document["mount"]["mount_point"] != "/run/tilo-installer-bootc"
            or document["mount"]["read_only"] is not True
            or not re.fullmatch(r"[0-9a-f]{64}", document["mount"]["mountinfo_sha256"])):
        raise ValueError("installer bootc runtime receipt differs")
    return document
