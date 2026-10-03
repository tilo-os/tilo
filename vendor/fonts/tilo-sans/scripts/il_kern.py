# /// script
# requires-python = ">=3.11"
# dependencies = ["uharfbuzz>=0.45", "fonttools>=4.60", "ufoLib2", "numpy"]
# ///
"""Kerning for the distinct I and l (ss13), measured from the built fonts.

    uv run vendor/fonts/tilo-sans/build.py
    uv run vendor/fonts/tilo-sans/scripts/il_kern.py           # rewrite the pairs
    uv run vendor/fonts/tilo-sans/build.py                     # and repeat until
    uv run vendor/fonts/tilo-sans/scripts/il_kern.py --check   # it reports none

Every kerning pair with a serif I or a tailed l on either side is regenerated
from scratch at each of the eight masters, from ink gaps taken on the shaped
font (HarfBuzz, with the feature on and off). Nothing in these pairs is typed
by hand; to change one, change a rule here. Run it again whenever a glyph
joins the I/l families or a new letter could sit next to them (new scripts,
new accents): `--check` exits non-zero while any pair is off its rule.

Who owns which pairs:
  left  public.kern1.KO_l.ss13       l.ss13 and the tailed l forms, f_l/f_f_l.ss13
  left  public.kern1.KO_lcaron.ss13  lcaron.ss13
  left  public.kern1.KO_I.ss13       I.ss13 and its accented forms
  right public.kern2.KO_I.ss13       I.ss13, its accented forms, IJ.ss13
  and glyph-level exceptions on their members. Everything else (and the
  left side of l.ss13, which is l's) keeps upstream's kerning.

Terms. "plain" is the same pair with ss13 off (Instrument's own I or l).
The stem band is the height where the serif I is a bare stem (between its
serifs) or the tailed l is a bare stem (above its tail); "extra" is how much
further the stem sits from the next glyph than the plain stem does
(l: the tail's advance; I: the serif's inset).

Rules, at each master:
  stem      the white beside the stem: the stem-band gap is the plain gap plus
            K, where K is 12 (l) / 14 (I) units for most partners; next to
            rounds (O C G Q, o c e d q g, 0 6 8 9) two thirds of "extra" for
            the I and one third for the l (its tail already fills under the
            round); half of it next to shapes that live at the top or middle
            (T V W Y 7, quotes, dashes, guillemets, asterisk, question, bullet,
            middle dot). Tightening stops where a serif would come closer to
            the partner than it sits to H, or the tail closer than it sits to
            n (scaled by plain l's gap to the partner over its gap to n, so a
            round gets proportionally less): tail and serifs space like a stem
            and the stem never hides behind them.
  floors    never closer than these (full height):
            - serif I: 40 units from anything, or the plain I's gap where that
              is already closer (cap accent meeting cap accent); 40 always
              against lowercase accents, apostrophe carons and between the
              accented serif I's themselves (accent against accent);
            - tailed l: never tighter than Instrument's t, or the plain l;
            - I I: Instrument's Z Z; I J, t I, ť I: L Z (baseline bars);
              l I: the larger of L Z and Z Z; ď ľ ť ř before I: 45;
            - Condensed Bold: l l and l I 10 more, so they keep a pixel of
              white at 12-13 px.
  lcaron    lcaron.ss13 kerns as the larger (looser) of the tailed l's value
            and plain ľ's value moved by the advance difference, so the caron
            keeps ľ's clearance and the tail keeps l.ss13's.
  Catalan   l·l (locl CAT): the middle dot sits centred between the two tailed
            l's, as it does between plain ones.

The italic masters are measured on their own outlines.
"""

from __future__ import annotations

import argparse
import math
import sys
import unicodedata
from collections import defaultdict
from multiprocessing import Pool
from pathlib import Path

import numpy as np
import ufoLib2
import uharfbuzz as hb
from fontTools.pens.basePen import BasePen

ROOT = Path(__file__).resolve().parent.parent
SOURCES = ROOT / "sources"
FONTS = ROOT / "fonts"

MASTERS = {  # UFO style -> (italic, wdth, wght)
    "Regular": (False, 100, 400),
    "Bold": (False, 100, 700),
    "CondensedRegular": (False, 75, 400),
    "CondensedBold": (False, 75, 700),
    "Italic": (True, 100, 400),
    "BoldItalic": (True, 100, 700),
    "CondensedItalic": (True, 75, 400),
    "CondensedBoldItalic": (True, 75, 700),
}

L_CLASS = "public.kern1.KO_l.ss13"
LC_CLASS = "public.kern1.KO_lcaron.ss13"
I1_CLASS = "public.kern1.KO_I.ss13"
I2_CLASS = "public.kern2.KO_I.ss13"

CAP = 720
K_L = 12
K_I = 14
FLOOR = 40
CARON = 45
ABS_FLOOR = 24  # nothing closer, even where the plain letter collides: pairs.py's 16 and room between masters
CONDENSED_BOLD_EXTRA = 10
ROUND_SHARE_L = 1 / 3  # the tail already fills under a round: keep less of the extra white
ROUND_SHARE_I = 1 / 3  # the serifs sit where a round curves away: the stem keeps a third of its extra white
TOLERANCE = 2  # a member may sit this much inside a floor before it gets its own pair
DIAGONAL = 0.9  # an approach is diagonal when the distance is shorter than this share of the level gap

