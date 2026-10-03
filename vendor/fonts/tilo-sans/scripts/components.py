# /// script
# requires-python = ">=3.11"
# dependencies = ["fonttools>=4.60", "ufoLib2", "skia-pathops", "shapely>=2"]
# ///
"""Clearance between the parts of every accented glyph, in every master and
between them; the angle of the stacked tones; the height of the stacks.

    uv run vendor/fonts/tilo-sans/scripts/components.py            # report problems
    uv run vendor/fonts/tilo-sans/scripts/components.py --all      # every measured gap
    uv run vendor/fonts/tilo-sans/scripts/components.py --glyphs Abreveacute ecircumflexhookabove

A composite is flattened into its parts: the base letter and each mark (a
stacked-mark ligature such as circumflexcomb_acutecomb contributes both of
its marks). For every pair the script measures the shortest distance between
the inks and, where they overlap, the overlapping area. That catches what a
look at the bounding boxes misses: a tone mark dipping into a breve's cup, a
tilde resting on a circumflex, a capital accent touching its letter.

Limits (font units):
  mark on mark       >= 30   tone or tilde above a breve, tilde above a circumflex
  mark beside mark   >= 30   acute, grave or hook beside a circumflex
  mark above letter  >= 30
  mark below letter  >= 24   dot below
An ogonek or cedilla joins its letter by design, so it is not held to a
distance from the letter (only from other marks).
They hold in every master and at the instances between them (wght
500/550/600/650 at wdth 75 and 87.5, and 500/550/600 at wdth 100), where the
parts are interpolated from the masters as the variable font does it.

Two more rules, per master:
  tone angle   the acute and grave beside a circumflex or on a breve lean
               like the master's own acutecomb/gravecomb, within 5 degrees,
               so sắc and huyền stay apart however tight the stack;
  stack top    no added glyph's ink rises above 1030 units: with g's
               descender at -215, lines 1250 apart (TextStyle's default 1.25)
               keep a capital stack clear of the line above.

A problem is an error; build.py runs this check and stops. Glyphs Instrument
Sans ships are measured but not held to the limits: their outlines are
upstream's (verify_upstream.py guards them).
"""

from __future__ import annotations

import argparse
import itertools
import sys
from pathlib import Path

import ufoLib2
from fontTools.designspaceLib import DesignSpaceDocument
from fontTools.misc.transform import Transform
from fontTools.pens.boundsPen import BoundsPen
from shapely import union_all
from ufoLib2.objects import Contour, Point

sys.path.insert(0, str(Path(__file__).resolve().parent))
from inkgeom import gap, geometry  # noqa: E402
from vietnamese import stroke_axis_angle  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
SOURCES = ROOT / "sources"
DESIGNSPACES = ["TiloSans.designspace", "TiloSans-Italic.designspace"]
LIMITS = {"stacked": 30, "beside": 30, "above": 30, "below": 24}
ANGLE_TOLERANCE = 5
# marks drawn to join the letter they hang from
ATTACHED = {"ogonekcomb", "cedillacomb"}
MAX_TOP = 1030
# glyph with a tone of its own -> the master's mark it must lean like
TONES = {f"{base}_{tone}{case}": tone for base in ("circumflexcomb",) for tone in ("acutecomb", "gravecomb")
         for case in ("", ".case")}
TONES.update({"acutecomb.case": "acutecomb", "gravecomb.case": "gravecomb"})
# instances between the masters: (wdth, wght)
BETWEEN = [(w, g) for g in (500, 550, 600, 650) for w in (75, 87.5)] + [(100, g) for g in (500, 550, 600)]
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
    return measure_inks([(label, mark, ink(g, t)) for label, mark, g, t in ps])


def measure_inks(inks):
    results = []
    bases = [i for i in inks if not i[1] and i[2] is not None]
    marks = [i for i in inks if i[1] and i[2] is not None]
    if bases:
        base_ink = union_all([b[2] for b in bases])
        base_label = "+".join(b[0] for b in bases)
        base_mid = (base_ink.bounds[1] + base_ink.bounds[3]) / 2
        for label, _, m in marks:
            if label.split(".")[0] in ATTACHED:
                continue
            d, ov = gap(base_ink, m)
            kind = "below" if m.bounds[3] < base_mid else "above"
            results.append((kind, base_label, label, d, ov))
    for (la, _, a), (lb, _, b) in itertools.combinations(marks, 2):
        d, ov = gap(a, b)
        results.append((relation(la, lb), la, lb, d, ov))
    return results


