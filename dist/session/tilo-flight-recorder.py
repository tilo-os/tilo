#!/usr/bin/python3
"""Stream session output to stdout while retaining a bounded crash log."""

from __future__ import annotations

import os
import selectors
import sys
import tempfile
import time
from pathlib import Path


READ_CHUNK = 64 * 1024
LOG_LIMIT = 4 * 1024 * 1024
PERSIST_INTERVAL = 0.25


def persist(path: Path, retained: bytearray) -> None:
    # A restarting session can overlap its predecessor. Each publication owns
    # its staging inode; a shared name lets one recorder consume the other's
    # file between writing and replacement.
    descriptor, name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary = Path(name)
    try:
        # Replacement installs this inode's mode. Diagnostics are private
        # regardless of the caller's umask.
        with os.fdopen(descriptor, "wb") as stream:
            os.fchmod(stream.fileno(), 0o600)
            stream.write(retained)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


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
    # Every persist rewrites and fsyncs the whole record. Doing that per read
    # turned a chatty session into a continuous multi-megabyte write stream,
    # and a writer blocked on a full pipe stalls the compositor's stderr.
    # Output is relayed at once; the record follows within PERSIST_INTERVAL,
    # and in full at end of stream.
    dirty_since: float | None = None
    while True:
        timeout = None
        if dirty_since is not None:
            timeout = max(0.0, dirty_since + PERSIST_INTERVAL - time.monotonic())
        if not selector.select(timeout):
            persist(path, retained)
            dirty_since = None
            continue
        chunk = os.read(source.fileno(), READ_CHUNK)
        if not chunk:
            break
        sys.stdout.buffer.write(chunk)
        sys.stdout.buffer.flush()
        retained.extend(chunk)
        if len(retained) > LOG_LIMIT:
            del retained[:-LOG_LIMIT]
        if dirty_since is None:
            dirty_since = time.monotonic()
        elif time.monotonic() - dirty_since >= PERSIST_INTERVAL:
            persist(path, retained)
            dirty_since = None
    if dirty_since is not None:
        persist(path, retained)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