# Left forms (text, features) of each family; the first is the base.
L_FORMS = ["l", "ĺ", "ļ", "ł", "fl", "ffl"]
I_FORMS = ["I", "Í", "Ì", "Î", "Ï", "Ī", "Į", "İ", ("Ï", "ss01"), ("İ", "ss01"),
           "Ι", "Ϊ", ("Ϊ", "ss01"), "Ί", "І", "Ї"]  # Greek and Cyrillic: the same serif I
I_RIGHT = [f for f in I_FORMS if f != "Ί"] + ["Ĳ"]  # Ί's left side is its tonos
# The letter whose bars meet neighbours the way the serifs do, per script.
ANALOGUE = {"LATIN": "Z", "GREEK": "Ζ", "CYRILLIC": "Е"}
# Glyphs that copy another glyph's left-side pairs: Ί's tonos is Ί's.
COPY_LEFT = {"Iotatonos.ss13": "Iotatonos"}
BARE_I = {"I.ss13", "Iota.ss13", "I-cy.ss13"}  # the serif I without an accent, in each script
# The tailed l's forms on the right (their left side is plain l's) and the
# lowercase accents that reach over their ascender.
L_RIGHT_TEXT = ["l", "ĺ", "ľ", "ļ", "ł"]
L_RIGHT = {"l.ss13", "lacute.ss13", "lcaron.ss13", "lcommaaccent.ss13", "lcommaaccent.ss01.ss13", "lslash.ss13"}
ACCENTS = {"igrave", "iacute", "icircumflex", "idieresis", "idieresis.ss01", "imacron", "itilde", "ibreve",
           "icaron", "ihookabove", "tcaron", "dcaron", "rcaron"}
# Lowercase accents and apostrophe carons that sit at the serif's height.
FORCED = set("ìíîïīĩĭǐỉťďľř")
# The bare vertical strokes the distinct forms must stay apart from (the
# letters i j l I have their own rules): next to these the serif I and the
# tailed l keep the plain letter's distance, in any direction, up to
# CONFUSABLE_GAP (a clear pixel and a quarter at 12 px).
CONFUSABLE = set("1|¦!¡")
CONFUSABLE_GAP = 104

# Partners by the glyph they become (kern groups decide the rest).
ROUND_LEFT = {"O", "C", "G", "Q", "o", "c", "e", "d", "q", "g", "zero", "zero.zero", "six", "eight", "nine",
              "Oslash", "oslash", "Omicron", "Theta", "Omega", "omicron", "sigma", "alpha", "theta", "phi",
              "O-cy", "Es-cy", "E-cy", "Ef-cy", "o-cy", "es-cy", "e-cy", "ie-cy", "ef-cy"}
ROUND_RIGHT = {"O", "C", "D", "Q", "o", "b", "p", "c", "e", "zero", "zero.zero", "six", "eight", "nine",
               "Oslash", "oslash", "Omicron", "Theta", "Omega", "omicron", "rho", "phi", "theta",
               "O-cy", "Ereversed-cy", "Ef-cy", "Yu-cy", "o-cy", "ereversed-cy", "er-cy", "ef-cy", "yu-cy",
               "be-cy", "ie-cy"}
MID = {"T", "V", "W", "Y", "seven", "quotedbl", "quotesingle", "quotedblleft", "quotedblright",
       "quoteleft", "quoteright", "quotedblleft.ss01", "quotedblright.ss01", "quoteleft.ss01",
       "quoteright.ss01", "hyphen", "endash", "emdash", "guillemetleft", "guillemetright",
       "guilsinglleft", "guilsinglright", "asterisk", "question", "question.ss01", "bullet",
       "periodcentered", "backslash"}


# --------------------------------------------------------------------------
# measuring

class FlatPen(BasePen):
    def __init__(self):
        super().__init__(None)
        self.segs = []
        self.cur = self.start = None

    def _moveTo(self, p):
        self.cur = self.start = p

    def _lineTo(self, p):
        self.segs.append((*self.cur, *p))
        self.cur = p

    def _curveToOne(self, a, b, c):
        p0 = self.cur
        for i in range(1, 17):
            t = i / 16
            u = 1 - t
            p = (u**3 * p0[0] + 3 * u * u * t * a[0] + 3 * u * t * t * b[0] + t**3 * c[0],
                 u**3 * p0[1] + 3 * u * u * t * a[1] + 3 * u * t * t * b[1] + t**3 * c[1])
            self.segs.append((*self.cur, *p))
            self.cur = p

    def _qCurveToOne(self, a, b):
        p0 = self.cur
        for i in range(1, 13):
            t = i / 12
            u = 1 - t
            p = (u * u * p0[0] + 2 * u * t * a[0] + t * t * b[0], u * u * p0[1] + 2 * u * t * a[1] + t * t * b[1])
            self.segs.append((*self.cur, *p))
            self.cur = p

    def _closePath(self):
        if self.cur is not None and self.cur != self.start:
            self.segs.append((*self.cur, *self.start))
        self.cur = None

    _endPath = _closePath


YS = np.arange(-400, 1101, 2, dtype=float)
REACH = 240  # diagonal distances are looked for this far above and below
ITALIC_ANGLE = 13


