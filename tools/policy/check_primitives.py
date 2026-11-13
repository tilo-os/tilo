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
  `Color::new([...])`, `Color::WHITE`/`BLACK`) instead of a theme token. A
  colour read from a field is neither picked nor written out — it is the
  theme's (`theme.text_primary`) or data the frame arrived with (a client's
  base colour, a backdrop's ground) — so only a written-out colour counts,
  by the provenance test the scrim rule (`MEDIA_FILL`) goes by.

A session surface — a sheet, menu, quick-settings page, bar, banner, lock
screen, onboarding page — looks like the apps only if it is built from the
same components they are (docs/architecture/ui-inventory.md). In production
Rust under `crates/session/` these refuse a surface that lays itself out
instead of composing the shared ones:

- hand-panel: painting a panel, card, edge or shadow itself
  (`RoundedRect::from_rect`, `panel_ground`, `panel_border`, `panel_shadow`,
  `surface::card`, …) instead of `ui::widget::paint_surface` /
  `SurfaceLevel`, `FloatingPanelFace`, `ListRowFace`, `QuickTileFace` or the
  `Button`/`IconButton` faces;
- hand-text: painting a bare text run (`draw_text`, `draw_style`) instead of
  putting the words in a widget (`Text`, `TextStack`, `ListRow`, `Section`,
  `Button`);
- hand-row: a painted shape within a few lines of a text run — a row, tile
  or field composed by hand — instead of `ListRow`, `ControlRow`,
  `ListRowFace` or `PropertyList`;
- hand-metric: a literal radius, padding, inset or gap instead of the
  theme's (`theme.tokens.radii`, `Spacing::…`, `Metrics::…`);
- own-scroll: a scroll position the surface keeps and clamps itself instead
  of the shared `Scrollable` / `Scroll`.

Text an agent's model reads is not interface: the agent's tool schemas,
tool descriptions and operation summaries (`MODEL_TEXT`) are prompts, in the
notation the model is prompted with (`id — label`, `| variant`), and no
person reads them on a surface. The two string-literal rules (glyph-join,
glyph-icon) skip them, as they skip logs and panics; a control painted or a
colour picked there is still counted.

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
# messages and lint reasons. `format_args!` goes straight to a writer (a log
# line written in one write), never into a string a surface shows.
NOT_UI = re.compile(
    r"(?:\b(?:info|warn|error|debug|trace|e?println|e?print|panic|unreachable|todo|"
    r"assert\w*|debug_assert\w*|bail|ensure|anyhow|write|writeln|format_err|format_args)!"
    r"|\.(?:expect|context|with_context|map_err)\b|\breason\s*=)[^;{}]*$"
)
RAW_COLOUR = re.compile(
    # Written out: channels, an array, a value this line names, or a named
    # constant (in a path, or read from one). A colour read from a field is the
    # theme's or a frame's, never the author's.
    r"\bColor::(?:from_rgb8|from_rgba8|from_rgb|from_rgba)\s*\(\s*[-0-9.]"
    r"|\bColor::new\s*\(\s*(?:\[|vec!|[-0-9.]|[A-Za-z_][A-Za-z0-9_]*\s*(?:::|,|\)))"
    r"|\bColor::(?:from_rgb8|from_rgba8|from_rgb|from_rgba)\s*\(\s*[A-Z][A-Z0-9_]*\b"
    r"|\bColor::(?:WHITE|BLACK)\b"
)
ROUNDED_FILL = re.compile(r"RoundedRect::(?:from_rect|new)")
# A fill painted from a media colour is a caption or subtitle scrim drawn over a
# picture, not a control: there is nothing in the design system to build it from,
# and the text beside it is media, not a label. Left out of the hand-rolled rule
# deliberately, on the colour's provenance rather than the file's identity.
MEDIA_FILL = re.compile(r"\bmedia\b")
TEXT_RUN = re.compile(r"\.draw_(?:text|style)\s*\(")
# How close a rounded fill and a text run must be to count as one control.
NEAR_LINES = 12

