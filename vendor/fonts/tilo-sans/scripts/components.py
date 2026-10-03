# /// script
# requires-python = ">=3.11"
# dependencies = ["fonttools>=4.60", "ufoLib2", "skia-pathops", "shapely>=2"]
# ///
"""Clearance between the parts of every accented glyph, in every master.

    uv run vendor/fonts/tilo-sans/scripts/components.py            # report problems
    uv run vendor/fonts/tilo-sans/scripts/components.py --all      # every measured gap
    uv run vendor/fonts/tilo-sans/scripts/components.py --glyphs Abreveacute ecircumflexhookabove

A composite is flattened into its parts: the base letter and each mark (a
stacked-mark ligature such as circumflexcomb_acutecomb contributes both of
its marks). For every pair the script measures the shortest distance between
the inks and, where they overlap, the overlapping area. That catches what a
look at the bounding boxes misses: a tone mark dipping into a breve's cup, a
tilde resting on a circumflex, a capital accent touching its letter.

Limits (font units, every master):
  mark on mark       >= 38   tone or tilde above a breve, tilde above a circumflex
  mark beside mark   >= 32   acute, grave or hook beside a circumflex
  mark above letter  >= 30
  mark below letter  >= 24   dot below
A gap below its limit is an error; build.py runs this check and stops.
Glyphs Instrument Sans ships are measured but not held to the limits: their
outlines are upstream's (verify_upstream.py guards them).
"""

from __future__ import annotations

import argparse
import itertools
import sys
from pathlib import Path

import ufoLib2
from fontTools.designspaceLib import DesignSpaceDocument
from fontTools.misc.transform import Transform
from shapely import union_all

sys.path.insert(0, str(Path(__file__).resolve().parent))
from inkgeom import gap, geometry  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
SOURCES = ROOT / "sources"
DESIGNSPACES = ["TiloSans.designspace", "TiloSans-Italic.designspace"]
LIMITS = {"stacked": 38, "beside": 32, "above": 30, "below": 24}
CATEGORIES_KEY = "public.openTypeCategories"
# The stored glyph order is upstream's: build.py appends new glyphs only in
# memory, so the names it lists are exactly Instrument Sans' glyphs.
UPSTREAM_KEY = "public.glyphOrder"


def is_mark(font, name: str) -> bool:
    if font.lib.get(CATEGORIES_KEY, {}).get(name) == "mark":
        return True
    base = name.split(".")[0]
    return base.endswith("comb") or "comb_" in base


def parts(font, name: str, transform=Transform(), out=None):
    """[(label, is_mark, glyph, transform)] for the glyph's leaf parts: each
    glyph's own contours form one part, components are followed down."""
    out = [] if out is None else out
    glyph = font[name]
    if len(glyph.contours):
        out.append((name, is_mark(font, name), glyph, transform))
    for comp in glyph.components:
        if comp.baseGlyph in font:
            parts(font, comp.baseGlyph, transform.transform(comp.transformation), out)
    return out


def ink(glyph, transform):
    def draw(pen):
        for c in glyph.contours:
            c.draw(pen)
    return geometry(draw, tuple(transform))


def relation(a: str, b: str) -> str:
    """How two marks sit: a tone in a circumflex ligature sits beside the
    circumflex; everything else stacks."""
    names = {a.split(".")[0], b.split(".")[0]}
    circ = any(n.startswith("circumflexcomb") for n in names)
    tilde = any(n.startswith("tildecomb") for n in names)
    return "beside" if circ and not tilde else "stacked"


def measure(font, name):
    """[(kind, part_a, part_b, distance, overlap_area)] for one glyph."""
    ps = parts(font, name)
    if len(ps) < 2 or not any(p[1] for p in ps):
        return []
    inks = [(label, mark, ink(g, t)) for label, mark, g, t in ps]
    results = []
    bases = [i for i in inks if not i[1] and i[2] is not None]
    marks = [i for i in inks if i[1] and i[2] is not None]
    if bases:
        base_ink = union_all([b[2] for b in bases])
        base_label = "+".join(b[0] for b in bases)
        base_mid = (base_ink.bounds[1] + base_ink.bounds[3]) / 2
        for label, _, m in marks:
            d, ov = gap(base_ink, m)
            kind = "below" if m.bounds[3] < base_mid else "above"
            results.append((kind, base_label, label, d, ov))
    for (la, _, a), (lb, _, b) in itertools.combinations(marks, 2):
        d, ov = gap(a, b)
        results.append((relation(la, lb), la, lb, d, ov))
    return results


def run(glyphs=None, show_all=False, quiet=False) -> list[str]:
    """Check every master; returns the problems as printable lines."""
    problems: list[str] = []
    for dsname in DESIGNSPACES:
        ds = DesignSpaceDocument.fromfile(SOURCES / dsname)
        for source in ds.sources:
            font = ufoLib2.Font.open(source.path)
            upstream = set(font.lib.get(UPSTREAM_KEY, []))
            master = Path(source.path).stem.replace("TiloSans-", "")
            for name in glyphs or sorted(font.keys()):
                if name not in font:
                    continue
                held = name not in upstream
                for kind, a, b, d, ov in measure(font, name):
                    limit = LIMITS[kind]
                    bad = held and d < limit
                    what = f"overlap {ov:.0f} u²" if ov else f"gap {d:.0f}"
                    line = f"{master:20s} {name:30s} {a:28s} | {b:28s} {kind:7s} {what} (min {limit})"
                    if bad:
                        problems.append(line)
                    if show_all and not quiet:
                        print(("FAIL " if bad else ("     " if held else "upst ")) + line)
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--glyphs", nargs="*", help="only these glyphs")
    parser.add_argument("--all", action="store_true", help="print every gap, not just the problems")
    args = parser.parse_args()
    problems = run(glyphs=args.glyphs, show_all=args.all)
    if not args.all:
        for line in problems:
            print("FAIL " + line)
    print(f"{len(problems)} clearance problem(s)")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
