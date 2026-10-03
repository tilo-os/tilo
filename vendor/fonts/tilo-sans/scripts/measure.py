# /// script
# requires-python = ">=3.11"
# dependencies = ["fonttools>=4.60", "ufoLib2"]
# ///
"""Print the vertical metrics, stem weights, overshoots and sidebearings of
every master, measured from the outlines: the numbers a new glyph must match.

    uv run vendor/fonts/tilo-sans/scripts/measure.py
    uv run vendor/fonts/tilo-sans/scripts/measure.py --glyphs n o H O A-cy

Stems are measured with a horizontal or vertical probe through the glyph
(n's left stem at half x-height, o's sides and bowls at its centre lines, H's
stem and bar, O likewise). In the italic the horizontal probe crosses a
slanted stem; the figure shown is corrected to the stroke's true thickness
(run x cos(italic angle)). Italic sidebearings are measured on the outline
slanted back upright about half the x-height, as Glyphs.app shows them.
"""

from __future__ import annotations

import argparse
import math
from pathlib import Path

import ufoLib2
from fontTools.designspaceLib import DesignSpaceDocument
from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.pointInsidePen import PointInsidePen
from fontTools.pens.transformPen import TransformPen

ROOT = Path(__file__).resolve().parent.parent
SOURCES = ROOT / "sources"


def inside(font, glyph, x, y) -> bool:
    pen = PointInsidePen(font, (x, y))
    glyph.draw(pen)
    return pen.getResult()


def runs(font, glyph, axis: str, at: float, lo: float, hi: float, step: float = 0.5) -> list[tuple[float, float]]:
    """Ink runs along a horizontal (axis='x', y=at) or vertical (axis='y', x=at) probe."""
    out, start = [], None
    v = lo
    while v <= hi:
        hit = inside(font, glyph, v, at) if axis == "x" else inside(font, glyph, at, v)
        if hit and start is None:
            start = v
        elif not hit and start is not None:
            out.append((start, v))
            start = None
        v += step
    if start is not None:
        out.append((start, hi))
    return out


def bounds(font, glyph):
    pen = BoundsPen(font)
    glyph.draw(pen)
    return pen.bounds


def sidebearings(font, glyph) -> tuple[int, int] | None:
    """LSB/RSB as Glyphs shows them: in the italic, of the outline slanted
    back upright about half the x-height."""
    angle = math.radians(abs(font.info.italicAngle or 0))
    pen = BoundsPen(font)
    shear = math.tan(angle)
    glyph.draw(TransformPen(pen, (1, 0, -shear, 1, shear * font.info.xHeight / 2, 0)))
    if pen.bounds is None:
        return None
    return round(pen.bounds[0]), round(glyph.width - pen.bounds[2])


def measure(font: ufoLib2.Font) -> dict:
    info = font.info
    slant = math.cos(math.radians(abs(info.italicAngle or 0)))
    xh, cap = info.xHeight, info.capHeight
    m: dict = {}

    def stem(name: str, y: float, index: int = 0) -> float | None:
        g = font[name]
        r = runs(font, g, "x", y, -100, g.width + 100)
        return round((r[index][1] - r[index][0]) * slant) if len(r) > index else None

    def bar(name: str, x: float, ylo: float, yhi: float, index: int = 0) -> float | None:
        g = font[name]
        r = runs(font, g, "y", x, ylo, yhi)
        return round(r[index][1] - r[index][0]) if len(r) > index else None

    def centre_x(name: str, y: float) -> float:
        # In the italic the optical centre moves right with height.
        b = bounds(font, font[name])
        return (b[0] + b[2]) / 2 + (y - (b[1] + b[3]) / 2) * math.tan(math.radians(abs(info.italicAngle or 0)))

    m["n stem"] = stem("n", xh / 2)
    m["o side"] = stem("o", xh / 2)
    m["o top/bottom"] = (bar("o", centre_x("o", xh), xh / 2, xh + 30), bar("o", centre_x("o", 0), -30, xh / 2))
    m["H stem"] = stem("H", cap / 4)
    m["H bar"] = bar("H", centre_x("H", cap / 2), cap / 4, cap * 3 / 4)
    m["O side"] = stem("O", cap / 2)
    m["O top/bottom"] = (bar("O", centre_x("O", cap), cap / 2, cap + 30), bar("O", centre_x("O", 0), -30, cap / 2))
    ob = bounds(font, font["o"])
    Ob = bounds(font, font["O"])
    m["overshoot o (below, above)"] = (round(-ob[1]), round(ob[3] - xh))
    m["overshoot O (below, above)"] = (round(-Ob[1]), round(Ob[3] - cap))
    m["ascender (b, d, l top)"] = tuple(round(bounds(font, font[n])[3]) for n in ("b", "d", "l"))
    m["descender (p, q bottom)"] = tuple(round(bounds(font, font[n])[1]) for n in ("p", "q"))
    for name in ("n", "o", "H", "O"):
        g = font[name]
        m[f"{name} advance, LSB, RSB"] = (g.width, *sidebearings(font, g))
    return m


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--glyphs", nargs="*", help="also print advance and sidebearings of these glyphs")
    args = parser.parse_args()
    for ds_name in ("TiloSans.designspace", "TiloSans-Italic.designspace"):
        ds = DesignSpaceDocument.fromfile(SOURCES / ds_name)
        print(f"== {ds_name}")
        for source in ds.sources:
            font = ufoLib2.Font.open(source.path)
            loc = ", ".join(f"{k} {v:g}" for k, v in source.location.items())
            info = font.info
            print(f"-- {source.filename}  ({loc})  UPM {info.unitsPerEm}, x {info.xHeight}, cap {info.capHeight}, "
                  f"asc {info.ascender}, desc {info.descender}, italic angle {info.italicAngle or 0}")
            for key, value in measure(font).items():
                print(f"   {key:28} {value}")
            for name in args.glyphs or []:
                if name in font:
                    g = font[name]
                    sb = sidebearings(font, g) or ("-", "-")
                    print(f"   {name:28} advance {g.width}, LSB {sb[0]}, RSB {sb[1]}")
                else:
                    print(f"   {name:28} (not in this master)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
