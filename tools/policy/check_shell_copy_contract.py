#!/usr/bin/env python3
"""Audit Tilo's public collaboration vocabulary.

Protocol and implementation types may continue to use ``room`` and ``peer``.
Ordinary shell copy uses Workspace, People, Collaborator and Participant. This
The audit covers the shell modules that own public collaboration copy and
stops at each file's test module so assertion prose may describe implementation
details.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]

PUBLIC_COPY_FILES = (
    "crates/session/shell/src/compositor/native.rs",
    "crates/session/desktop/src/collaborators.rs",
    "crates/session/desktop/src/workspace_settings.rs",
    "crates/session/desktop/src/workspace_settings/overlay.rs",
    "crates/session/shell/src/shell/overlay_host/mod.rs",
    "crates/session/shell/src/shell/overview.rs",
    "crates/session/shell/src/shell/peer_sync_overlay.rs",
    "crates/session/shell/src/shell/peer_windows.rs",
    "crates/session/shell/src/shell/room_activity.rs",
    "crates/session/shell/src/shell/search/providers/actions.rs",
    "crates/session/shell/src/shell/search/providers/records.rs",
)

FORBIDDEN = re.compile(
    r"\b(?:room activity|this room|another room|new room|no peers connected|"
    r"invite peer|connect to peer)\b",
    re.IGNORECASE,
)
STRING_LITERAL = re.compile(r'"(?:[^"\\]|\\.)*"', re.DOTALL)


def production_source(text: str) -> str:
    """Discard the conventional trailing Rust test module, when present."""
    markers = ("\n#[cfg(test)]", "\n#[cfg(all(test")
    ends = [text.find(marker) for marker in markers if text.find(marker) >= 0]
    source = text[: min(ends)] if ends else text
    # Full-line comments often quote the legacy words while explaining an
    # internal type. Blank them rather than deleting them so diagnostics keep
    # their original line numbers.
    return re.sub(r"(?m)^\s*//.*$", "", source)


def line_number(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def audit() -> list[str]:
    violations: list[str] = []
    for relative in PUBLIC_COPY_FILES:
        path = ROOT / relative
        text = production_source(path.read_text(encoding="utf-8"))
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


def main() -> int:
    violations = audit()
    if violations:
        print("shell copy contract FAILED", file=sys.stderr)
        for violation in violations:
            print(f"  {violation}", file=sys.stderr)
        return 1
    print("shell copy contract OK (Workspace/People/Collaborator vocabulary)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
