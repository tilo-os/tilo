# /// script
# requires-python = ">=3.11"
# dependencies = ["fonttools>=4.60", "uharfbuzz", "numpy", "ufoLib2"]
# ///
"""Do neighbouring letters collide once kerned? Checks the compiled fonts.

    uv run vendor/fonts/tilo-sans/scripts/pairs.py                # report collisions
    uv run vendor/fonts/tilo-sans/scripts/pairs.py --show 40      # also the 40 tightest pairs
    uv run vendor/fonts/tilo-sans/scripts/pairs.py --text "đĩa Tĩnh"   # check the pairs in a string

Checked letters: every letter Tilo Sans added beyond Instrument Sans (its
Greek and Cyrillic included) plus every accented letter of the Vietnamese
alphabet, Instrument's own included ('Tìm' matters as much as 'Tĩnh').
Each is paired both ways with the neighbours it meets in text: printable
ASCII but the underscore, the Vietnamese letters without a tone (a ă â b c d
đ e ê ... ơ ư, both cases), quotes, dashes and the ellipsis. A Greek or
Cyrillic letter meets only the punctuation and figures among them: letters
of two scripts are shaped in separate runs and never kerned together. Two toned
letters never stand side by side in Vietnamese (a syllable carries one
tone), and a lowercase letter is not followed by a capital inside a word,
so pairs such as 'ÍÌ' or 'íY' are not checked and get no kerning. Pairs are
shaped with HarfBuzz, so GPOS kerning applies, at the eight masters and at
the instances between them (wght 500/550/600/650 at wdth 75 and 87.5, and
500/550/600 at wdth 100). The two inks
are compared as horizontal profiles in 4-unit bands, the left glyph's
widened by the minimum gap in every direction, so a tone mark that comes
near a neighbour's ascender diagonally counts as well as one that touches.

A pair whose inks come closer than MIN_GAP (font units) is a collision.
scripts/kerning.py opens the colliding pairs, by kerning group, to
MIN_GAP + FIX_MARGIN at the masters, so the instances between them stay
clear. build.py runs this check after compiling.
"""

from __future__ import annotations

import argparse
import math
import sys
import unicodedata
from pathlib import Path

import numpy as np
import uharfbuzz as hb
import ufoLib2
from fontTools.pens.basePen import BasePen
from fontTools.ttLib import TTFont

ROOT = Path(__file__).resolve().parent.parent
FONTS = ROOT / "fonts"
SOURCES = ROOT / "sources"
MIN_GAP = 16
FIX_MARGIN = 8  # kerning.py opens pairs to MIN_GAP + this, so instances between masters stay clear
BAND = 4
YMIN, YMAX = -320, 1160

UPRIGHT = ("TiloSans[wdth,wght].ttf", {
    "CondensedRegular": (75, 400), "CondensedBold": (75, 700), "Regular": (100, 400), "Bold": (100, 700),
})
ITALIC = ("TiloSans-Italic[wdth,wght].ttf", {
    "CondensedItalic": (75, 400), "CondensedBoldItalic": (75, 700), "Italic": (100, 400), "BoldItalic": (100, 700),
})
# Instances between the masters, checked but not kerned (kerning interpolates).
BETWEEN = {f"wght{g}-wdth{w:g}": (w, g) for g in (500, 550, 600, 650) for w in (75, 87.5)}
BETWEEN.update({"SemiBold": (100, 600), "Medium": (100, 500), "wght550-wdth100": (100, 550)})

# The Vietnamese alphabet with every tone (Instrument's own accented letters
# included), and the letters without a tone.
VIETNAMESE = (
    "aàáảãạăằắẳẵặâầấẩẫậbcdđeèéẻẽẹêềếểễệghiìíỉĩịklmnoòóỏõọôồốổỗộơờớởỡợpqrstuùúủũụưừứửữựvxyỳýỷỹỵ"
)
UNTONED = "aăâbcdđeêghiklmnoôơpqrstuưvxy"
# Neighbours: ASCII but the underscore, the untoned Vietnamese letters,
# quotes, dashes, the ellipsis.
CONTEXT = (
    "".join(chr(c) for c in range(0x21, 0x7F) if chr(c) != "_")  # a dot below may sit on an underscore
    + UNTONED + UNTONED.upper()
    + "‘’“”«»–—…·"
)


