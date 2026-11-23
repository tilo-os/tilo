"""Shared `use`-statement inspection for the repository clock contract checks."""

from __future__ import annotations

import re
from collections.abc import Callable

CLOCK_TYPES = ("Instant", "SystemTime")
CLOCK_PATTERN = "|".join(CLOCK_TYPES)
USE_STATEMENT = re.compile(r"\buse\s+[^;]*;", re.DOTALL)
RENAMED = re.compile(rf"\b({CLOCK_PATTERN})\s+as\b")


def clock_imports(
    text: str,
    from_clock_module: Callable[[str], bool],
    is_glob: Callable[[str], bool] | None = None,
) -> list[tuple[int, str, bool]]:
    """The clock types a file's `use` statements bring into scope, and whether from the clock module.

    ``(offset, type, from the clock module)`` per statement. A renamed import
    (`Instant as Clock`) binds another name, so it neither brings the bare name
    into scope from the clock module nor shadows an earlier import of it; when
    `is_glob` matches a clock-module statement, it brings in both clock types.
    """
    found: list[tuple[int, str, bool]] = []
    for statement in USE_STATEMENT.finditer(text):
        body = statement.group()
        from_clock = bool(from_clock_module(body))
        renamed = set(RENAMED.findall(body))
        glob = from_clock and is_glob is not None and bool(is_glob(body))
        for name in CLOCK_TYPES:
            if name not in renamed and (glob or re.search(rf"\b{name}\b", body)):
                found.append((statement.start(), name, from_clock))
    return found