def interpolated_inks(fonts, name, wdth, wght):
    """The glyph's parts at (wdth, wght), interpolated from the four masters
    (bilinear, as the variable font interpolates its four corner masters)."""
    tw, tg = (wdth - 75) / 25, (wght - 400) / 300
    weights = {("Condensed", "Regular"): (1 - tw) * (1 - tg), ("Condensed", "Bold"): (1 - tw) * tg,
               ("", "Regular"): tw * (1 - tg), ("", "Bold"): tw * tg}
    per = {k: parts(fonts[k], name) for k in weights}
    ref = per[("", "Regular")]
    if len(ref) < 2 or not any(p[1] for p in ref) or any(len(v) != len(ref) for v in per.values()):
        return None
    out = []
    for i, (label, mark, glyph, _) in enumerate(ref):
        contours = []
        for ci, contour in enumerate(glyph.contours):
            pts = []
            for pi, pt in enumerate(contour.points):
                x = sum(w * per[k][i][2].contours[ci].points[pi].x for k, w in weights.items())
                y = sum(w * per[k][i][2].contours[ci].points[pi].y for k, w in weights.items())
                pts.append(Point(x, y, pt.type, pt.smooth))
            contours.append(Contour(points=pts))
        t = tuple(sum(w * tuple(per[k][i][3])[j] for k, w in weights.items()) for j in range(6))

        def draw(pen, contours=contours):
            for c in contours:
                c.draw(pen)
        out.append((label, mark, geometry(draw, t)))
    return out


def tone_angle_problems(font, master) -> list[str]:
    """A stacked acute or grave leaning unlike the master's own."""
    out = []
    for name, ref in TONES.items():
        if name not in font or not font[name].contours:
            continue
        own = stroke_axis_angle([[p.x, p.y] for p in font[ref].contours[0].points])
        got = stroke_axis_angle([[p.x, p.y] for p in font[name].contours[-1].points])
        if abs(got - own) > ANGLE_TOLERANCE:
            out.append(f"{master:20s} {name:30s} tone at {got:.1f} deg, {ref} at {own:.1f}"
                       f" (at most {ANGLE_TOLERANCE} apart)")
    return out


def top_problems(font, master, upstream) -> list[str]:
    out = []
    for g in font:
        if g.name in upstream:
            continue
        bp = BoundsPen(font)
        g.draw(bp)
        if bp.bounds and bp.bounds[3] > MAX_TOP:
            out.append(f"{master:20s} {g.name:30s} ink top {bp.bounds[3]:.0f} (at most {MAX_TOP})")
    return out


def run(glyphs=None, show_all=False, quiet=False) -> list[str]:
    """Check every master and the instances between them; returns the
    problems as printable lines."""
    problems: list[str] = []
    for dsname in DESIGNSPACES:
        ds = DesignSpaceDocument.fromfile(SOURCES / dsname)
        fonts = {}
        for source in ds.sources:
            font = ufoLib2.Font.open(source.path)
            upstream = set(font.lib.get(UPSTREAM_KEY, []))
            master = Path(source.path).stem.replace("TiloSans-", "")
            loc = source.location
            fonts[("Condensed" if loc["Width"] == 75 else "", "Bold" if loc["Weight"] == 700 else "Regular")] = font
            problems += tone_angle_problems(font, master)
            problems += top_problems(font, master, upstream)
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
        family = "italic" if "Italic" in dsname else "upright"
        ref = fonts[("", "Regular")]
        upstream = set(ref.lib.get(UPSTREAM_KEY, []))
        for wdth, wght in BETWEEN:
            where = f"{family} {wdth:g}/{wght}"
            for name in glyphs or sorted(ref.keys()):
                if name not in ref or name in upstream:
                    continue
                inks = interpolated_inks(fonts, name, wdth, wght)
                for kind, a, b, d, ov in measure_inks(inks or []):
                    what = f"overlap {ov:.0f} u²" if ov else f"gap {d:.0f}"
                    line = f"{where:20s} {name:30s} {a:28s} | {b:28s} {kind:7s} {what} (min {LIMITS[kind]})"
                    if d < LIMITS[kind]:
                        problems.append(line)
                    elif show_all and not quiet:
                        print("     " + line)
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
    print(f"{len(problems)} problem(s)")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
