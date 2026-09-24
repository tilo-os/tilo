#!/usr/bin/env python3
"""Exercise the persistent session relay without ending its input stream."""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path


RECORDER = Path(__file__).resolve().parents[1] / "tilo-flight-recorder.py"


def wait_for(path: Path, expected: bytes) -> None:
    deadline = time.monotonic() + 2
    while time.monotonic() < deadline:
        if path.exists() and path.read_bytes() == expected:
            return
        time.sleep(0.01)
    actual = path.read_bytes() if path.exists() else None
    raise AssertionError(f"live flight record was {actual!r}, expected {expected!r}")


def main() -> int:
    with tempfile.TemporaryDirectory() as directory:
        log = Path(directory) / "latest.log"
        process = subprocess.Popen(
            [sys.executable, str(RECORDER), str(log)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
        )
        assert process.stdin is not None
        assert process.stdout is not None

        first = b"startup failed before one buffered block\n"
        process.stdin.write(first)
        process.stdin.flush()
        wait_for(log, first)
        assert process.poll() is None, "fixture must prove persistence before EOF"

        second = os.urandom(4 * 1024 * 1024 + 257)
        relayed = bytearray(process.stdout.read1(65536))
        offset = 0
        while offset < len(second):
            written = os.write(process.stdin.fileno(), second[offset : offset + 65536])
            offset += written
            relayed.extend(process.stdout.read1(65536))
        process.stdin.close()
        relayed.extend(process.stdout.read())
        assert process.wait(timeout=10) == 0
        assert relayed == first + second
        assert log.read_bytes() == (first + second)[-(4 * 1024 * 1024) :]

    print("flight recorder: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
