#!/usr/bin/python3
"""Stream session output to stdout while retaining a bounded crash log.

Next to the full record (``latest.log``) the recorder keeps the session's
log events on their own (``events.log``): the lines the compositor's
``tracing`` subscriber writes, each starting with its timestamp and level.
The full record is mostly the periodic measurement stream (``paint-stats``
every two seconds, ``FRAME-LIFECYCLE``, ``PUMP-PHASE``, ...), which fills
its bound in a few hours. A launch's timing lines, an Xwayland bridge start
or an error from the morning would be gone by the afternoon; in their own
record, under the same bound, only other events can displace them.
"""

from __future__ import annotations

import os
import re
import selectors
import sys
import tempfile
import time
from pathlib import Path


READ_CHUNK = 64 * 1024
LOG_LIMIT = 4 * 1024 * 1024
PERSIST_INTERVAL = 0.25
# After storage refused a publication (e.g. a full disk), wait this long
# before trying again instead of retrying on every output burst.
PERSIST_RETRY = 5.0
EVENTS_NAME = "events.log"

# A `tracing_subscriber::fmt` record: RFC 3339 UTC timestamp, then the level,
# each possibly wrapped in ANSI colour codes.
_ANSI = rb"(?:\x1b\[[0-9;]*m)*"
EVENT_RECORD = re.compile(
    rb"^" + _ANSI + rb"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z" + _ANSI
    + rb"\s+" + _ANSI + rb"\s*(?:TRACE|DEBUG|INFO|WARN|ERROR)\b"
)


def is_event(line: bytes) -> bool:
    """Whether one output line is a `tracing` record rather than stream output."""
    return EVENT_RECORD.match(line) is not None


class Retained:
    """The last ``limit`` bytes of a stream, and whether they changed since persisted."""

    def __init__(self, limit: int = LOG_LIMIT) -> None:
        self.limit = limit
        self.data = bytearray()

    def extend(self, chunk: bytes) -> None:
        self.data.extend(chunk)
        if len(self.data) > self.limit:
            del self.data[: -self.limit]


class EventFilter:
    """Pick the complete `tracing` lines out of a byte stream split anywhere."""

    def __init__(self, limit: int = LOG_LIMIT) -> None:
        self.limit = limit
        self.partial = bytearray()

    def feed(self, chunk: bytes) -> bytes:
        self.partial.extend(chunk)
        end = self.partial.rfind(b"\n")
        if end < 0:
            # A line longer than the record cannot be kept whole; its head
            # is enough to classify it, so keep a bounded prefix.
            if len(self.partial) > self.limit:
                del self.partial[self.limit :]
            return b""
        complete = bytes(self.partial[: end + 1])
        del self.partial[: end + 1]
        return b"".join(line for line in complete.splitlines(keepends=True) if is_event(line))

    def finish(self) -> bytes:
        rest = bytes(self.partial)
        self.partial.clear()
        if rest and is_event(rest):
            return rest + b"\n"
        return b""


def persist(path: Path, retained: bytearray) -> bool:
    """Publish the record; False when storage refused it (full disk, I/O error).

    The recorder sits between the compositor and its stderr. If it died on a
    failed write, the compositor's next log line would hit a closed pipe, so a
    refused write only skips this publication; the record stays in memory and
    the next attempt carries all of it.
    """
    try:
        _publish(path, retained)
    except OSError:
        return False
    return True


def _publish(path: Path, retained: bytearray) -> None:
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


def mark_ready() -> None:
    """Tell the session wrapper the relay runs, whether storage took the start record or not."""
    ready = os.environ.get("TILO_FLIGHT_RECORD_READY")
    if not ready:
        return
    try:
        Path(ready).touch()
    except OSError:
        pass


def retry_at() -> float:
    """Dirty mark that makes the next attempt come after PERSIST_RETRY, not at once."""
    return time.monotonic() + PERSIST_RETRY - PERSIST_INTERVAL


def relay(chunk: bytes) -> None:
    """Pass output on; a closed or failing stdout never stops the recorder."""
    try:
        sys.stdout.buffer.write(chunk)
        sys.stdout.buffer.flush()
    except OSError:
        pass


def main() -> int:
    if len(sys.argv) not in (2, 5):
        return 2
    path = Path(sys.argv[1])
    events_path = path.with_name(EVENTS_NAME)
    retained = Retained()
    events = Retained()
    event_filter = EventFilter()
    if len(sys.argv) == 5:
        timestamp, pid, shell = sys.argv[2:]
        header = f"tilo-flight-record-v1 start={timestamp} pid={pid} shell={shell}\n".encode()
        retained.extend(header)
        events.extend(header)
        persist(events_path, events.data)
        persist(path, retained.data)
    mark_ready()
    events_dirty = False

    def publish() -> bool:
        """Persist both records; False when storage refused either."""
        nonlocal events_dirty
        if events_dirty and persist(events_path, events.data):
            events_dirty = False
        return persist(path, retained.data) and not events_dirty

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
            dirty_since = None if publish() else retry_at()
            continue
        chunk = os.read(source.fileno(), READ_CHUNK)
        if not chunk:
            break
        relay(chunk)
        retained.extend(chunk)
        if picked := event_filter.feed(chunk):
            events.extend(picked)
            events_dirty = True
        if dirty_since is None:
            dirty_since = time.monotonic()
        elif time.monotonic() - dirty_since >= PERSIST_INTERVAL:
            dirty_since = None if publish() else retry_at()
    if rest := event_filter.finish():
        events.extend(rest)
        events_dirty = True
    if dirty_since is not None or events_dirty:
        publish()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
