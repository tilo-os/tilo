#!/usr/bin/env python3
"""Route app clock reads and clock types through `ui::time` (rule W2).

On Linux and macOS `ui::time`'s types are `std::time`'s own, so this is
behaviour-neutral there; on the web they are `web-time`'s, whose clocks work
where `std::time::Instant::now` panics.

    std::time::{Instant, SystemTime, UNIX_EPOCH, SystemTimeError} -> ui::time::…
    Instant::now()     -> ui::time::now()
    SystemTime::now()  -> ui::time::wall()

usage: app_clock.py APP_DIR...
"""

from __future__ import annotations

import pathlib
import re
import sys

CLOCK_TYPES = {"Duration", "Instant", "SystemTime", "UNIX_EPOCH", "SystemTimeError"}
USE_GROUP = re.compile(r"\buse std::time::\{([^}]*)\};")
QUALIFIED = re.compile(r"\bstd::time::(Instant|SystemTime|UNIX_EPOCH|SystemTimeError)\b")
IMPORTED_NOW = re.compile(r"(?<![:\w])(Instant|SystemTime)::now\(\)")
QUALIFIED_NOW = re.compile(r"\bui::time::(Instant|SystemTime)::now\(\)")


def rewrite_group(match: re.Match[str]) -> str:
    names = [name.strip() for name in match.group(1).split(",") if name.strip()]
    moved = [n for n in names if n.split(" as ")[0].strip() in CLOCK_TYPES]
    kept = [n for n in names if n not in moved]
    parts = []
    if moved:
        parts.append(
            f"use ui::time::{moved[0]};" if len(moved) == 1 else f"use ui::time::{{{', '.join(moved)}}};"
        )
    if kept:
        parts.append(
            f"use std::time::{kept[0]};" if len(kept) == 1 else f"use std::time::{{{', '.join(kept)}}};"
        )
    return "\n".join(parts)


def rewrite(text: str) -> str:
    text = USE_GROUP.sub(rewrite_group, text)
    text = QUALIFIED.sub(lambda m: f"ui::time::{m.group(1)}", text)
    text = QUALIFIED_NOW.sub(
        lambda m: "ui::time::now()" if m.group(1) == "Instant" else "ui::time::wall()", text
    )
    text = IMPORTED_NOW.sub(
        lambda m: "ui::time::now()" if m.group(1) == "Instant" else "ui::time::wall()", text
    )
    return text


def main(argv: list[str]) -> int:
    changed = 0
    for app in map(pathlib.Path, argv):
        for path in sorted(app.rglob("*.rs")):
            if "target" in path.parts:
                continue
            before = path.read_text()
            after = rewrite(before)
            if after != before:
                path.write_text(after)
                changed += 1
    print(f"rewrote {changed} files")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
