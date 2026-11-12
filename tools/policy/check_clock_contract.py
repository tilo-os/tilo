#!/usr/bin/env python3
"""Keep the trust-bearing crates' clock reads behind ``tilo_clock``.

A certificate, an invitation, an admission, a replay window and a presence
lease all compare a time against one another. Two readings of the clock in one
exchange is how a device once refused a certificate the host had just renewed:
the host stamped and checked it with ``tilo_clock::wall()``, the device with
``SystemTime::now()``, and a certificate that lapsed between the two readings
was refused by the device itself before the host could answer for it.

So the crates that decide what a peer may do — the owner state, the session
client, its sync door, the room host, the action broker, and the wire and join
types they share — read the clock through [`tilo_clock`] only:

* validity and expiry compare against ``tilo_clock::wall()``;
* deadlines and ordering use ``tilo_clock::now()``.

Those two are the reads a test can hold still (``tilo_clock::testing``) and a
host can lend to a module it runs, which is what makes an expiry testable
without sleeping past it and what keeps a native and a browser build on one
clock. ``SystemTime::now()`` and ``Instant::now()`` read the machine directly
and bypass both.

Test sources and ``#[cfg(test)]`` items are not judged, and neither is
``tilo-clock`` itself. Tokio's timer instant is the one clock a file may still
read, and only where the reason below says its deadlines are timers rather
than a decision about trust: tokio's own ``time::pause`` drives those in tests.
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from judged import Judged, add_staged_argument, judged  # noqa: E402
from rust_source import code_only, is_test_file, line_number, production_source  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]

# The crates whose clock reads decide what a peer may do, and the shared types
# they carry those decisions in.
SCOPES = (
    Path("crates/collab/tilo-peer-state"),
    Path("crates/collab/collab-types"),
    Path("crates/collab/workspace-join"),
    Path("crates/collab/tilo-action-broker"),
    Path("crates/session/client"),
    Path("crates/session/session/src/sync"),
    Path("crates/hosting/tilo-room-host"),
)

# A clock read: `Instant::now()`, `std::time::Instant::now()`,
# `web_time::SystemTime::now()`, `tokio::time::Instant::now()`. The path in
# front of the type says which clock it is.
READ = re.compile(
    r"(?P<path>(?:\b(?:[A-Za-z_]\w*)\s*::\s*)*)(?P<clock>Instant|SystemTime)\s*::\s*now\s*\("
)
TOKIO_INSTANT_IMPORT = re.compile(
    r"\buse\s+tokio::time::(?:Instant\b|\{[^}]*\bInstant\b)"
)

TIMER_REASON = (
    "tokio's timer clock: its deadlines are timers tokio::time::pause drives in "
    "tests, not a decision about trust"
)

# The files allowed to read tokio's timer instant, and why. Each one measures
# how long a peer, a task or a replay took before giving up on it; none of them
# admits, expires or signs anything by that reading.
TIMERS = {
    Path(
        "crates/collab/tilo-action-broker/src/collab_app_broker.rs"
    ): TIMER_REASON,
    Path(
        "crates/collab/tilo-action-broker/src/collab_app_broker/action_host.rs"
    ): TIMER_REASON,
    Path(
        "crates/collab/tilo-action-broker/src/collab_app_broker/committed_document.rs"
    ): TIMER_REASON,
    Path(
        "crates/collab/tilo-action-broker/src/collab_app_broker/native_task_execution.rs"
    ): TIMER_REASON,
    Path("crates/hosting/tilo-room-host/src/peer/arriving.rs"): TIMER_REASON,
    Path("crates/hosting/tilo-room-host/src/peer/access_audit.rs"): TIMER_REASON,
    Path("crates/hosting/tilo-room-host/src/peer/delivery.rs"): TIMER_REASON,
    Path("crates/hosting/tilo-room-host/src/peer/session/writer.rs"): TIMER_REASON,
    Path("crates/hosting/tilo-room-host/src/presence_mailbox.rs"): TIMER_REASON,
    Path("crates/hosting/tilo-room-host/src/workspace.rs"): TIMER_REASON,
}

DIRECT = {
    "SystemTime": (
        "reads the machine's wall clock; validity and expiry compare against "
        "tilo_clock::wall(), the reading a test holds and a host lends"
    ),
    "Instant": (
        "reads the machine's monotonic clock; deadlines and ordering use "
        "tilo_clock::now(), the reading a test holds and a host lends"
    ),
}


@dataclass(frozen=True)
class Violation:
    path: str
    line: int
    reason: str

    def __str__(self) -> str:
        location = f"{self.path}:{self.line}" if self.line else self.path
        return f"{location}: {self.reason}"


def is_timer(relative: Path, qualified: str, tokio_imported: bool) -> bool:
    """Whether this read is tokio's timer instant rather than the machine's."""
    if qualified.endswith("tokio::time::"):
        tokio = True
    else:
        tokio = tokio_imported and not qualified
    return tokio


def audit_sources(files: Judged, scope: Path) -> list[Violation]:
    violations: list[Violation] = []
    for relative, source in files.sources(scope, errors="strict"):
        if is_test_file(relative.relative_to(scope)):
            continue
        # Comments and literals blanked too: a message naming a clock is not a
        # read of one.
        text = code_only(production_source(source))
        tokio_imported = TOKIO_INSTANT_IMPORT.search(text) is not None
        for found in READ.finditer(text):
            qualified = re.sub(r"\s+", "", found.group("path"))
            clock = found.group("clock")
            line = line_number(text, found.start())
            if is_timer(relative, qualified, tokio_imported):
                if relative in TIMERS:
                    continue
                violations.append(
                    Violation(
                        str(relative),
                        line,
                        "reads tokio's timer instant where no reason allows it; "
                        "use tilo_clock::now()/tilo_clock::wall(), or record here "
                        "why this deadline is a timer and not a decision about trust",
                    )
                )
                continue
            violations.append(Violation(str(relative), line, DIRECT[clock]))
    return violations


def audit(root: Path = ROOT, staged: bool = False) -> list[Violation]:
    files = judged(root, staged)
    violations: list[Violation] = []
    for scope in SCOPES:
        if (root / scope).is_dir():
            violations += audit_sources(files, scope)
    return violations


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    add_staged_argument(parser)
    violations = audit(staged=parser.parse_args(argv).staged)
    if violations:
        print("Clock contract FAILED", file=sys.stderr)
        for violation in violations:
            print(violation)
        return 1
    print(
        "Clock contract OK (the trust-bearing crates read time through tilo_clock)",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
