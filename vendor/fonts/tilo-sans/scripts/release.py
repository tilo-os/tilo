# /// script
# requires-python = ">=3.11"
# dependencies = ["fonttools>=4.60"]
# ///
"""Write the shipping (default) and the review (full) binaries.

    uv run vendor/fonts/tilo-sans/scripts/release.py     # build.py runs it after compiling

The sources hold everything every area has drawn. Not all of it ships: a
script or feature that review has not yet approved is held back, so Tilo's
fallback face (Onest for Cyrillic, Geologica for Greek) keeps drawing it.

    fonts/TiloSans[wdth,wght].ttf, fonts/TiloSans-Italic[wdth,wght].ttf
        what build.py compiles: everything (the tools and proofs use these)
    fonts/full/      the same files, for review only; never shipped
    fonts/default/   what Tilo ships: the full fonts with HELD_BACK removed

A held-back script loses its code points (and with them every glyph only
they reach: alternates, locl forms, ligature parts); a held-back feature
loses its lookups and the glyphs only it reaches. To ship one, delete its
entry from HELD_BACK, rebuild, and check fonts/default/ with proof.py.
"""

from __future__ import annotations

import shutil
import sys
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont

ROOT = Path(__file__).resolve().parent.parent
FONTS = ROOT / "fonts"
FILES = ("TiloSans[wdth,wght].ttf", "TiloSans-Italic[wdth,wght].ttf")

# area -> what to hold back. Code points as inclusive ranges.
HELD_BACK = {
    # Cyrillic: Russian, Ukrainian, Belarusian, Bulgarian, Serbian, Macedonian,
    # Kazakh. Held back until the kerning and heavy-weight issues found in
    # review are fixed and native readers have approved it.
    "cyrillic": {
        "codepoints": [(0x0400, 0x052F), (0x1C80, 0x1C8F), (0x2DE0, 0x2DFF), (0xA640, 0xA69F)],
        "features": [],
    },
    # Distinct I and l (ss13, cv01, cv02) and the slashed zero: held back
    # until review approves the diagonal clearances (serif I against 1, 4,
    # ¿ and the accents) and I1 at 12-14 px (proofs/sheets/il-sheet/).
    "il": {
        "codepoints": [],
        "features": ["ss13", "cv01", "cv02", "zero"],
    },
}


def held_codepoints() -> set[int]:
    return {cp for area in HELD_BACK.values() for lo, hi in area["codepoints"] for cp in range(lo, hi + 1)}


def held_features() -> set[str]:
    return {tag for area in HELD_BACK.values() for tag in area["features"]}


def default_font(path: Path) -> TTFont:
    font = TTFont(path)
    keep = sorted(set(font.getBestCmap()) - held_codepoints())
    features = sorted({fr.FeatureTag for t in ("GSUB", "GPOS") if t in font
                       for fr in font[t].table.FeatureList.FeatureRecord} - held_features())
    # The glyphs the kept features reach. aalt lists every alternate (the
    # held-back ones too), so it does not count; it is pruned to these glyphs.
    probe = subset.Options()
    probe.layout_features = [f for f in features if f != "aalt"]
    probe.notdef_outline = True
    probe.glyph_names = True
    reach = subset.Subsetter(probe)
    reach.populate(unicodes=keep)
    reached = TTFont(path)
    reach.subset(reached)
    glyphs = reached.getGlyphOrder()

    options = subset.Options()
    options.layout_features = features
    options.layout_closure = False
    options.layout_scripts = ["*"]
    options.name_IDs = ["*"]
    options.name_languages = ["*"]
    options.name_legacy = True
    options.glyph_names = True
    options.notdef_outline = True
    options.recalc_bounds = False
    options.hinting = True
    options.legacy_kern = True
    options.drop_tables = []
    options.passthrough_tables = True
    subsetter = subset.Subsetter(options)
    subsetter.populate(glyphs=glyphs, unicodes=keep)
    subsetter.subset(font)
    return font


def main() -> int:
    (FONTS / "default").mkdir(parents=True, exist_ok=True)
    (FONTS / "full").mkdir(parents=True, exist_ok=True)
    for name in FILES:
        src = FONTS / name
        if not src.exists():
            continue
        shutil.copy2(src, FONTS / "full" / name)
        font = default_font(src)
        out = FONTS / "default" / name
        font.save(out)
        full = TTFont(src)
        print(f"[release] {name}: full {len(full.getGlyphOrder())} glyphs / {len(full.getBestCmap())} code points;"
              f" default {len(font.getGlyphOrder())} glyphs / {len(font.getBestCmap())} code points"
              f" (held back: {', '.join(HELD_BACK) or 'nothing'})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