class Shaper:
    def __init__(self, italic, wdth, wght):
        path = FONTS / ("TiloSans-Italic[wdth,wght].ttf" if italic else "TiloSans[wdth,wght].ttf")
        self.face = hb.Face(path.read_bytes())
        self.font = hb.Font(self.face)
        self.font.set_variations({"wdth": wdth, "wght": wght})
        self.profiles = {}
        self.upright = {}
        self.skew = math.tan(math.radians(ITALIC_ANGLE)) if italic else 0.0
        self.names = {}  # production name in the binary -> UFO name
        self.path = path

    def face_cmap(self) -> dict[int, str]:
        from fontTools.ttLib import TTFont
        return TTFont(self.path, lazy=True).getBestCmap()

    def profile(self, gid):
        """(left, right) ink edge per scanline in YS, nan where empty. In the
        italic, measured with the slant taken out (as Glyphs shows italic
        sidebearings), so a slanted stem's white compares with the upright's."""
        if gid not in self.upright:
            left, right = self.raw(gid)
            shift = YS * self.skew
            self.upright[gid] = (left - shift, right - shift)
        return self.upright[gid]

    def raw(self, gid):
        """(left, right) ink edge per scanline of the outline as drawn."""
        if gid not in self.profiles:
            pen = FlatPen()
            self.font.draw_glyph_with_pen(gid, pen)
            left = np.full(len(YS), np.nan)
            right = np.full(len(YS), np.nan)
            if pen.segs:
                s = np.array(pen.segs)
                x0, y0, x1, y1 = s[:, 0], s[:, 1], s[:, 2], s[:, 3]
                Y = YS[:, None]
                hit = ((y0 <= Y) & (Y < y1)) | ((y1 <= Y) & (Y < y0))
                dy = np.where(y1 == y0, 1, y1 - y0)
                X = x0 + (Y - y0) / dy * (x1 - x0)
                left = np.where(hit, X, np.inf).min(axis=1)
                right = np.where(hit, X, -np.inf).max(axis=1)
                left[~hit.any(axis=1)] = np.nan
                right[~hit.any(axis=1)] = np.nan
            self.profiles[gid] = (left, right)
        return self.profiles[gid]


    def shape(self, text, feats, lang=None):
        buf = hb.Buffer()
        buf.add_str(text)
        buf.guess_segment_properties()
        if lang:
            buf.language = lang
        hb.shape(self.font, buf, feats)
        out = []
        x = 0
        for info, pos in zip(buf.glyph_infos, buf.glyph_positions):
            name = self.font.glyph_to_string(info.codepoint)
            out.append((self.names.get(name, name), info.codepoint, x + pos.x_offset, pos.x_advance))
            x += pos.x_advance
        return out

    def gap(self, glyphs, i=0, band_a=None, band_b=None, raw=False):
        """Ink gap between glyphs i and i+1; bands are (lo, hi) on each glyph."""
        _, ga, xa, _ = glyphs[i]
        _, gb, xb, _ = glyphs[i + 1]
        ra = (self.raw if raw else self.profile)(ga)[1] + xa
        lb = (self.raw if raw else self.profile)(gb)[0] + xb
        m = ~np.isnan(ra) & ~np.isnan(lb)
        if band_a:
            m &= (YS >= band_a[0]) & (YS <= band_a[1])
        if band_b:
            m &= (YS >= band_b[0]) & (YS <= band_b[1])
        if not m.any():
            return None
        return float((lb - ra)[m].min())

    def _facing(self, glyphs, i, band_a, band_b, reach):
        """(dx, dy) between every scanline of the left glyph's right edge and
        every scanline of the right glyph's left edge within `reach` of it."""
        _, ga, xa, _ = glyphs[i]
        _, gb, xb, _ = glyphs[i + 1]
        ra = self.raw(ga)[1] + xa
        lb = self.raw(gb)[0] + xb
        ma = ~np.isnan(ra)
        mb = ~np.isnan(lb)
        if band_a:
            ma &= (YS >= band_a[0]) & (YS <= band_a[1])
        if band_b:
            mb &= (YS >= band_b[0]) & (YS <= band_b[1])
        ia = np.flatnonzero(ma)[::2]  # 4-unit scanlines: plenty against gaps of 40+
        ib = np.flatnonzero(mb)[::2]
        if not len(ia) or not len(ib):
            return None
        dy = YS[ib][None, :] - YS[ia][:, None]
        dx = lb[ib][None, :] - ra[ia][:, None]
        near = np.abs(dy) <= reach
        if not near.any():
            return None
        return dx[near], dy[near]

    def dist(self, glyphs, i=0, band_a=None, band_b=None, reach=REACH, shift=0.0):
        """Shortest ink distance between glyphs i and i+1, in any direction
        (with the right glyph moved `shift` units further).

        Each scanline of the left glyph's right edge is compared with every
        scanline of the right glyph's left edge up to `reach` units above or
        below it, so a serif tip that comes near a figure's flag, a bowl or an
        accent diagonally counts, not only one level with it. Where the two
        edges overlap horizontally on different scanlines, the vertical
        distance between those scanlines is the gap (an accent under a serif).
        """
        f = self._facing(glyphs, i, band_a, band_b, reach)
        if f is None:
            return None
        dx, dy = f
        return float(np.sqrt(np.maximum(dx + shift, 0) ** 2 + dy ** 2).min())

    def need_shift(self, glyphs, need, i=0, band_a=None, band_b=None):
        """How far the right glyph must move (negative: may move closer) for
        dist() to be exactly `need`: solved, not stepped, so a diagonal
        approach (where moving sideways gains less than its length) settles
        in one pass."""
        f = self._facing(glyphs, i, band_a, band_b, max(need, 1))
        if f is None:
            return None
        dx, dy = f
        close = np.abs(dy) < need
        if not close.any():
            return None
        return float((np.sqrt(need ** 2 - dy[close] ** 2) - dx[close]).max())

    def kern(self, text, feats, i=0, lang=None):
        a = self.shape(text, feats, lang)
        b = self.shape(text, {**feats, "kern": False}, lang)
        return a[i][3] - b[i][3]


