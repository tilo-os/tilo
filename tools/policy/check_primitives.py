#!/usr/bin/env python3
"""The shell and apps build controls, rows and text from the design system's
primitives, never by hand (docs/design-language.md, "Primitives").

In production Rust under `crates/session/` and `apps/` this refuses:

- hand-rolled: a rounded rect filled within a few lines of a text run, the
  shape of a button, row or chip painted by hand instead of with
  `ui::widget::{ButtonFace, BarControlFace, ListRowFace, ChipFace, ...}`;
- glyph-join: a string literal that joins facts with a separator glyph
  ("·", "•", "|", "—", "–" or " / " between values) instead of laying them
  out as separate components (`ui::widget::Facts`, `TextStack`,
  `KeyHintsFace`);
- glyph-icon: a string literal that uses a unicode symbol as an icon (an
  arrow, a check mark, a cross, a dot, a star) instead of `ui::icons::Icon`;
- raw-colour: a colour literal (`Color::from_rgb8`, `Color::from_rgba8`,
  `Color::new([...])`, `Color::WHITE`/`BLACK`) instead of a theme token.

Code that still does any of this is listed in `primitives_baseline.txt`, one
line per file and rule with how many it may still have. A count only
shrinks: more than listed fails, fewer fails until the line is lowered, so
fixed code cannot come back.
"""

from __future__ import annotations

import re
import sys
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path(__file__).resolve().parent / "primitives_baseline.txt"

SCOPES = ("crates/session", "apps")

# A separator glyph standing between two values.
GLYPH_JOIN = re.compile(r"\S\s+[·•|—–›»]\s+\S|\S\s/\s\S|^\s*[·•]\s|\s[·•]\s*$")
# Unicode symbols used as icons: arrows, check marks, crosses, dots, stars,
# triangles.
GLYPH_ICON = re.compile("[←→↑↓↖↗↘↙↔↕⇐⇒⇑⇓✓✔✗✘✕✖●○◉◎★☆▶◀▲▼▸◂▴▾►◄⬤⏵⏴⏶⏷]")
# Literals no person reads in the interface: logs, panics, errors, assertion
# messages and lint reasons.
NOT_UI = re.compile(
    r"(?:\b(?:info|warn|error|debug|trace|e?println|e?print|panic|unreachable|todo|"
    r"assert\w*|debug_assert\w*|bail|ensure|anyhow|write|writeln|format_err)!"
    r"|\.(?:expect|context|with_context|map_err)\b|\breason\s*=)[^;{}]*$"
)
RAW_COLOUR = re.compile(
    r"\bColor::(?:from_rgb8|from_rgba8|from_rgb|from_rgba|new)\s*\("
    r"|\bColor::(?:WHITE|BLACK)\b"
)
ROUNDED_FILL = re.compile(r"RoundedRect::(?:from_rect|new)")
TEXT_RUN = re.compile(r"\.draw_(?:text|style)\s*\(")
# How close a rounded fill and a text run must be to count as one control.
NEAR_LINES = 12

RULES = ("hand-rolled", "glyph-join", "glyph-icon", "raw-colour")


def sources(root: Path) -> list[Path]:
    found = []
    for scope in SCOPES:
        base = root / scope
        if not base.is_dir():
            continue
        for path in base.rglob("*.rs"):
            relative = path.relative_to(root)
            if ("target" in relative.parts or "src" not in relative.parts
                    or any(part.startswith(".") for part in relative.parts)
                    or rust_source.is_test_file(relative)
                    or "examples" in relative.parts):
                continue
            found.append(path)
    return sorted(found)


def literal_value(value: str) -> str:
    """The literal as a person reads it: placeholders become a value."""
    return re.sub(r"\{[^{}]*\}", "x", value)


def count_file(text: str) -> Counter:
    counts: Counter = Counter()
    code, literals = rust_source.lex(text)
    marked = list(code)
    for literal in literals:
        for index in range(literal.start, literal.end):
            if marked[index] != "\n":
                marked[index] = "x"
    kept = rust_source.blank_test_items("".join(marked))
    for literal in literals:
        if literal.byte or kept[literal.start] != "x":
            continue
        if NOT_UI.search(code[max(0, literal.start - 160):literal.start]):
            continue
        value = literal_value(literal.value)
        if GLYPH_JOIN.search(value):
            counts["glyph-join"] += 1
        if GLYPH_ICON.search(value):
            counts["glyph-icon"] += 1
    production = rust_source.blank_test_items(code)
    counts["raw-colour"] += len(RAW_COLOUR.findall(production))
    lines = production.splitlines()
    fills = [i for i, line in enumerate(lines) if ROUNDED_FILL.search(line)]
    texts = [i for i, line in enumerate(lines) if TEXT_RUN.search(line)]
    for fill in fills:
        if any(abs(fill - t) <= NEAR_LINES for t in texts):
            counts["hand-rolled"] += 1
    return counts


def scan(root: Path) -> dict[tuple[str, str], int]:
    found: dict[tuple[str, str], int] = {}
    for path in sources(root):
        counts = count_file(path.read_text(errors="replace"))
        relative = path.relative_to(root).as_posix()
        for rule, count in counts.items():
            if count:
                found[(relative, rule)] = count
    return found


def load_baseline(path: Path) -> dict[tuple[str, str], int]:
    allowed: dict[tuple[str, str], int] = {}
    if not path.exists():
        return allowed
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        rule, count, file = line.split(maxsplit=2)
        if rule not in RULES:
            raise ValueError(f"{path.name}: unknown rule {rule!r}")
        allowed[(file, rule)] = int(count)
    return allowed


def check(root: Path, baseline_path: Path) -> list[str]:
    found = scan(root)
    allowed = load_baseline(baseline_path)
    errors = []
    for key in sorted(set(found) | set(allowed)):
        file, rule = key
        have, may = found.get(key, 0), allowed.get(key, 0)
        if have > may:
            errors.append(f"{file}: {have} {rule} (allowed {may}); build it from the "
                          "design system's primitives (docs/design-language.md, Primitives)")
        elif have < may:
            errors.append(f"{baseline_path.name}: {rule} {may} {file} is now {have}; "
                          "lower the count (or delete the line at 0)")
    return errors


def print_baseline(root: Path) -> None:
    print("# Code under crates/session and apps that still paints controls by hand,")
    print("# joins facts with glyphs, uses symbols as icons or raw colours")
    print("# (tools/policy/check_primitives.py). Counts only shrink.")
    for (file, rule), count in sorted(scan(root).items(), key=lambda kv: (kv[0][1], kv[0][0])):
        print(f"{rule} {count} {file}")


def main(argv: list[str]) -> int:
    if argv[1:] == ["--print-baseline"]:
        print_baseline(ROOT)
        return 0
    errors = check(ROOT, BASELINE)
    if errors:
        print("\n".join(errors))
        return 1
    print("primitives OK (controls, facts, icons and colours come from the design system)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
