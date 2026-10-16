#!/usr/bin/env python3
"""Profile CI installer media with a small copy-on-write setup tail.

The source is never opened for writing. The production profile encoder still
writes and reads back its regular-file block; QEMU then writes that exact block
to a resized qcow2 overlay and this helper independently reads it back.
"""

import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile


BLOCK = 64 * 1024
HANDOFF_HEADER = 4096
HANDOFF_FILES = {
    "etc/NetworkManager/system-connections/tilo-setup.nmconnection",
    "etc/tilo/bringup-ssh.pub",
}
HEX = re.compile(r"^[0-9a-fA-F]{2}$")


def run(*args):
    return subprocess.run(args, check=True, text=True, capture_output=True).stdout


def image_info(path):
    return json.loads(run("qemu-img", "info", "--output=json", "-f", "qcow2", str(path)))


def read_tail(path, offset, count=BLOCK):
    output = run("qemu-io", "-r", "-f", "qcow2", "-c",
                 f"read -v {offset} {count}", str(path))
    actual = bytearray()
    next_offset = offset
    for line in output.splitlines():
        if ":  " not in line:
            continue
        address, rest = line.split(":  ", 1)
        if not re.fullmatch(r"[0-9a-fA-F]+", address):
            continue
        words = rest[:3 * 16].split()
        if int(address, 16) != next_offset or len(words) != 16 or not all(HEX.fullmatch(w) for w in words):
            raise ValueError("qcow2 profile readback has malformed hex data")
        actual.extend(int(word, 16) for word in words)
        next_offset += 16
    if len(actual) != count or next_offset != offset + count:
        raise ValueError("qcow2 profile readback is incomplete")
    return bytes(actual)


def handoff_payload(directory):
    """Wi-Fi and public SSH key only; personal identity stays in OOBE."""
    if (directory.is_symlink() or not directory.is_dir()
            or (hasattr(directory, "is_junction") and directory.is_junction())):
        raise ValueError("setup handoff must be a regular directory")
    payload = bytearray()
    files = []
    for path in sorted(directory.rglob("*")):
        if path.is_symlink() or (hasattr(path, "is_junction") and path.is_junction()):
            raise ValueError("setup handoff must not contain redirected paths")
        if path.is_dir():
            continue
        relative = path.relative_to(directory).as_posix()
        if relative not in HANDOFF_FILES or not path.is_file() or path.stat().st_nlink != 1:
            raise ValueError("setup handoff admits only Wi-Fi and an owner SSH public key")
        with path.open("rb") as stream:
            content = stream.read(65537)
        if not content or len(content) > 65536:
            raise ValueError("setup handoff file is empty or oversized")
        payload.extend(relative.encode() + b"\n" + str(len(content)).encode() + b"\n" + content)
        files.append(relative)
    if not files:
        raise ValueError("setup handoff is empty")
    return bytes(payload)


def profile(source, output, encoder, args, handoff=None):
    if not source.is_file() or source.is_symlink() or output.exists() or output.is_symlink():
        raise ValueError("source must be a regular file and output must be new")
    source = source.resolve(strict=True)
    if output.resolve() == source:
        raise ValueError("source and output must differ")
    info = image_info(source)
    size = info.get("virtual-size")
    if (info.get("format") != "qcow2" or "backing-filename" in info
            or type(size) is not int or size < BLOCK or size % 512):
        raise ValueError("source must be a standalone, sector-aligned qcow2")
    if encoder is not None and (not encoder.is_file() or encoder.is_symlink()):
        raise ValueError("profile encoder must be a regular file")
    if encoder is None and (handoff is None or args):
        raise ValueError("handoff-only mode requires a handoff and no profile arguments")
    payload = handoff_payload(handoff) if handoff is not None else b""
    extra = (len(payload) + 511) // 512 * 512 + HANDOFF_HEADER if payload else 0
    try:
        run("qemu-img", "create", "-f", "qcow2", "-F", "qcow2", "-b",
            str(source), str(output))
        run("qemu-img", "resize", "-f", "qcow2", str(output), str(size + BLOCK + extra))
        with tempfile.TemporaryDirectory(prefix="tilo-profile-") as temporary:
            block = Path(temporary) / "profile.bin"
            block.write_bytes(bytes(BLOCK))
            if encoder is not None:
                run(str(encoder), "--target", str(block), *args)
            expected = block.read_bytes()
            if len(expected) != BLOCK:
                raise ValueError("profile encoder changed the reserved block size")
            if payload:
                header = (b"TILO_SETUP_HANDOFF_V1\n" + hashlib.sha256(payload).hexdigest().encode()
                          + b"\n" + str(len(payload)).encode() + b"\n").ljust(HANDOFF_HEADER, b"\0")
                padding = bytes(extra - HANDOFF_HEADER - len(payload))
                expected = padding + payload + header + expected
                block.write_bytes(expected)
            run("qemu-io", "-f", "qcow2", "-c",
                f"write -s {block} {size} {len(expected)}", str(output))
            if read_tail(output, size, len(expected)) != expected:
                raise ValueError("qcow2 profile readback differs from the production encoder")
        if image_info(output).get("virtual-size") != size + BLOCK + extra:
            raise ValueError("profiled qcow2 virtual size differs")
    except Exception:
        output.unlink(missing_ok=True)
        raise


if __name__ == "__main__":
    if len(sys.argv) < 5:
        raise SystemExit("usage: profile-installer-qcow2.py SOURCE OUTPUT ENCODER --hostname ...\n"
                         "   or: profile-installer-qcow2.py SOURCE OUTPUT --handoff-only --setup-handoff DIR")
    args = sys.argv[4:]
    handoff = None
    if "--setup-handoff" in args:
        index = args.index("--setup-handoff")
        if index + 1 >= len(args):
            raise SystemExit("--setup-handoff requires a directory")
        handoff = Path(args[index + 1])
        del args[index:index + 2]
    encoder = None if sys.argv[3] == "--handoff-only" else Path(sys.argv[3])
    profile(Path(sys.argv[1]), Path(sys.argv[2]), encoder, args, handoff)