# --------------------------------------------------------------------------
# kerning data


class Kerning:
    def __init__(self, font):
        self.pairs = dict(font.kerning)
        self.g1 = {}
        self.g2 = {}
        for name, members in font.groups.items():
            for m in members:
                if name.startswith("public.kern1."):
                    self.g1[m] = name
                elif name.startswith("public.kern2."):
                    self.g2[m] = name

    def key1(self, g):
        return self.g1.get(g, g)

    def key2(self, g):
        return self.g2.get(g, g)

    def get(self, a, b):
        for k in ((a, b), (a, self.key2(b)), (self.key1(a), b), (self.key1(a), self.key2(b))):
            if k in self.pairs:
                return self.pairs[k]
        return 0


def ensure_groups(font):
    """lcaron.ss13 kerns on its own: out of KO_dcaron, into KO_lcaron.ss13."""
    g = font.groups
    dc = "public.kern1.KO_dcaron"
    if "lcaron.ss13" in g.get(dc, []):
        g[dc] = [m for m in g[dc] if m != "lcaron.ss13"]
    if g.get(LC_CLASS) != ["lcaron.ss13"]:
        g[LC_CLASS] = ["lcaron.ss13"]


def even_up(v):
    return int(math.ceil(v / 2.0) * 2)


def even(v):
    return int(round(v / 2.0) * 2)


# --------------------------------------------------------------------------
# solving one master


def script_of(c):
    """LATIN, GREEK or CYRILLIC for a letter, None for anything else
    (figures, punctuation: they join the run of the letter beside them)."""
    if not c.isalpha():
        return None
    script = unicodedata.name(c, "?").split(" ")[0]
    return script if script in ("GREEK", "CYRILLIC") else "LATIN"


def same_run(a, b):
    """HarfBuzz shapes letters of two scripts in separate runs, which are
    never kerned: such pairs are not measured."""
    sa, sb = script_of(a[0]), script_of(b[0])
    return sa is None or sb is None or sa == sb


def text_of(form):
    if isinstance(form, tuple):
        return form[0], {form[1]: True}
    return form, {}


