#!/usr/bin/env python3
"""A surface derives its shape from the rect it is laid out in, not from a number.

Comparing a width or height to a literal is how a layout decides it is narrow:
it makes the decision once, at whatever size the surface happened to have, and
then keeps it when the surface changes — which on a tiling desktop is every time
a window opens beside it. A window narrowed after it opened then draws its wide
layout into a narrow rect and clips, while the same app opened narrow draws
correctly. That defect is invisible to anyone who does not resize, which is why
it survived review: it is not found by looking at a window, only by watching
one change size.

The shared breakpoint (`ui::widget::NavMode`, derived per layout pass with
hysteresis) exists to make the decision once, correctly, for everyone. This
check keeps new comparisons from growing around it.

Counts in the baseline only shrink. A hit that is genuinely not a breakpoint —
a hairline, a minimum hit target, a drawing bound inside a fixed-size graphic —
is listed in the code-owned allowance table instead
(`crates/sdk/ui-core/tests/support/allowances.rs`), naming the file and the
enclosing item with the reason it is not a layout decision. A comment can never
move the exemption, and an entry that stops covering a hit fails this check.
"""
from __future__ import annotations

import argparse
import re
import sys
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
BASELINE = ROOT / "tools/policy/breakpoint_baseline.txt"
SCANNED = ("apps", "crates/session")

# A breakpoint compares a dimension to a size worth calling a size: 100 and up.
# `width < 1.0` is arithmetic about an empty rect, not a layout decision.
SIZE = r"-?\d{3,}(?:\.\d+)?"
COMPARISON = r"(?:<=|>=|<|>)"
# `w` and `h` are what the arithmetic in a layout calls them.
DIMENSION = r"(?:[A-Za-z_]*?(?:width|height)|w|h)"
PATTERNS = {
    "dimension compared to a literal": re.compile(
        rf"\b{DIMENSION}\b\s*{COMPARISON}\s*{SIZE}(?![0-9])"
    ),
    "literal compared to a dimension": re.compile(
        rf"{SIZE}(?![0-9])\s*{COMPARISON}\s*[A-Za-z_.]*?\b{DIMENSION}\b"
    ),
}
# An app that defines its own breakpoint has its own layout grammar: the same
# narrow window bends differently in each, and a fix in the shared one reaches
# none of them. The number itself is harmless; owning it is the defect.
APP_BREAKPOINT_CONSTANT = re.compile(
    r"\bconst\s+[A-Z0-9_]*(?:WIDTH|HEIGHT|BREAKPOINT|NARROW|COMPACT|WIDE)[A-Z0-9_]*"
    r"\s*:\s*(?:f32|f64|u32|i32|usize)\s*=\s*\d{3,}"
)
RULE = "Breakpoint"


def baseline(root: Path) -> Counter[str]:
    path = root / "tools/policy/breakpoint_baseline.txt"
    if not path.is_file():
        return Counter()
    counts: Counter[str] = Counter()
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        count, _, relative = line.partition(" ")
        counts[relative.strip()] += int(count)
    return counts


def audit(root: Path = ROOT) -> tuple[Counter[str], int, list[str]]:
    """The comparisons the tree has, the sources scanned, and the allowance
    entries that excused no hit."""
    found: Counter[str] = Counter()
    allowed = {(entry.file, entry.item) for entry in rust_source.allowances(root, RULE)}
    used: set[tuple[str, str]] = set()
    scanned = 0
    for tree in SCANNED:
        for source in sorted((root / tree).rglob("*.rs")):
            if any(part in {"target", "vendor"} for part in source.parts):
                continue
            # A test asserts what a surface does at a size; it is not a surface.
            if rust_source.is_test_file(source.relative_to(root)):
                continue
            scanned += 1
            text = source.read_text(encoding="utf-8")
            relative = str(source.relative_to(root))
            spans = rust_source.item_spans(rust_source.code_only(text))
            # Only code decides a layout: a comparison inside a string literal is
            # data being passed somewhere else (`yt-dlp`'s `[height<=720]` format
            # selector), a commented-out one is not code at all, and one inside a
            # `#[cfg(test)]` item asserts what a surface does at a size rather
            # than deciding its layout.
            code, literals = rust_source.lex(rust_source.blank_test_items(text))
            masked = list(code)
            for literal in literals:
                for index in range(literal.start, literal.end):
                    if masked[index] != "\n":
                        masked[index] = " "
            scanned_text = "".join(masked)
            patterns = list(PATTERNS.values())
            if tree == "apps":
                patterns.append(APP_BREAKPOINT_CONSTANT)
            for pattern in patterns:
                for match in pattern.finditer(scanned_text):
                    item = rust_source.enclosing_item(
                        spans, rust_source.line_number(scanned_text, match.start()) - 1
                    )
                    if (relative, item) in allowed:
                        used.add((relative, item))
                        continue
                    found[relative] += 1
    stale = sorted(f"{file} {item}" for file, item in allowed - used)
    return found, scanned, stale


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--write-baseline",
        action="store_true",
        help="record every comparison the tree has now, for a change that removes none",
    )
    arguments = parser.parse_args()

    found, scanned, stale = audit()
    if arguments.write_baseline:
        body = "\n".join(f"{count} {path}" for path, count in sorted(found.items()))
        BASELINE.write_text(
            "# Surfaces that still decide their shape from a number\n"
            "# (tools/policy/check_breakpoints.py). Counts only shrink: derive the\n"
            "# breakpoint from the rect this surface is laid out in, or list a hit\n"
            "# that is not a breakpoint in the allowance table\n"
            "# (crates/sdk/ui-core/tests/support/allowances.rs).\n"
            + (body + "\n" if body else ""),
            encoding="utf-8",
        )
        print(f"wrote {sum(found.values())} comparison(s) in {len(found)} file(s)")
        return 0

    allowed = baseline(ROOT)
    violations = []
    for path, count in sorted(found.items()):
        if count > allowed.get(path, 0):
            violations.append(f"{path}: {count} dimension(s) compared to a literal, {allowed.get(path, 0)} allowed")
    for entry in stale:
        violations.append(
            f"{entry}: the allowance covers no comparison; the item moved or was fixed"
        )
    if violations:
        print("breakpoint literals FAILED", file=sys.stderr)
        for violation in violations:
            print(f"  {violation}", file=sys.stderr)
        print(
            "  A surface takes its breakpoint from the rect it is laid out in, every\n"
            "  layout pass (`ui::widget::NavMode`). A hit that is not a breakpoint is\n"
            "  listed in the allowance table (crates/sdk/ui-core/tests/support/allowances.rs).",
            file=sys.stderr,
        )
        return 1
    print(
        "breakpoint literals OK "
        f"({scanned} sources; {sum(found.values())} known, none new)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
