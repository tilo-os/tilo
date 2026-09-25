#!/usr/bin/env python3
"""Profile a CI installer qcow2 with a 64 KiB copy-on-write tail.

The source is never opened for writing. The production profile encoder still
writes and reads back its regular-file block; QEMU then writes that exact block
to a resized qcow2 overlay and this helper independently reads it back.
"""

import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile


BLOCK = 64 * 1024
HEX = re.compile(r"^[0-9a-fA-F]{2}$")


def run(*args):
    return subprocess.run(args, check=True, text=True, capture_output=True).stdout


def image_info(path):
    return json.loads(run("qemu-img", "info", "--output=json", "-f", "qcow2", str(path)))


def read_tail(path, offset):
    output = run("qemu-io", "-r", "-f", "qcow2", "-c",
                 f"read -v {offset} {BLOCK}", str(path))
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
    if len(actual) != BLOCK or next_offset != offset + BLOCK:
        raise ValueError("qcow2 profile readback is incomplete")
    return bytes(actual)


def profile(source, output, encoder, args):
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
    if not encoder.is_file() or encoder.is_symlink():
        raise ValueError("profile encoder must be a regular file")
    try:
        run("qemu-img", "create", "-f", "qcow2", "-F", "qcow2", "-b",
            str(source), str(output))
        run("qemu-img", "resize", "-f", "qcow2", str(output), str(size + BLOCK))
        with tempfile.TemporaryDirectory(prefix="tilo-profile-") as temporary:
            block = Path(temporary) / "profile.bin"
            block.write_bytes(bytes(BLOCK))
            run(str(encoder), "--target", str(block), *args)
            expected = block.read_bytes()
            if len(expected) != BLOCK:
                raise ValueError("profile encoder changed the reserved block size")
            run("qemu-io", "-f", "qcow2", "-c",
                f"write -s {block} {size} {BLOCK}", str(output))
            if read_tail(output, size) != expected:
                raise ValueError("qcow2 profile readback differs from the production encoder")
        if image_info(output).get("virtual-size") != size + BLOCK:
            raise ValueError("profiled qcow2 virtual size differs")
    except Exception:
        output.unlink(missing_ok=True)
        raise


if __name__ == "__main__":
    if len(sys.argv) < 5:
        raise SystemExit("usage: profile-installer-qcow2.py SOURCE OUTPUT ENCODER --hostname ...")
    profile(Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3]), sys.argv[4:])