def solve(style, write=False):
    italic, wdth, wght = MASTERS[style]
    font = ufoLib2.Font.open(SOURCES / f"TiloSans-{style}.ufo")
    ensure_groups(font)
    K = Kerning(font)
    sh = Shaper(italic, wdth, wght)
    ps = font.lib.get("public.postscriptNames", {})
    for n in font.keys():
        base, dot, suffix = n.partition(".")
        prod = ps.get(n) or (ps.get(base, base) + dot + suffix)
        sh.names[prod] = n
    # glyphs added since the import have no stored production name (build.py
    # derives it at compile time): read it from the binary's cmap instead
    by_cp = {u: g.name for g in font for u in g.unicodes}
    for u, prod in sh.face_cmap().items():
        if u in by_cp:
            sh.names[prod] = by_cp[u]
            for n in font.keys():
                if n.startswith(by_cp[u] + "."):
                    sh.names[prod + n[len(by_cp[u]):]] = n
    SS = {"ss13": True}
    I1_SET = set(font.groups.get(I1_CLASS, []))
    I2_SET = set(font.groups.get(I2_CLASS, []))
    cond_bold = wdth == 75 and wght == 700

    # partners: every Latin-ish character the font maps, plus the alternates
    # ss01 and zero reach (the round-dot quotes and question, the slashed zero)
    chars = []
    for u in sorted(sh.face.unicodes):
        c = chr(u)
        cat = unicodedata.category(c)
        if cat[0] not in "LNPS" or cat in ("So", "Lm") or u > 0x2BFF:
            continue
        if 0x0370 <= u <= 0x1DFF and script_of(c) not in ("GREEK", "CYRILLIC"):
            continue
        chars.append(c)
    partners = []
    seen = set()
    for c in chars:
        for feats in ({}, {"ss01": True}, {"zero": True}):
            g = sh.shape(c, feats)
            if len(g) != 1:
                continue
            name = g[0][0]
            if name in seen:
                continue
            seen.add(name)
            partners.append((c, feats, name))

    def name_of(text, feats):
        return sh.shape(text, {**SS, **feats})[0][0]

    # geometry of this master
    def ink_rows(gname):
        gid = sh.font.get_glyph_from_name(gname)
        return sh.profile(gid)

    left_i, right_i = ink_rows("I.ss13")
    rows = ~np.isnan(right_i) & (YS >= 0) & (YS <= CAP)
    width = right_i - left_i
    stem_w = np.nanmin(np.where(rows, width, np.nan))
    serif_rows = rows & (width > stem_w + 4)
    serif_h = float(YS[serif_rows & (YS < CAP / 2)].max())
    I_STEM = (serif_h + 12, CAP - serif_h - 12)
    I_SERIF_LO = (-400, serif_h + 12)
    I_SERIF_HI = (CAP - serif_h - 12, CAP)
    l_left, l_right = ink_rows("l.ss13")
    # the stem's right edge as a line (slanted in the italic), fitted high up
    hi = (YS >= 400) & (YS <= 680) & ~np.isnan(l_right)
    slope, icept = np.polyfit(YS[hi], l_right[hi], 1)
    beyond = (l_right - (slope * YS + icept)) > 2
    tail_top = float(YS[beyond & (YS < 400)].max())
    L_STEM = (tail_top + 10, 2000)
    L_TAIL = (-400, tail_top + 10)

    def g_full(text, feats, **kw):
        gl = sh.shape(text, feats)
        return sh.gap(gl, 0, **kw)

    def d_full(text, feats, **kw):
        gl = sh.shape(text, feats)
        return sh.dist(gl, 0, **kw) if len(gl) == 2 else None

    def nearest(*ds):
        ds = [d for d in ds if d is not None]
        return min(ds) if ds else None

    def diagonal_need(gl, plain_d, z_d):
        """The serif I's floor against a partner. Where its nearest approach is
        diagonal (a serif tip towards the flag of 1, the arm of 4, the bowl
        of ¿, an accent), the serif keeps the distance Z's bars keep from the
        same partner, or the plain I's where that is closer: Z is the
        letter whose bars meet neighbours the way serifs do. Side by side,
        FLOOR (or the plain I's distance, where closer); the stem rule spaces
        those. Which it is gets decided with the pair set 100 units apart
        level, so the answer does not depend on the kerning being solved."""
        level = None if plain_d is None else min(FLOOR, plain_d)
        full = sh.gap(gl, raw=True)
        if full is None or sh.dist(gl, shift=100 - full) < 100 * DIAGONAL:
            return max(level or 0, nearest(plain_d, z_d) or 0) or None
        return level

    def floor_d(k0, gl, need, **bands):
        """The kerning at which the pair's inks are `need` apart, any direction."""
        if need is None:
            return -9999
        s = sh.need_shift(gl, need, **bands)
        return -9999 if s is None else k0 + s

    LZ = g_full("LZ", {}, band_a=(-400, 200))
    ZZ = g_full("ZZ", {})
    adv = {n: font[n].width for n in ("l", "l.ss13", "lcaron", "lcaron.ss13", "I", "I.ss13")}
    extra_l = adv["l.ss13"] - adv["l"]
    # how much further the serif I's stem sits than the plain I's, per side
    extra_i_r = (g_full("IH", SS, band_a=I_STEM) or 0) - (g_full("IH", {}) or 0)
    extra_i_l = (g_full("HI", SS, band_b=I_STEM) or 0) - (g_full("HI", {}) or 0)
    tail_ref = g_full("ln", SS, band_a=L_TAIL)
    plain_ln = g_full("ln", {})
    serif_ref_r = min(g_full("IH", SS, band_a=I_SERIF_LO), g_full("IH", SS, band_a=I_SERIF_HI))
    serif_ref_l = min(g_full("HI", SS, band_b=I_SERIF_LO), g_full("HI", SS, band_b=I_SERIF_HI))

    # wants[(left glyph, right glyph)] = (value, floor, reason)
    wants = {}

    def stem_rule(k0, plain, stem, extra, kind, base_k, band_gap, ref, round_share=None):
        if stem is None or plain is None:
            return k0, "keep"
        if kind == "round":
            k_add, why = round_share * extra, "round"
        elif kind == "mid":
            k_add, why = extra / 2, "mid"
        else:
            k_add, why = base_k, "stem"
        target = k0 + (plain + k_add - stem)
        if target >= k0:
            return target, why
        if band_gap is not None and ref is not None:
            if band_gap - ref < (k0 - target):
                return min(k0, k0 - max(0.0, band_gap - ref)), why + "/tail-or-serif"
        return target, why

    def floor_k(k0, g, need):
        if g is None or need is None:
            return -9999
        return k0 + (need - g)

    def record(lg, rg, value, floor, why):
        value = max(value, floor)
        key = (lg, rg)
        if key in wants:
            ov, of, ow = wants[key]
            if floor > of or value > ov:
                wants[key] = (max(ov, value), max(of, floor), ow + "+" + why)
            return
        wants[key] = (value, floor, why)

    def kind_of(name, side):
        if name in MID:
            return "mid"
        base = name.split(".")[0] if name not in ROUND_LEFT | ROUND_RIGHT else name
        if (side == "left" and (name in ROUND_LEFT or base in ROUND_LEFT)) or \
           (side == "right" and (name in ROUND_RIGHT or base in ROUND_RIGHT)):
            return "round"
        return "other"

    def base_letter(name):
        return name.split(".")[0]

    # --- tailed l (and serif I) on the left -----------------------------
    for form in L_FORMS + I_FORMS:
        ftext, ffeats = text_of(form)
        is_l = form in L_FORMS
        for (c, feats, pname) in partners:
            f = {**feats, **ffeats}
            if not same_run(ftext, c):
                continue
            text = ftext + c
            gl = sh.shape(text, {**SS, **f})
            if len(gl) != 2 or len(sh.shape(text, f)) != 2:
                continue
            lname, rname = gl[0][0], gl[1][0]
            n_left = len(sh.shape(ftext, {**SS, **f}))
            if n_left != 1:
                continue
            k0 = sh.kern(text, {**SS, **f})
            full = sh.gap(gl)
            pl = sh.shape(text, f)
            plain = sh.gap(pl)
            if full is None:
                continue
            diag = sh.dist(gl)
            plain_d = sh.dist(pl)
            if is_l:
                stem = sh.gap(gl, band_a=L_STEM)
                band = sh.gap(gl, band_a=L_TAIL)
                kind = kind_of(rname if not rname.endswith(".ss13") else base_letter(rname), "left")
                # the tail keeps the white a stem would keep from this partner:
                # tail_ref next to straights, proportionally less next to rounds
                ref = tail_ref * min(1.0, plain / plain_ln) if plain else tail_ref
                value, why = stem_rule(k0, plain, stem, extra_l, kind, K_L, band, ref, ROUND_SHARE_L)
                need = nearest(d_full("t" + c, f), plain_d)
                if c in FORCED:
                    need = max(need or 0, FLOOR)
                if c in CONFUSABLE:
                    need = max(need or 0, min(plain_d or 0, CONFUSABLE_GAP))
                fl = floor_d(k0, gl, max(need or 0, ABS_FLOOR))
                if rname in I2_SET:
                    need = max(LZ, ZZ) + (CONDENSED_BOLD_EXTRA if cond_bold else 0)
                    fl = max(fl, floor_k(k0, sh.gap(gl, band_a=(-400, 200)), need))
                    fl = max(fl, floor_d(k0, gl, FLOOR if plain_d is None else min(FLOOR, plain_d)))
                if rname == "l.ss13" and cond_bold:
                    fl = max(fl, floor_k(k0, full, tail_ref + CONDENSED_BOLD_EXTRA))
                record(lname, rname, value, fl, why)
            else:
                stem = sh.gap(gl, band_a=I_STEM)
                band = min([x for x in (sh.gap(gl, band_a=I_SERIF_LO), sh.gap(gl, band_a=I_SERIF_HI)) if x is not None], default=None)
                kind = kind_of(rname, "left")
                value, why = stem_rule(k0, plain, stem, extra_i_r, kind, K_I, band, serif_ref_r, ROUND_SHARE_I)
                accent_pair = rname in I2_SET
                need = diagonal_need(gl, plain_d, d_full(ANALOGUE[script_of(ftext) or "LATIN"] + c, f))
                if c in FORCED or accent_pair or rname == "IJ.ss13" or need is None:
                    need = max(need or 0, FLOOR)
                if c in CONFUSABLE:
                    need = max(need, min(plain_d or 0, CONFUSABLE_GAP))
                fl = floor_d(k0, gl, max(need or 0, ABS_FLOOR))
                if rname in BARE_I:
                    fl = max(fl, floor_k(k0, sh.gap(gl, band_a=(-400, CAP), band_b=(-400, CAP)), ZZ))
                if base_letter(rname) in ("J", "Jcircumflex"):
                    fl = max(fl, floor_k(k0, full, LZ))
                record(lname, rname, value, fl, why)

    # --- serif I on the right -------------------------------------------
    left_partners = partners + [(c, {}, None) for c in ("ľ",)]
    for form in I_RIGHT:
        ftext, ffeats = text_of(form)
        for (c, feats, _) in left_partners:
            f = {**feats, **ffeats}
            if not same_run(c, ftext):
                continue
            text = c + ftext
            gl = sh.shape(text, {**SS, **f})
            if len(gl) != 2 or len(sh.shape(text, f)) != 2:
                continue
            lname, rname = gl[0][0], gl[1][0]
            if lname in I1_SET:
                continue  # serif I against serif I: done above
            k0 = sh.kern(text, {**SS, **f})
            full = sh.gap(gl)
            pl = sh.shape(text, f)
            plain = sh.gap(pl)
            if full is None:
                continue
            diag = sh.dist(gl)
            plain_d = sh.dist(pl)
            stem = sh.gap(gl, band_b=I_STEM)
            band = min([x for x in (sh.gap(gl, band_b=I_SERIF_LO), sh.gap(gl, band_b=I_SERIF_HI)) if x is not None], default=None)
            kind = kind_of(lname, "right")
            value, why = stem_rule(k0, plain, stem, extra_i_l, kind, K_I, band, serif_ref_l, ROUND_SHARE_I)
            need = diagonal_need(gl, plain_d, d_full(c + ANALOGUE[script_of(ftext) or "LATIN"], f))
            if c in FORCED or need is None:
                need = max(need or 0, FLOOR)
            if c in CONFUSABLE:
                need = max(need, min(plain_d or 0, CONFUSABLE_GAP))
            fl = floor_d(k0, gl, max(need or 0, ABS_FLOOR))
            if c in "tť":
                fl = max(fl, floor_k(k0, sh.gap(gl, band_a=(-400, 200)), LZ))
            if lname in ("l.ss13", "lacute.ss13", "lcommaaccent.ss13", "lslash.ss13", "f_l.ss13", "f_f_l.ss13", "lcaron.ss13", "lcommaaccent.ss01.ss13"):
                need = max(LZ, ZZ) + (CONDENSED_BOLD_EXTRA if cond_bold else 0)
                fl = max(fl, floor_k(k0, sh.gap(gl, band_a=(-400, 200)), need))
            if c in "ďřťľ":
                fl = max(fl, floor_d(k0, gl, CARON))
            record(lname, rname, value, fl, why)

    # --- Catalan middle dot ------------------------------------------------
    gl = sh.shape("l·l", SS, "ca")
    if gl[1][0].startswith("periodcentered.loclCAT"):
        k0 = sh.kern("l·l", SS, 0, "ca")
        g1 = sh.gap(gl, 0)
        g2 = sh.gap(gl, 1)
        record("l.ss13", gl[1][0], k0 - (g1 - g2) / 1.0, -9999, "catalan")

    # --------------------------------------------------------------------
    # collapse onto kerning keys
    new = {}
    L_MEMBERS = set(font.groups.get(L_CLASS, []))
    I1_MEMBERS = set(font.groups.get(I1_CLASS, []))
    I2_MEMBERS = set(font.groups.get(I2_CLASS, []))
    k1 = lambda g: L_CLASS if g in L_MEMBERS else I1_CLASS if g in I1_MEMBERS else K.key1(g)
    k2 = lambda g: I2_CLASS if g in I2_MEMBERS else K.key2(g)
    by_key = defaultdict(list)
    for (lg, rg), (v, fl, why) in wants.items():
        by_key[(k1(lg), k2(rg))].append((lg, rg, v, fl, why))
    BASES = {"l.ss13", "I.ss13"}
    first_partner = {}
    for (c, feats, name) in partners:
        first_partner.setdefault(K.key2(name), name)
        first_partner.setdefault(K.key1(name), name)
    for (ka, kb), items in by_key.items():
        def score(it):
            lg, rg = it[0], it[1]
            left_base = lg in BASES or lg == ka or first_partner.get(ka) == lg
            right_base = rg in BASES or rg == kb or first_partner.get(kb) == rg
            return (not left_base, not right_base)
        items.sort(key=score)
        base = items[0]
        v = even(base[2])
        new[(ka, kb)] = v
        # members whose floor the group value breaks get exceptions: per right
        # glyph when every left member needs it, per left glyph when it needs
        # it against every right member, else glyph against glyph
        needs = {(lg, rg): even_up(fl) for lg, rg, _, fl, _ in items[1:] if fl > v + TOLERANCE}
        if not needs:
            continue
        lefts = defaultdict(set)
        rights = defaultdict(set)
        for lg, rg, *_ in items:
            lefts[rg].add(lg)
            rights[lg].add(rg)
        done = set()
        for rg in {r for _, r in needs}:
            if rg != kb and all((lg, rg) in needs for lg in lefts[rg]):
                new[(ka, rg)] = max(needs[(lg, rg)] for lg in lefts[rg])
                done |= {(lg, rg) for lg in lefts[rg]}
        for lg in {l for l, _ in needs}:
            if lg != ka and all((lg, rg) in needs or (lg, rg) in done for rg in rights[lg]):
                new[(lg, kb)] = max(needs.get((lg, rg), new.get((ka, rg), v)) for rg in rights[lg])
                done |= {(lg, rg) for rg in rights[lg]}
        for (lg, rg), n in needs.items():
            if (lg, rg) not in done:
                new[(lg, rg)] = n

    # lcaron.ss13: the looser of the tailed l and plain ľ (moved by the advance)
    kl = Kerning(font)
    kl.pairs = {k: v for k, v in kl.pairs.items() if not owned(k, font)}
    kl.pairs.update(new)
    right_keys = set()
    for (a, b) in list(kl.pairs):
        if a in (L_CLASS, "public.kern1.KO_dcaron", "lcaron", "l.ss13") or a in L_MEMBERS:
            right_keys.add(b)
    d_body = adv["l.ss13"] - adv["lcaron.ss13"]
    d_caron = adv["lcaron"] - adv["lcaron.ss13"]
    for b in sorted(right_keys):
        if b == I2_CLASS or b in I2_MEMBERS:
            continue  # measured above
        rep = font.groups[b][0] if b.startswith("public.kern2.") and font.groups.get(b) else b
        if rep not in font:
            continue
        new[(LC_CLASS, b)] = max(kl.get("l.ss13", rep) + d_body, kl.get("lcaron", rep) + d_caron)

    # ...and its caron keeps FLOOR from anything, in any direction (plain ľ's
    # own clearance is negative against T Y 7 and the i accents)
    lc_need = {}
    for (c, feats, _) in partners:
        gl = sh.shape("ľ" + c, {**SS, **feats})
        if len(gl) != 2 or gl[0][0] != "lcaron.ss13" or gl[1][0] in I2_MEMBERS:
            continue
        dc = sh.dist(gl, band_a=(CAP - 200, 2000))
        if dc is None:
            continue
        k0 = sh.kern("ľ" + c, {**SS, **feats})
        rg = gl[1][0]
        cur = new.get((LC_CLASS, rg), new.get((LC_CLASS, K.key2(rg)), 0))
        need = floor_d(k0, gl, FLOOR, band_a=(CAP - 200, 2000))
        if need > cur + TOLERANCE:
            lc_need[rg] = even_up(need)
    lc_groups = defaultdict(list)
    for rg in lc_need:
        lc_groups[K.key2(rg)].append(rg)
    for b, members in lc_groups.items():
        listed = [p[2] for p in partners if K.key2(p[2]) == b and p[2] not in I2_MEMBERS]
        group = b != members[0] and set(members) >= set(listed) and len(listed) > 1
        if group:
            new[(LC_CLASS, b)] = max(lc_need[m] for m in members)
        for m in members:
            # a member's own pair (from the derivation above) overrides the
            # group's: raise it too
            if not group or (LC_CLASS, m) in new:
                new[(LC_CLASS, m)] = max(new.get((LC_CLASS, m), -9999), lc_need[m])

    # the left side of the tailed l is plain l's, but its pairs with the i
    # accents are owned here: an accent keeps FLOOR from the ascender
    kl = Kerning(font)
    kl.pairs = {k: v for k, v in kl.pairs.items() if not owned(k, font)}
    for c in sorted(FORCED - {"ľ"}):
        for feats in ({}, {"ss01": True}):
            for form in L_RIGHT_TEXT:
                text = c + form
                gl = sh.shape(text, {**SS, **feats})
                if len(gl) != 2 or gl[1][0] not in L_RIGHT or gl[0][0] not in ACCENTS:
                    continue
                diag = sh.dist(gl)
                if diag is None:
                    continue
                need = floor_d(sh.kern(text, {**SS, **feats}), gl, FLOOR)
                lg, rg = gl[0][0], gl[1][0]
                if need > kl.get(lg, rg) + TOLERANCE:
                    new[(lg, rg)] = even_up(need)

    # Ί's left side is plain Ί's tonos: its pairs are copied
    for (a, b), v in kl.pairs.items():
        for dst, src in COPY_LEFT.items():
            if b == src and dst in font:
                new[(a, dst)] = v

    # drop zeros that override nothing
    cleaned = {}
    for (a, b), v in new.items():
        if v == 0:
            parent = 0
            for pk in ((a, k2_parent(b, font)), (k1_parent(a, font), b), (k1_parent(a, font), k2_parent(b, font))):
                if pk in new and pk != (a, b):
                    parent = new[pk]
                    break
            if parent == 0:
                continue
        cleaned[(a, b)] = v

    old = {k: v for k, v in font.kerning.items() if owned(k, font)}
    changes = []
    for k in sorted(set(old) | set(cleaned)):
        if old.get(k) != cleaned.get(k):
            changes.append((k, old.get(k), cleaned.get(k)))
    reasons = {}
    for (ka, kb), items in by_key.items():
        reasons[(ka, kb)] = items[0][4]
    for k in old:
        del font.kerning[k]
    font.kerning.update(cleaned)
    info = dict(LZ=LZ, ZZ=ZZ, extra_l=extra_l, extra_i=(round(extra_i_l), round(extra_i_r)),
                tail_top=tail_top, serif_h=serif_h, tail_ref=round(tail_ref),
                serif_ref=(round(serif_ref_l), round(serif_ref_r)))
    if write:
        font.save()
    return style, changes, reasons, info


