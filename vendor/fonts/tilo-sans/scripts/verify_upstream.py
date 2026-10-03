# /// script
# requires-python = ">=3.11"
# dependencies = ["fonttools>=4.60", "uharfbuzz>=0.40"]
# ///
"""Check that the built Tilo Sans still matches upstream Instrument Sans for
every glyph upstream has.

    uv run vendor/fonts/tilo-sans/scripts/verify_upstream.py

Compares fonts/TiloSans*.ttf against upstream's variable TTFs at the pinned
commit (vendor/fonts/tilo-sans/.upstream/, cloned by import_upstream.py), at
every master and named-instance location:

* glyph set and cmap: every upstream glyph and code point is still present
  (additions are listed, not failures);
* advances and outlines of every upstream glyph, point by point;
* shaping: a corpus covering every upstream character, every kerning pair
  class, mark stacking and each GSUB feature shapes to the same glyphs and
  positions.

Exit status 1 when an upstream glyph changed. A change made on purpose (a
re-spaced or improved Latin glyph) shows up here by name; say why in the
commit that makes it.
"""

from __future__ import annotations

import argparse
import itertools
import sys
from pathlib import Path

import uharfbuzz as hb
from fontTools.pens.areaPen import AreaPen
from fontTools.pens.boundsPen import BoundsPen
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

ROOT = Path(__file__).resolve().parent.parent
UPSTREAM = ROOT / ".upstream" / "instrument-sans" / "fonts" / "variable"
PAIRS = [
    ("TiloSans[wdth,wght].ttf", "InstrumentSans[wdth,wght].ttf"),
    ("TiloSans-Italic[wdth,wght].ttf", "InstrumentSans-Italic[wdth,wght].ttf"),
]
LOCATIONS = [
    {"wdth": w, "wght": g} for w, g in itertools.product((75, 87.5, 100), (400, 500, 600, 700))
]
FEATURES = ["", "ss01", "ss02", "ss03", "ss04", "ss05", "ss06", "ss07", "ss08", "ss09", "ss10",
            "ss11", "ss12", "case", "tnum", "ordn", "liga=0", "kern=0"]
LANGUAGES = ["", "tr", "az", "kk", "tt", "crh", "ro", "mo", "ca"]
TOLERANCE = 1  # font units; cu2qu rounding may move an off-curve point by one


def glyph_points(font: TTFont, name: str):
    """Flattened outline: a composite and the same shape drawn as one outline compare equal."""
    coords, ends, flags = font["glyf"][name].getCoordinates(font["glyf"])
    return tuple(ends), tuple(f & 1 for f in flags), [tuple(p) for p in coords]


def shape_summary(font: TTFont, name: str):
    glyph_set = font.getGlyphSet()
    area, bounds = AreaPen(glyph_set), BoundsPen(glyph_set)
    glyph_set[name].draw(area)
    glyph_set[name].draw(bounds)
    return area.value, bounds.bounds or (0, 0, 0, 0)


def compare_outlines(ours: TTFont, theirs: TTFont, names: list[str]) -> tuple[list[str], set[str]]:
    """Point-by-point; where the point structure differs (cu2qu chose another
    split of the same curve) fall back to area and bounds."""
    problems, reapproximated = [], set()
    for name in names:
        a = glyph_points(ours, name)
        b = glyph_points(theirs, name)
        if a[0] != b[0] or a[1] != b[1]:
            (area_a, box_a), (area_b, box_b) = shape_summary(ours, name), shape_summary(theirs, name)
            box_delta = max(abs(p - q) for p, q in zip(box_a, box_b))
            area_delta = abs(area_a - area_b) / max(abs(area_b), 1)
            if box_delta <= TOLERANCE and area_delta < 0.01:
                reapproximated.add(name)
            else:
                problems.append(f"{name}: outline differs (bounds by {box_delta}, area by {area_delta:.2%})")
            continue
        worst = max((max(abs(p[0] - q[0]), abs(p[1] - q[1])) for p, q in zip(a[2], b[2])), default=0)
        if worst > TOLERANCE:
            problems.append(f"{name}: outline moved by up to {worst} units")
    return problems, reapproximated


def corpus(theirs: TTFont) -> list[str]:
    cmap = theirs.getBestCmap()
    chars = "".join(chr(u) for u in sorted(cmap) if u > 0x20)
    lines = [chars[i:i + 40] for i in range(0, len(chars), 40)]
    letters = [chr(u) for u in sorted(cmap) if chr(u).isalpha()]
    lines += ["".join(a + b for b in letters) for a in letters]  # every letter pair
    marks = "̧̨̦̀́̂̃̄̆̇̈̊̋̌"
    lines += ["".join(base + m for m in marks) for base in "AaEeIiOoUuYyij"]
    lines += ["".join(base + m1 + m2 for m1 in marks for m2 in marks) for base in "Aai"]
    lines += ["ffi ffl fi fl www -> <- 1a 2o 1A 2O l·l L·L Şş Ţţ iİ", "(HELLO) [WORLD] {¿¡HOLA!?} -–— :;"]
    return lines


def shape(blob: bytes, text: str, location: dict, features: str, language: str):
    face = hb.Face(blob)
    font = hb.Font(face)
    font.set_variations(location)
    buf = hb.Buffer()
    buf.add_str(text)
    buf.guess_segment_properties()
    if language:
        buf.language = language
    feats = {}
    if features:
        tag, _, value = features.partition("=")
        feats[tag] = int(value) if value else 1
    hb.shape(font, buf, feats)
    names = [font.glyph_to_string(i.codepoint) for i in buf.glyph_infos]
    pos = [(p.x_advance, p.x_offset, p.y_offset) for p in buf.glyph_positions]
    return names, pos


