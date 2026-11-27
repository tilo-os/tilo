#!/usr/bin/env python3
"""Only tilo-ui binds a control to an action.

A control runs a ``BoundAction`` (``crates/ui/ui-base``): the action's address
and how the control's value becomes the action's arguments. ``tilo-ui`` makes
every one from the ``ActionBinding`` a resource exports, which checks each value
against the action's declared parameters (``crates/ui/ui-core/src/bind.rs``).
A crate that made one itself would bind a control to an address and arguments
of its own making, past that check.

Rust has no visibility between two crates short of ``pub``, so the base's one
constructor, ``BoundAction::from_tilo_ui_binding``, is public and hidden from
the docs; this check refuses every mention of it outside the base that defines
it and the tilo-ui module that calls it, tests included.
"""

from __future__ import annotations

import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCANNED = (Path("crates"), Path("apps"), Path("tools"), Path("sdk"))
CONSTRUCTOR = "from_tilo_ui_binding"
ALLOWED = (
    Path("crates/ui/ui-base/src/widget/action_emitter.rs"),
    Path("crates/ui/ui-core/src/bind.rs"),
)


@dataclass(frozen=True)
class Violation:
    path: str
    line: int

    def __str__(self) -> str:
        return (
            f"{self.path}:{self.line}: builds a BoundAction itself; bind the control "
            "through tilo-ui (an ActionBinding handed to the widget, or tilo_ui::Bind)"
        )


def audit(root: Path = ROOT) -> list[Violation]:
    violations: list[Violation] = []
    for top in SCANNED:
        base = root / top
        if not base.is_dir():
            continue
        for path in sorted(base.rglob("*.rs")):
            relative = path.relative_to(root)
            if relative in ALLOWED or "target" in relative.parts:
                continue
            text = path.read_text(encoding="utf-8", errors="replace")
            if CONSTRUCTOR not in text:
                continue
            for number, line in enumerate(text.splitlines(), start=1):
                if CONSTRUCTOR in line:
                    violations.append(Violation(relative.as_posix(), number))
    return violations


def main() -> int:
    violations = audit()
    if violations:
        print("Control binding contract FAILED", file=sys.stderr)
        for violation in violations:
            print(violation)
        return 1
    print("Control binding contract OK (only tilo-ui binds a control)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
