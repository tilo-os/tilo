# /// script
# dependencies = ["uharfbuzz", "fonttools"]
# ///
"""Spot checks on the built Greek: pair kerning as HarfBuzz applies it, the
ano teleia and round-dot substitutions, and how far accents hang past the
advance.

    uv run vendor/fonts/tilo-sans/scripts/greek/check.py [--pairs "Τυ Τώ κλ"]
"""
from __future__ import annotations

import argparse
from pathlib import Path

import uharfbuzz as hb
from fontTools.pens.boundsPen import BoundsPen
from fontTools.ttLib import TTFont

FONTS = Path(__file__).resolve().parents[2] / "fonts"
INSTANCES = [("Regular", 400, 100), ("Bold", 700, 100), ("Cond", 400, 75), ("CondBold", 700, 75)]
PAIRS = ("Τυ Τύ Τω Τώ Τμ Τψ Τκ Τι Γω Γώ Γυ Γι Υω Υμ Υυ Ρυ Ψυ Τo φχ φλ κλ ολ αλ λο λα σσ σα "
         "ΤΆ ΓΈ ΥΌ ΤΏ ϊλ τΐ ΐκ Τϊ γγ λλ ζΤ").split()
WORDS = ["τέλος·και", "τέλος·και"]


def shaper(path, wght, wdth, features=None):
    blob = hb.Blob.from_file_path(str(path))
    face = hb.Face(blob)
    font = hb.Font(face)
    font.set_variations({"wght": wght, "wdth": wdth})

    def shape(text, lang="el"):
        buf = hb.Buffer()
        buf.add_str(text)
        buf.guess_segment_properties()
        buf.language = lang
        hb.shape(font, buf, features or {})
        return buf.glyph_infos, buf.glyph_positions
    return shape


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pairs", default=None)
    ap.add_argument("--italic", action="store_true")
    args = ap.parse_args()
    path = FONTS / ("TiloSans-Italic[wdth,wght].ttf" if args.italic else "TiloSans[wdth,wght].ttf")
    tt = TTFont(path)
    order = tt.getGlyphOrder()
    pairs = args.pairs.split() if args.pairs else PAIRS

    print("pair kerning (units):")
    print(f"{'':6}" + "".join(f"{n:>9}" for n, _, _ in INSTANCES))
    shapes = {n: shaper(path, w, d) for n, w, d in INSTANCES}
    for p in pairs:
        row = []
        for n, _, _ in INSTANCES:
            infos, pos = shapes[n](p)
            infos1, pos1 = shapes[n](p[0])
            row.append(pos[0].x_advance - pos1[0].x_advance)
        print(f"{p:6}" + "".join(f"{v:9}" for v in row))

    print("\nsubstitutions (Regular):")
    for feats, text in (({}, WORDS[0]), ({}, WORDS[1]), ({"ss01": True}, WORDS[0]), ({"ss01": True}, "ϊΐΰ΅;")):
        infos, _ = shaper(path, 400, 100, feats)(text)
        print(f"  {str(feats):16} {text:12} -> {' '.join(order[i.codepoint] for i in infos)}")

    print("\naccent overhang past the advance (left/right, units; negative = inside):")
    gs = tt.getGlyphSet(location={"wght": 400, "wdth": 100})
    names = ["iotadieresis", "upsilondieresis", "iotadieresistonos", "upsilondieresistonos", "Iotadieresis",
             "Upsilondieresis", "dieresistonos"]
    for n, w, d in INSTANCES:
        gs = tt.getGlyphSet(location={"wght": w, "wdth": d})
        cells = []
        for g in names:
            bp = BoundsPen(gs)
            gs[g].draw(bp)
            x0, _, x1, _ = bp.bounds
            cells.append(f"{g[:12]} {-x0:4.0f}/{x1 - gs[g].width:4.0f}")
        print(f"  {n:9} " + "  ".join(cells))


if __name__ == "__main__":
    main()
