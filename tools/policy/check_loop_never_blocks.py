#!/usr/bin/env python3
"""The session's loop never waits on async work.

A device signature goes through the device's signer, which may answer only
later: a page's key signs through a promise, and a signer may answer through
the very loop that asks. Code of the session (`tilo-session`) and its client
(`tilo-session-client`) therefore never drives a future to its end on the
thread that asks (`pollster::block_on`, `futures::executor::block_on`, a free
`block_on`): the work runs as a task of its own and its asker hears the
answer when it lands (`crate::landing`). A thread of its own may still run a
runtime as its whole body (`runtime.block_on(...)`), which waits on nothing
the loop does.

Test code is not judged: a test is the one caller that may wait.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import code_only, is_test_file, line_number, production_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
CRATES = ("crates/session/session/src", "crates/session/client/src")

# An executor that drives a future on the calling thread, or a free `block_on`
# imported from one; a method on a runtime (`.block_on(`) is a thread's body.
BLOCKING = re.compile(
    r"\bpollster\b|\bfutures(?:_executor\b|::executor\b)|(?<![.\w])block_on\s*\("
)


def audit(root: Path = ROOT) -> list[str]:
    found: list[str] = []
    for crate in CRATES:
        for path in sorted((root / crate).rglob("*.rs")):
            relative = path.relative_to(root)
            if is_test_file(relative):
                continue
            code = production_source(
                code_only(path.read_text(encoding="utf-8", errors="replace"))
            )
            lines = sorted({line_number(code, match.start()) for match in BLOCKING.finditer(code)})
            found.extend(
                f"{relative.as_posix()}:{line}: waits on async work on the asking thread; "
                "run it off the loop (crate::landing) and answer when it lands"
                for line in lines
            )
    return found


def main() -> int:
    found = audit()
    if found:
        print("Loop never blocks FAILED", file=sys.stderr)
        for line in found:
            print(line)
        return 1
    print("Loop never blocks OK (the session's async work answers when it lands)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
