#!/usr/bin/env python3
"""Prepare exact packaged kernel inputs for boot-smoke.sh."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import re
import resource
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile


MAX_KERNEL_BYTES = 256 * 1024 * 1024
MAX_PACKAGED_MODULE_BYTES = 64 * 1024 * 1024
MAX_MODULE_BYTES = 128 * 1024 * 1024
TRAILER = b"~Module signature appended~\n"


def read_regular_member(
    archive: tarfile.TarFile, members: list[tarfile.TarInfo], name: str, limit: int
) -> bytes:
    matches = [member for member in members if member.name == name]
    if len(matches) != 1:
        raise ValueError(f"expected exactly one archive member {name!r}, found {len(matches)}")
    member = matches[0]
    if not member.isfile():
        raise ValueError(f"not a regular file: {member.name}")
    if member.size < 0 or member.size > limit:
        raise ValueError(f"archive member exceeds {limit} bytes: {member.name}")
    stream = archive.extractfile(member)
    if stream is None:
        raise ValueError(f"cannot read archive member: {member.name}")
    data = stream.read(limit + 1)
    if len(data) != member.size or len(data) > limit:
        raise ValueError(f"invalid or oversized archive member: {member.name}")
    return data


def decompress_zstd(data: bytes) -> bytes:
    zstd = shutil.which("zstd")
    if zstd is None:
        raise ValueError("zstd utility is required for compressed packaged modules")

    def limit_output() -> None:
        resource.setrlimit(resource.RLIMIT_FSIZE, (MAX_MODULE_BYTES, MAX_MODULE_BYTES))

    with tempfile.TemporaryDirectory(prefix="tilo-module-zstd-") as directory:
        compressed = Path(directory) / "module.ko.zst"
        decompressed = Path(directory) / "module.ko"
        compressed.write_bytes(data)
        with decompressed.open("wb") as output:
            result = subprocess.run(
                [zstd, "--decompress", "--stdout", "--quiet", str(compressed)],
                stdout=output,
                stderr=subprocess.PIPE,
                preexec_fn=limit_output,
                timeout=30,
            )
        if result.returncode != 0:
            detail = result.stderr.decode(errors="replace").strip()
            raise ValueError(f"zstd decompression failed: {detail or result.returncode}")
        if decompressed.stat().st_size > MAX_MODULE_BYTES:
            raise ValueError(f"decompressed module exceeds {MAX_MODULE_BYTES} bytes")
        return decompressed.read_bytes()


def module_variants(data: bytes) -> tuple[bytes, bytes]:
    if not data.endswith(TRAILER):
        raise ValueError("dummy.ko has no signature trailer")
    header = len(data) - len(TRAILER) - 12
    if header < 12:
        raise ValueError("invalid module signature header")
    length = struct.unpack_from(">I", data, header + 8)[0]
    if not 0 < length < header - 64:
        raise ValueError("invalid module signature length")
    unsigned = data[: header - length]
    if unsigned[:4] != b"\x7fELF":
        raise ValueError("dummy.ko is not ELF")
    tampered = bytearray(data)
    tampered[len(unsigned) - 1] ^= 1
    return unsigned, bytes(tampered)


def prepare(stage: Path, release: str, output: Path) -> None:
    if not re.fullmatch(r"[a-zA-Z0-9._+-]+", release):
        raise ValueError("invalid kernel release")
    prefix = f"./lib/modules/{release}/"
    uncompressed_name = prefix + "kernel/drivers/net/dummy.ko"
    compressed_name = uncompressed_name + ".zst"
    with tarfile.open(stage) as archive:
        members = archive.getmembers()
        kernel = read_regular_member(
            archive, members, prefix + "vmlinuz", MAX_KERNEL_BYTES
        )
        module_matches = [
            member
            for member in members
            if member.name in (uncompressed_name, compressed_name)
        ]
        if len(module_matches) != 1:
            raise ValueError(
                "expected exactly one packaged dummy module "
                f"({uncompressed_name!r} or {compressed_name!r}), found {len(module_matches)}"
            )
        selected = module_matches[0].name
        packaged = read_regular_member(
            archive, members, selected, MAX_PACKAGED_MODULE_BYTES
        )
    signed = decompress_zstd(packaged) if selected == compressed_name else packaged
    if len(signed) > MAX_MODULE_BYTES:
        raise ValueError(f"module exceeds {MAX_MODULE_BYTES} bytes")
    unsigned, tampered = module_variants(signed)

    (output / "Image").write_bytes(kernel)
    rootfs = output / "rootfs"
    (rootfs / "signed.ko").write_bytes(signed)
    (rootfs / "unsigned.ko").write_bytes(unsigned)
    (rootfs / "tampered.ko").write_bytes(tampered)
    with stage.open("rb") as stage_stream:
        evidence = {
            "release": release,
            "stageSha256": hashlib.file_digest(stage_stream, "sha256").hexdigest(),
            "kernelSha256": hashlib.sha256(kernel).hexdigest(),
            "signedModuleSha256": hashlib.sha256(signed).hexdigest(),
        }
    (output / "inputs.json").write_text(json.dumps(evidence, indent=2) + "\n")


def main() -> int:
    if len(sys.argv) != 4:
        print(
            "usage: prepare-boot-smoke.py STAGE.tar.gz KERNEL_RELEASE OUTPUT_DIRECTORY",
            file=sys.stderr,
        )
        return 2
    try:
        prepare(Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3]))
    except (
        OSError,
        subprocess.SubprocessError,
        tarfile.TarError,
        ValueError,
        struct.error,
    ) as error:
        print(f"boot-smoke input error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
