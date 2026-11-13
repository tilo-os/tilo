#!/usr/bin/env python3
"""The session opens its records through one ``RecordOwners``.

Two ``RecordOwners`` over one store are two record sets. A change written through one reaches the
store, and the other's own document is only refreshed when something tells it to, so a record a
part of the session writes can be read stale by another part — the byline that says Chat's own
word for an agent ("Agent") where the workspace's name ("Assistant") was written a moment before,
because the page's record set never saw the write. Nothing about it is a race a test can lean on:
it depends on which part happens to write first, so the class is refused here.

``crates/session/session/src/owners.rs`` holds the session's one ``RecordOwners``
(``SessionOwners``) and hands its clones to each part. Production code in
``crates/session/session/src`` must not build another: a second construction is a refusal,
wherever it appears. Test modules and test files are exempt (a fixture opens its own store), and
the model of "one store, clones share it" is proved by the owners module's own test.

The session's other crates (``desktop``, ``onboarding-host``) are separate processes with their
own stores and keep their own owners.
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from judged import add_staged_argument, judged  # noqa: E402
from rust_source import is_test_file, line_number, production_source  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]
# The session's own crate: the parts that share one store in one process.
SCANNED = (Path("crates/session/session/src"),)
# The one module allowed to construct the session's owners.
ALLOWED = (Path("crates/session/session/src/owners.rs"),)
# The constructors that make a new record registry. `open`, `open_beside` and the rest take an
# existing one and are not constructions.
CONSTRUCTION = re.compile(r"\bRecordOwners\s*::\s*(?P<name>new|with_store|portable)\s*\(")


@dataclass(frozen=True)
class Violation:
    path: str
    line: int
    name: str

    def __str__(self) -> str:
        return (
            f"{self.path}:{self.line}: RecordOwners::{self.name}(...) opens a second record set "
            "over the session's store; open it through crate::owners::SessionOwners (the "
            "session's one RecordOwners), so a write one part makes the others read at once"
        )


def audit_file(relative: Path, text: str) -> list[Violation]:
    text = production_source(text)
    return [
        Violation(relative.as_posix(), line_number(text, found.start()), found.group("name"))
        for found in CONSTRUCTION.finditer(text)
    ]


def audit(root: Path = ROOT, staged: bool = False) -> list[Violation]:
    violations: list[Violation] = []
    files = judged(root, staged)
    for top in SCANNED:
        for relative, text in files.sources(top, errors="strict"):
            if any(relative.is_relative_to(allowed) for allowed in ALLOWED):
                continue
            if "target" in relative.parts or is_test_file(relative):
                continue
            violations += audit_file(relative, text)
    return violations


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    add_staged_argument(parser)
    violations = audit(staged=parser.parse_args(argv).staged)
    if violations:
        print("Session record owners contract FAILED", file=sys.stderr)
        for violation in violations:
            print(violation)
        return 1
    print(
        "Session record owners contract OK (one RecordOwners per session, in owners.rs)",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