# With ss13 on: the forms that change (every I shape, every l) and the
# neighbours that reach over or under a serif or tail.
DISTINCT = "IÍÌÎÏĪĮİĨỈỊĲlĺľļłΙΪΊІЇ"
DISTINCT_NEIGHBOURS = "ìíîïīĩĭǐỉťďľř¿¡ÍÎÏ0"


class _Spans(BasePen):
    """Flatten an outline into line segments."""

    def __init__(self, glyphset):
        super().__init__(glyphset)
        self.segs = []
        self.cur = None
        self.start = None

    def _moveTo(self, p):
        self.cur = self.start = p

    def _lineTo(self, p):
        self.segs.append((self.cur, p))
        self.cur = p

    def _curveToOne(self, p1, p2, p3):
        p0 = self.cur
        prev = p0
        for i in range(1, 9):
            t = i / 8
            mt = 1 - t
            q = (mt**3 * p0[0] + 3 * mt * mt * t * p1[0] + 3 * mt * t * t * p2[0] + t**3 * p3[0],
                 mt**3 * p0[1] + 3 * mt * mt * t * p1[1] + 3 * mt * t * t * p2[1] + t**3 * p3[1])
            self.segs.append((prev, q))
            prev = q
        self.cur = p3

    def _qCurveToOne(self, p1, p2):
        p0 = self.cur
        prev = p0
        for i in range(1, 9):
            t = i / 8
            mt = 1 - t
            q = (mt * mt * p0[0] + 2 * mt * t * p1[0] + t * t * p2[0], mt * mt * p0[1] + 2 * mt * t * p1[1] + t * t * p2[1])
            self.segs.append((prev, q))
            prev = q
        self.cur = p2

    def _closePath(self):
        if self.cur != self.start:
            self.segs.append((self.cur, self.start))

    _endPath = _closePath


YS = np.arange(YMIN, YMAX, BAND) + BAND / 2


def profiles(glyphset, name):
    """(left, right) ink extents per band: the min and max x of any edge
    crossing the band (nan where the band is empty)."""
    pen = _Spans(glyphset)
    glyphset[name].draw(pen)
    left = np.full(len(YS), np.inf)
    right = np.full(len(YS), -np.inf)
    lo_b = YS - BAND / 2
    hi_b = YS + BAND / 2
    for (x0, y0), (x1, y1) in pen.segs:
        ya, yb = min(y0, y1), max(y0, y1)
        sel = (hi_b >= ya) & (lo_b <= yb)
        if not sel.any():
            continue
        if y1 == y0:
            xs_lo = np.full(sel.sum(), min(x0, x1))
            xs_hi = np.full(sel.sum(), max(x0, x1))
        else:
            ylo = np.clip(lo_b[sel], ya, yb)
            yhi = np.clip(hi_b[sel], ya, yb)
            xa = x0 + (ylo - y0) * (x1 - x0) / (y1 - y0)
            xb = x0 + (yhi - y0) * (x1 - x0) / (y1 - y0)
            xs_lo, xs_hi = np.minimum(xa, xb), np.maximum(xa, xb)
        left[sel] = np.minimum(left[sel], xs_lo)
        right[sel] = np.maximum(right[sel], xs_hi)
    return left, right


