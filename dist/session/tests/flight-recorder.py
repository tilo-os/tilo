#!/usr/bin/env python3
"""Exercise the persistent session relay without ending its input stream."""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path


RECORDER = Path(__file__).resolve().parents[1] / "tilo-flight-recorder.py"


def wait_for(path: Path, expected: bytes) -> None:
    # Bounds only a hang: a loaded runner can take seconds to start Python and fsync.
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        if path.exists() and path.read_bytes() == expected:
            return
        time.sleep(0.01)
    actual = path.read_bytes() if path.exists() else None
    raise AssertionError(f"live flight record was {actual!r}, expected {expected!r}")


def main() -> int:
    with tempfile.TemporaryDirectory() as directory:
        log = Path(directory) / "latest.log"
        previous_umask = os.umask(0o022)
        try:
            process = subprocess.Popen(
                [sys.executable, str(RECORDER), str(log), "now", "42", "/usr/bin/tilo-shell"],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
            )
        finally:
            os.umask(previous_umask)
        assert process.stdin is not None
        assert process.stdout is not None

        header = b"tilo-flight-record-v1 start=now pid=42 shell=/usr/bin/tilo-shell\n"
        wait_for(log, header)
        first = b"startup failed before one buffered block\n"
        process.stdin.write(first)
        process.stdin.flush()
        wait_for(log, header + first)
        assert log.stat().st_mode & 0o777 == 0o600
        assert process.poll() is None, "fixture must prove persistence before EOF"

        second = os.urandom(4 * 1024 * 1024 + 257)
        # The relay is read on a thread of its own while the input is written: a pipe holds
        # as little as two pages when its user has many pipes open (fs.pipe-user-pages-soft),
        # so writing a 64 KiB chunk before reading back deadlocks against a relay blocked on
        # its own full output.
        relayed = bytearray()
        reader = threading.Thread(target=lambda: relayed.extend(process.stdout.read()))
        reader.start()
        process.stdin.write(second)
        process.stdin.close()
        reader.join(timeout=60)
        assert not reader.is_alive(), "the relay stopped passing its input on"
        assert process.wait(timeout=10) == 0
        assert relayed == first + second
        assert log.read_bytes() == (header + first + second)[-(4 * 1024 * 1024) :]
        assert log.stat().st_mode & 0o777 == 0o600

    print("flight recorder: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
