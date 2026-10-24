#!/usr/bin/env python3
"""A window says a transient notice in its bar, never on a card over its content.

An operation's result, something under way or a failure is the window bar's
notice (``ui::window_toolbar::notify``, ``Transient::publish``): one at a
time, in the bar's location slot, its button a bar action while it shows.
The notification card (``ui::widget::toast::ToastCard``) is the shell's: the
desktop's notifications, the lock screen and the shell's overlays paint it,
and no app or window widget does.

This check walks every Rust source under ``apps/``, ``crates/`` and
``examples/`` and rejects code (not comments or strings) that names the card
or paints it, outside the card's own module and its tests and the shell.
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import code_only, line_number  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]
TREES = ("apps", "crates", "examples")

# Where the card may be painted: its own module and tests, and the shell.
ALLOWED = (
    "crates/sdk/ui-core/src/widget/toast.rs",
    "crates/sdk/ui-core/src/widget/toast_tests.rs",
    "crates/sdk/ui-core/tests/toast_test.rs",
    "crates/session/desktop-surface/",
    "crates/session/shell/",
)

# What painting the card reads like.
CARD = re.compile(
    r"\bToastCard\b|\btoast::(?:paint_card_surface|paint_card|card_height)\b"
)


@dataclass(frozen=True)
class Violation:
    path: str
    line: int

    def __str__(self) -> str:
        return (
            f"{self.path}:{self.line}: paints a notification card; say a transient "
            "notice in the window's bar (window_toolbar::notify, Transient::publish)"
        )


def sources(root: Path):
    for tree in TREES:
        base = root / tree
        if not base.is_dir():
            continue
        for path in sorted(base.rglob("*.rs")):
            relative = path.relative_to(root)
            if "target" in relative.parts or any(
                part.startswith(".") for part in relative.parts
            ):
                continue
            yield relative.as_posix(), path


def audit(root: Path = ROOT) -> list[Violation]:
    violations: list[Violation] = []
    for relative, path in sources(root):
        if relative.startswith(ALLOWED):
            continue
        code = code_only(path.read_text(encoding="utf-8", errors="replace"))
        for match in CARD.finditer(code):
            violations.append(Violation(relative, line_number(code, match.start())))
    return violations


def main() -> int:
    violations = audit()
    if violations:
        print("Transient notices FAILED", file=sys.stderr)
        for violation in violations:
            print(f"  {violation}", file=sys.stderr)
        return 1
    print("Transient notices OK (windows say them in their bar; only the shell paints cards)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
