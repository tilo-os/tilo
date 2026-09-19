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
    with temporary.open("wb") as stream:
        stream.write(retained)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)


def main() -> int:
    if len(sys.argv) != 2:
        return 2
    path = Path(sys.argv[1])
    retained = bytearray()
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
