#!/usr/bin/python3
"""Stream session output to stdout while retaining a bounded crash log."""

from __future__ import annotations

import os
import selectors
import sys
from pathlib import Path


READ_CHUNK = 64 * 1024
LOG_LIMIT = 4 * 1024 * 1024


def persist(path: Path, retained: bytearray) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    # `os.replace` installs the temporary inode, including its mode.  Do not
    # rely on the caller's umask here: compositor/app stderr may contain paths,
    # account names, document titles, or other session-private diagnostics.
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.fchmod(descriptor, 0o600)
    with os.fdopen(descriptor, "wb") as stream:
        stream.write(retained)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)


def main() -> int:
    if len(sys.argv) not in (2, 5):
        return 2
    path = Path(sys.argv[1])
    retained = bytearray()
    if len(sys.argv) == 5:
        timestamp, pid, shell = sys.argv[2:]
        retained.extend(
            f"tilo-flight-record-v1 start={timestamp} pid={pid} shell={shell}\n".encode()
        )
        persist(path, retained)
    source = sys.stdin.buffer.raw
    selector = selectors.DefaultSelector()
    selector.register(source, selectors.EVENT_READ)
    while selector.select():
        chunk = os.read(source.fileno(), READ_CHUNK)
        if not chunk:
            break
        sys.stdout.buffer.write(chunk)
        sys.stdout.buffer.flush()
        retained.extend(chunk)
        if len(retained) > LOG_LIMIT:
            del retained[:-LOG_LIMIT]
        persist(path, retained)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