def dilate_right(right, d):
    """Right profile grown by a disc of radius d."""
    out = right.copy()
    k = int(d // BAND)
    for j in range(1, k + 1):
        dx = math.sqrt(max(d * d - (j * BAND) ** 2, 0))
        out[j:] = np.maximum(out[j:], right[:-j] + dx)
        out[:-j] = np.maximum(out[:-j], right[j:] + dx)
    return np.maximum(out, right + d)


def source_names(font: TTFont) -> dict[str, str]:
    """Compiled (production) glyph name -> source glyph name, through the cmap."""
    src = ufoLib2.Font.open(SOURCES / "TiloSans-Regular.ufo")
    by_cp = {u: g.name for g in src for u in g.unicodes}
    return {name: by_cp[cp] for cp, name in font.getBestCmap().items() if cp in by_cp}


def added_letters(font: TTFont) -> list[str]:
    """Characters whose glyphs Tilo Sans added beyond Instrument Sans (the
    sources' stored glyph order is upstream's)."""
    src = ufoLib2.Font.open(SOURCES / "TiloSans-Regular.ufo")
    upstream = set(src.lib["public.glyphOrder"])
    by_cp = {u: g.name for g in src for u in g.unicodes}
    return [chr(cp) for cp in sorted(font.getBestCmap())
            if cp in by_cp and by_cp[cp] not in upstream and unicodedata.category(chr(cp))[0] in "LS"]


def shape_pair(hbfont, text, features=None):
    buf = hb.Buffer()
    buf.add_str(text)
    buf.guess_segment_properties()
    hb.shape(hbfont, buf, {"kern": True, "liga": False, "calt": False, **(features or {})})
    return buf.glyph_infos, buf.glyph_positions


def check(font_file: str, locations: dict, pairs: list[str], min_gap: float, show: int = 0, features=None):
    path = FONTS / font_file
    tt = TTFont(path)
    order = tt.getGlyphOrder()
    names = source_names(tt)
    face = hb.Face(hb.Blob.from_file_path(str(path)))
    results = []  # (gap, master, pair, needed)
    for master, (wdth, wght) in locations.items():
        hbfont = hb.Font(face)
        hbfont.set_variations({"wdth": wdth, "wght": wght})
        gs = tt.getGlyphSet(location={"wdth": wdth, "wght": wght})
        cache = {}

        def prof(gid):
            if gid not in cache:
                left, right = profiles(gs, order[gid])
                cache[gid] = (left, right, dilate_right(right, min_gap), dilate_right(right, min_gap + FIX_MARGIN))
            return cache[gid]
        for text in pairs:
            infos, poss = shape_pair(hbfont, text, features)
            if len(infos) != 2:
                continue
            a, b = infos[0].codepoint, infos[1].codepoint
            shift = poss[0].x_advance + poss[1].x_offset - poss[0].x_offset
            kern = poss[0].x_advance - hbfont.get_glyph_h_advance(a)
            la, ra, ra_dil, ra_fix = prof(a)
            lb = prof(b)[0]
            ok = np.isfinite(ra) & np.isfinite(lb)
            if not ok.any():
                continue
            gap = float(np.min(lb[ok] + shift - ra[ok]))
            okd = np.isfinite(ra_dil) & np.isfinite(lb)
            need = float(np.max(ra_dil[okd] - lb[okd] - shift)) if okd.any() else -1
            need_fix = float(np.max(ra_fix[okd] - lb[okd] - shift)) if okd.any() else -1
            results.append((gap, master, text, need, need_fix, names.get(order[a], order[a]), names.get(order[b], order[b]), kern))
    return results


def letter_script(ch: str) -> str | None:
    """The script of a letter (LATIN, GREEK, CYRILLIC, ...); None for anything
    that is not a letter. HarfBuzz shapes the scripts in separate runs, so two
    letters of different scripts are never kerned against each other, while
    punctuation and figures join the run of the letter beside them."""
    script = unicodedata.name(ch, "?").split(" ")[0]
    return script if ch.isalpha() and script in ("LATIN", "GREEK", "CYRILLIC") else None


def kerning_pairs(text: str) -> list[str]:
    return [text[i:i + 2] for i in range(len(text) - 1) if not text[i].isspace() and not text[i + 1].isspace()]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--show", type=int, default=0, help="also print the N tightest pairs")
    parser.add_argument("--text", action="append", default=[], help="check only the pairs in these strings")
    parser.add_argument("--min-gap", type=float, default=MIN_GAP)
    args = parser.parse_args()
    problems = run(args.text, args.min_gap, args.show)
    return 1 if problems else 0


def pair_set(font_file: str) -> list[str]:
    tt = TTFont(FONTS / font_file)
    cmap = tt.getBestCmap()
    # the added letters, and every accented letter of the Vietnamese
    # alphabet: a collision in 'Tìm' is as much a Vietnamese problem as one
    # in 'Tĩnh'
    checked = sorted(set(added_letters(tt)) | {c for c in VIETNAMESE + VIETNAMESE.upper() if ord(c) in cmap and ord(c) > 0x7F})
    ctx = sorted(c for c in set(CONTEXT) if ord(c) in cmap)
    pairs = set()
    for v in checked:
        for c in ctx:
            if letter_script(c) not in (None, letter_script(v)):
                continue  # a Latin letter beside a Greek or Cyrillic one: separate runs, never kerned
            pairs.add(v + c)
            pairs.add(c + v)
    # a lowercase letter is not followed by a capital inside a word
    return sorted(p for p in pairs if not (p[0].islower() and p[1].isupper()))


def distinct_pair_set(font_file: str) -> list[str]:
    """The serif I and the tailed l (ss13), every precomposed form and every
    script's I, beside the context, the lowercase accents and apostrophe
    carons, ¿ ¡ and each other, both ways."""
    cmap = TTFont(FONTS / font_file).getBestCmap()
    forms = [c for c in DISTINCT if ord(c) in cmap]
    ctx = sorted(c for c in set(CONTEXT + DISTINCT_NEIGHBOURS + DISTINCT) if ord(c) in cmap)
    pairs = set()
    for v in forms:
        for c in ctx:
            if letter_script(c) not in (None, letter_script(v)):
                continue
            pairs.add(v + c)
            pairs.add(c + v)
    return sorted(pairs)


def run(texts=(), min_gap=MIN_GAP, show=0, quiet=False, families=("upright", "italic"), between=True) -> list[str]:
    """Check the given families at the masters (and the instances between
    them), with the default forms and with the distinct forms (ss13, zero);
    returns the collisions."""
    problems = []
    for key, (font_file, masters) in (("upright", UPRIGHT), ("italic", ITALIC)):
        if key not in families:
            continue
        locations = dict(masters)
        if between:
            locations.update(BETWEEN)
        pairs = sorted({p for t in texts for p in kerning_pairs(t)}) if texts else pair_set(font_file)
        results = sorted(check(font_file, locations, pairs, min_gap))
        for gap, master, text, need, *_ in results:
            if need > 0.5:
                problems.append(f"{key:7s} {master:20s} {text!r:8s}: gap {gap:.0f}, kern +{math.ceil(need)} to reach {min_gap:g}")
        if not texts:
            # with ss13 on, a pair collides when it comes closer than the
            # minimum and closer than the same pair with ss13 off (a pair the
            # distinct forms do not change, such as '~ĺ', is upstream's)
            distinct = distinct_pair_set(font_file)
            plain = {(m, t): g for g, m, t, *_ in check(font_file, locations, distinct, min_gap)}
            ss13 = sorted(check(font_file, locations, distinct, min_gap, features={"ss13": True, "zero": True}))
            for gap, master, text, need, *_ in ss13:
                if need > 0.5 and gap < plain.get((master, text), math.inf) - 0.5:
                    problems.append(f"{key:7s} {master:20s} {text!r:8s} with ss13: gap {gap:.0f}, kern +{math.ceil(need)}"
                                    f" to reach {min_gap:g} (scripts/il_kern.py owns these pairs)")
        if show and not quiet:
            print(f"-- {font_file}: tightest pairs")
            for gap, master, text, *_ in results[:show]:
                print(f"   {master:20s} {text!r:8s} gap {gap:.0f}")
    if not quiet:
        for line in problems:
            print("COLLIDE " + line)
        print(f"{len(problems)} colliding pair(s) (minimum gap {min_gap:g})")
    return problems


if __name__ == "__main__":
    sys.exit(main())