# --- session surfaces (docs/architecture/ui-inventory.md) -------------------
#
# A session surface that composes the shared components draws no ground of its
# own and paints no words: it hands content to a widget, which owns both. The
# patterns below are the ways a surface takes that back into its own hands.
HAND_PANEL = re.compile(
    r"\bRoundedRect::(?:from_rect|new)\s*\("
    r"|\b(?:panel_ground|panel_border|panel_shadow)\s*\("
    r"|\bsurface::(?:card|card_on|shadow|elevation|panel|panel_unsnapped|floating_shadow)\s*\("
)
HAND_TEXT = re.compile(r"\.(?:draw_text|draw_style|draw_text_at)\s*\(")
# A literal instead of a token: a radius, a padding, an inset or a gap written
# as a number. Values the theme names (a token read, a size measured from one)
# are not literals and do not count.
HAND_METRIC = re.compile(
    r"\b(?:radius|card_radius|row_radius|panel_radius|corner_radius|badge_radius)\s*:\s*-?\d"
    r"|\.radius\s*\(\s*-?\d"
    r"|Insets::(?:all|symmetric|horizontal|vertical|new)\s*\(\s*-?\d"
    r"|\.padding\s*\(\s*-?\d"
    r"|\.with_gap\s*\(\s*-?\d"
    # A rounded rect whose corner radius is a written-out number.
    r"|RoundedRect::(?:from_rect|new)\s*\([^()]*,\s*-?\d"
)
# A scroll position a surface keeps and clamps itself, instead of a
# `Scrollable` (a whole body) or the `Scroll` widget (a list's rows).
OWN_SCROLL = re.compile(r"\bscroll_(?:y|offset|top|position|x)\b")
# A `Decorator` that overrides `draw`: a widget wrapped to paint it itself.
# A decorator that only reads input is how a sheet keeps its own keyboard
# model (Enter, Escape) and is not a surface that paints.
DECORATOR_IMPL = re.compile(r"\bimpl\s+(?:[\w:<>]*::)?Decorator\s+for\s+\w+")
# A rounded rect used as a clip mask (a fade or reveal layer) is not a painted
# panel: it shapes what the wrapped widget drew. How far above the rect its
# `push_layer` may stand.
CLIP_LAYER = re.compile(r"\bpush_layer\s*\(")
CLIP_REACH = 8

RULES = (
    "hand-rolled",
    "glyph-join",
    "glyph-icon",
    "raw-colour",
    "hand-panel",
    "hand-text",
    "hand-row",
    "hand-metric",
    "own-scroll",
    "own-draw",
)

# What to build it from instead, named in the failure so the fix is the shared
# component and not another local copy of it.
ADVICE = {
    "hand-rolled": "ui::widget::{ButtonFace, BarControlFace, ListRowFace, ChipFace, …}",
    "glyph-join": "ui::widget::{Facts, TextStack, KeyHintsFace}",
    "glyph-icon": "ui::icons::Icon",
    "raw-colour": "a theme token",
    "hand-panel": ("ui::widget::{paint_surface, FloatingPanelFace, ListRowFace, QuickTileFace, "
                   "Button, IconButton}"),
    "hand-text": "a widget that holds the words (Text, TextStack, ListRow, Section, Button)",
    "hand-row": "ui::widget::{ListRow, ListRowFace, ControlRow, PropertyList}",
    "hand-metric": "theme.tokens.radii, ui::tokens::Spacing, ui::tokens::Metrics",
    "own-scroll": "ui::widget::{Scrollable, Scroll}",
    "own-draw": ("the shared widget that draws it (Surface, FloatingPanel, ListRow, Button, "
                 "Scrollable), wrapped only to read input if that is all the wrapper does"),
}

# Sources whose string literals are written for an agent's model, not shown to
# a person: the agent's tools (their schemas, descriptions and the operation
# summaries it is handed). A path ending in "/" names a directory.
MODEL_TEXT = (
    "apps/agent/src/tools.rs",
    "apps/agent/src/tools/",
)
# The rules about string literals a person reads.
TEXT_RULES = ("glyph-join", "glyph-icon")


def is_model_text(relative: str) -> bool:
    return any(relative == entry or (entry.endswith("/") and relative.startswith(entry))
               for entry in MODEL_TEXT)


# The crates that draw the shell's own surfaces (the desktop, its chrome, the
# sheets, the greeter, setup). Everything under `crates/session` paints one
# person-facing surface or another, except the compositor, which shapes a
# client's buffer and has no toolkit to compose from.
SURFACE_SCOPE = "crates/session/"
SURFACE_EXCEPTIONS = ("crates/session/compositor/",)


def is_surface_path(relative: str) -> bool:
    return relative.startswith(SURFACE_SCOPE) and not relative.startswith(SURFACE_EXCEPTIONS)


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


