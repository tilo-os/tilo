#!/usr/bin/env python3
"""Audit Tilo's public collaboration vocabulary.

Protocol and implementation types may continue to use ``peer``.
Ordinary copy uses Workspace, People and Member. The audit
covers the session modules that own public collaboration copy and stops at each
file's test module so assertion prose may describe implementation details.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from judged import add_staged_argument, judged  # noqa: E402
from rust_source import line_number, production_source  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]

PUBLIC_COPY_FILES = (
    "crates/session/desktop/src/people.rs",
    "crates/session/desktop/src/workspace_settings.rs",
    "crates/session/desktop/src/workspace_settings/people.rs",
    "crates/session/desktop/src/topbar/sharing.rs",
    "crates/desktop/session/src/device_lists.rs",
    "crates/desktop/session/src/people.rs",
    "crates/desktop/session/src/workspace_settings.rs",
    "crates/desktop/sheets/src/workspace_settings/people.rs",
)

FORBIDDEN = re.compile(
    r"\b(?:room activity|this room|another room|new room|no peers connected|"
    r"invite peer|connect to peer)\b",
    re.IGNORECASE,
)
STRING_LITERAL = re.compile(r'"(?:[^"\\]|\\.)*"', re.DOTALL)


def audit(staged: bool = False) -> list[str]:
    """Judge the copy files; with ``staged`` only the ones the commit changes.

    A file the commit does not stage has the verdict the commit that last wrote
    it gave it, and the policy suite and CI judge them all.
    """
    files = judged(ROOT, staged)
    changed = files.changed() if staged else None
    violations: list[str] = []
    for relative in PUBLIC_COPY_FILES:
        if changed is not None and Path(relative) not in changed:
            continue
        text = production_source(files.read(relative))
        for literal in STRING_LITERAL.finditer(text):
            value = literal.group()[1:-1]
            match = FORBIDDEN.search(value)
            if match or value.casefold() == "members":
                phrase = match.group() if match else value
                violations.append(
                    f"{relative}:{line_number(text, literal.start())}: "
                    f"public copy uses {phrase!r}"
                )
    return violations


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    add_staged_argument(parser)
    violations = audit(parser.parse_args(argv).staged)
    if violations:
        print("shell copy contract FAILED", file=sys.stderr)
        for violation in violations:
            print(f"  {violation}", file=sys.stderr)
        return 1
    print("shell copy contract OK (Workspace/People/Member vocabulary)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