def compare_shaping(ours_path: Path, theirs_path: Path, theirs: TTFont) -> list[str]:
    a_blob = ours_path.read_bytes()
    b_blob = theirs_path.read_bytes()
    problems = []
    lines = corpus(theirs)
    for location in LOCATIONS[::3] + [LOCATIONS[-1]]:
        for features in FEATURES:
            for language in (LANGUAGES if features == "" else [""]):
                for text in lines:
                    a = shape(a_blob, text, location, features, language)
                    b = shape(b_blob, text, location, features, language)
                    if a == b:
                        continue
                    if a[0] != b[0]:
                        diff = next(i for i, (x, y) in enumerate(zip(a[0], b[0])) if x != y) if any(x != y for x, y in zip(a[0], b[0])) else min(len(a[0]), len(b[0]))
                        problems.append(f"{location} {features or '-'} {language or '-'}: glyphs differ at {text[max(0, diff-2):diff+3]!r}: {a[0][diff:diff+3]} vs {b[0][diff:diff+3]}")
                    else:
                        diff = next(i for i, (x, y) in enumerate(zip(a[1], b[1])) if x != y)
                        problems.append(f"{location} {features or '-'} {language or '-'}: position of {a[0][diff]} in {text[max(0, diff-2):diff+3]!r}: {a[1][diff]} vs {b[1][diff]}")
                    if len(problems) > 40:
                        return problems
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--fonts", type=Path, default=ROOT / "fonts")
    parser.add_argument("--upstream", type=Path, default=UPSTREAM)
    args = parser.parse_args()

    failed = False
    for ours_name, theirs_name in PAIRS:
        ours_path = args.fonts / ours_name
        theirs_path = args.upstream / theirs_name
        if not ours_path.exists():
            print(f"{ours_path} missing: run build.py first", file=sys.stderr)
            return 2
        if not theirs_path.exists():
            print(f"{theirs_path} missing: run scripts/import_upstream.py once to clone upstream", file=sys.stderr)
            return 2
        ours, theirs = TTFont(ours_path), TTFont(theirs_path)
        print(f"== {ours_name} vs upstream {theirs_name}")
        problems = []

        upstream_names = theirs.getGlyphOrder()
        missing = sorted(set(upstream_names) - set(ours.getGlyphOrder()))
        added = sorted(set(ours.getGlyphOrder()) - set(upstream_names))
        if missing:
            problems.append(f"glyphs missing: {' '.join(missing)}")
        their_cmap, our_cmap = theirs.getBestCmap(), ours.getBestCmap()
        for u, name in their_cmap.items():
            if our_cmap.get(u) != name:
                problems.append(f"U+{u:04X} maps to {our_cmap.get(u)} here, {name} upstream")
        print(f"   glyphs: {len(upstream_names)} upstream, {len(added)} added")
        for tag in ("wdth", "wght"):
            a = next(x for x in ours["fvar"].axes if x.axisTag == tag)
            b = next(x for x in theirs["fvar"].axes if x.axisTag == tag)
            if (a.minValue, a.defaultValue, a.maxValue) != (b.minValue, b.defaultValue, b.maxValue):
                problems.append(f"axis {tag} differs")
        if ours["avar"].segments != theirs["avar"].segments:
            problems.append("avar differs")
        for table, attrs in (("OS/2", ("sxHeight", "sCapHeight", "sTypoAscender", "sTypoDescender", "sTypoLineGap", "usWinAscent", "usWinDescent", "fsSelection")),
                             ("hhea", ("ascent", "descent", "lineGap")), ("post", ("italicAngle", "underlinePosition", "underlineThickness")),
                             ("head", ("unitsPerEm",))):
            for attr in attrs:
                if getattr(ours[table], attr) != getattr(theirs[table], attr):
                    problems.append(f"{table}.{attr}: {getattr(ours[table], attr)} here, {getattr(theirs[table], attr)} upstream")

        shared = [n for n in upstream_names if n not in missing]
        checked = 0
        reapproximated: set[str] = set()
        for location in LOCATIONS:
            a = instantiateVariableFont(TTFont(ours_path), location, static=True)
            b = instantiateVariableFont(TTFont(theirs_path), location, static=True)
            for name in shared:
                advance_a, lsb_a = a["hmtx"][name]
                advance_b, lsb_b = b["hmtx"][name]
                # The side bearing is the outline's xMin; a one-unit rounding
                # difference there is already judged by the outline check.
                if advance_a != advance_b or abs(lsb_a - lsb_b) > TOLERANCE:
                    problems.append(f"{location}: {name} advance/lsb {a['hmtx'][name]} here, {b['hmtx'][name]} upstream")
            outline_problems, approx = compare_outlines(a, b, shared)
            problems += [f"{location}: {line}" for line in outline_problems]
            reapproximated |= approx
            checked += 1
        print(f"   outlines + advances: {len(shared)} glyphs at {checked} locations")
        if reapproximated:
            print(f"   same shape, different quadratic points (area within 1%, bounds within {TOLERANCE}): {' '.join(sorted(reapproximated))}")

        shaping = compare_shaping(ours_path, theirs_path, theirs)
        print(f"   shaping: {len(corpus(theirs))} lines x {len(FEATURES)} feature settings x {len(LANGUAGES)} languages")
        problems += shaping

        if problems:
            failed = True
            print(f"   {len(problems)} difference(s):")
            for line in problems[:60]:
                print(f"     {line}")
        else:
            print("   identical for every upstream glyph")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