def k1_parent(a, font):
    for name, members in font.groups.items():
        if name.startswith("public.kern1.") and a in members:
            return name
    return a


def k2_parent(b, font):
    for name, members in font.groups.items():
        if name.startswith("public.kern2.") and b in members:
            return name
    return b


def owned(key, font):
    a, b = key
    fam_l = set(font.groups.get(L_CLASS, [])) | set(font.groups.get(I1_CLASS, [])) | {"lcaron.ss13"}
    fam_r = set(font.groups.get(I2_CLASS, []))
    return (a in (L_CLASS, LC_CLASS, I1_CLASS) or a in fam_l or b == I2_CLASS or b in fam_r
            or (a in ACCENTS and b in L_RIGHT) or b in COPY_LEFT)


def stale():
    newest = max(p.stat().st_mtime for p in SOURCES.rglob("*") if p.is_file())
    for f in ("TiloSans[wdth,wght].ttf", "TiloSans-Italic[wdth,wght].ttf"):
        p = FONTS / f
        if not p.exists() or p.stat().st_mtime < newest:
            return f
    return None


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--check", action="store_true", help="report, write nothing, exit 1 if any pair is off")
    ap.add_argument("--master", action="append", choices=list(MASTERS), help="only these masters")
    ap.add_argument("-v", "--verbose", action="store_true", help="print every change")
    a = ap.parse_args()
    s = stale()
    if s:
        sys.exit(f"fonts/{s} is older than the sources: run build.py first")
    todo = a.master or list(MASTERS)
    with Pool(len(todo)) as pool:
        results = pool.starmap(solve, [(m, not a.check) for m in todo])
    total = 0
    for style, changes, reasons, info in results:
        big = [c for c in changes if c[1] is None or c[2] is None or abs((c[1] or 0) - (c[2] or 0)) > 2]
        print(f"{style:20s} {len(changes):4d} changed ({len(big)} by more than 2)  {info}")
        for (ka, kb), o, n in changes:
            if a.verbose or (ka, kb) in [c[0] for c in big]:
                print(f"    {ka:28s} {kb:32s} {o!s:>5} -> {n!s:>5}  {reasons.get((ka, kb), '')}")
        total += len(big)
    if a.check and total:
        sys.exit(1)


if __name__ == "__main__":
    main()