def count_own_draw(code: str) -> int:
    """`Decorator` impls that paint in their own `draw`: a surface drawn by the
    wrapper instead of by the widget it wraps.

    A wrapper that moves, fades or clips what it wraps only forwards `draw`,
    and one that reads input only overrides `handle_event`; neither is a
    surface of its own.
    """
    counts = 0
    for found in DECORATOR_IMPL.finditer(code):
        brace = code.find("{", found.end())
        if brace < 0:
            continue
        body = code[brace:rust_source.matching(code, brace)]
        draw = re.search(r"\bfn\s+draw\s*\(", body)
        if not draw:
            continue
        draw_brace = body.find("{", draw.end())
        if draw_brace < 0:
            continue
        drawn = body[draw_brace:rust_source.matching(body, draw_brace)]
        if re.search(r"\bscene\.(?:fill|stroke|draw_glyphs)\s*\(", drawn):
            counts += 1
    return counts


def in_clip_layer(lines: list[str], index: int) -> bool:
    """Whether the rounded rect on line `index` is a clip mask for the wrapped
    widget (a fade or a reveal), not a panel painted in its place: the
    `push_layer` it belongs to names it within the few lines above."""
    above = "\n".join(lines[max(0, index - CLIP_REACH):index + 1])
    return bool(CLIP_LAYER.search(above))


def count_file(text: str, relative: str = "") -> Counter:
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
        if not any(abs(fill - t) <= NEAR_LINES for t in texts):
            continue
        # The fill line and the twelve lines above it: one of them names the
        # media colour this scrim is painted from.
        if any(MEDIA_FILL.search(lines[n]) for n in range(max(0, fill - NEAR_LINES), fill + 1)):
            continue
        counts["hand-rolled"] += 1
    # A session surface's own ground, words, geometry and scrolling: counted in
    # the crates that draw the shell, where the shared components are the only
    # way to make one look like the rest of Tilo.
    if is_surface_path(relative):
        panels = [
            i for i, line in enumerate(lines)
            if HAND_PANEL.search(line) and not in_clip_layer(lines, i)
        ]
        counts["hand-panel"] += len(panels)
        counts["hand-text"] += len(HAND_TEXT.findall(production))
        counts["hand-metric"] += len(HAND_METRIC.findall(production))
        counts["own-scroll"] += len(OWN_SCROLL.findall(production))
        counts["own-draw"] += count_own_draw(production)
        texts = [i for i, line in enumerate(lines) if HAND_TEXT.search(line)]
        # One row drawn with several calls (its ground, its edge) is one row:
        # panel lines close together are the same shape.
        clusters: list[list[int]] = []
        for panel in panels:
            if clusters and panel - clusters[-1][-1] <= NEAR_LINES:
                clusters[-1].append(panel)
            else:
                clusters.append([panel])
        counts["hand-row"] += sum(
            1 for cluster in clusters
            if any(abs(panel - t) <= NEAR_LINES for panel in cluster for t in texts)
        )
    return counts


def scan(root: Path) -> dict[tuple[str, str], int]:
    found: dict[tuple[str, str], int] = {}
    for path in sources(root):
        relative = path.relative_to(root).as_posix()
        counts = count_file(path.read_text(errors="replace"), relative)
        for rule, count in counts.items():
            if rule in TEXT_RULES and is_model_text(relative):
                continue
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
    shown = (baseline_path.relative_to(root).as_posix()
             if baseline_path.is_relative_to(root) else baseline_path.name)
    errors = []
    for key in sorted(set(found) | set(allowed)):
        file, rule = key
        have, may = found.get(key, 0), allowed.get(key, 0)
        if have > may:
            advice = ADVICE.get(rule, "the design system's primitives")
            errors.append(f"{file}: {have} {rule} (allowed {may}); build it from {advice} "
                          "(docs/design-language.md, Primitives; "
                          "docs/architecture/ui-inventory.md)")
        elif have < may:
            errors.append(f"{shown}: {rule} {may} {file} is now {have}; "
                          "lower the count (or delete the line at 0)")
    return errors


def print_baseline(root: Path) -> None:
    print("# Code under crates/session and apps that still paints controls by hand,")
    print("# joins facts with glyphs, uses symbols as icons, writes out colours,")
    print("# or (in a session surface) lays out its own ground, words, geometry or")
    print("# scrolling (tools/policy/check_primitives.py). Counts only shrink.")
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
    print("primitives OK (controls, facts, icons and colours come from the design system)",
          file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
