# /// script
# dependencies = ["fonttools", "ufoLib2", "numpy"]
# ///
"""Greek kerning, derived from Instrument's Latin kerning and checked against
the outlines. Rerunning it replaces every Greek group membership and pair.

1. Each Greek glyph names the Latin glyph whose right side (first in a pair)
   and left side (second in a pair) its own sides match, and joins that
   glyph's kerning groups. ufo2ft's kern writer splits pairs per script, so
   Greek and Latin never kern against each other.
2. Where Instrument kerns the Latin analogue as a single glyph rather than
   through its group (T-u is a glyph pair, not a class pair), the Greek pair
   would fall through to 0. Every Greek pair (Greek-Greek and Greek against
   punctuation) is compared with the value its Latin analogues get, and any
   difference is written as a glyph pair, in every master.
3. A few pairs differ on purpose (OVERRIDES): the capitals with a tonos keep
   the tonos clear of a preceding T-like arm, and let it hang into the space
   after an opening bracket, quote or guillemet.
4. Where a Greek side is only partly like its analogue, the pair is set from
   the outlines against the analogue's white (`optical`): ζ ξ before
   lowercase take c's white plus a little; υ ω before ν γ χ take u's white
   before v and x; Γ's long arm closes over x-height letters to the white T's
   arm keeps; an x-height bar (π σ on the right, π τ on the left) keeps at
   least the white between two t's bars.
5. Collision guard: in every master the outlines of each Greek letter pair,
   after kerning, must stay at least as far apart as nearly every Latin
   letter pair does (the 2nd percentile of Latin pair distances, x0.55, in that
   master). Pairs closer than that are opened up.
6. Punctuation guard: a Greek letter beside punctuation (brackets, quotes,
   slash, figures) keeps at least 70% of the distance its Latin analogue
   keeps there; only negative kerning is taken back.

    uv run vendor/fonts/tilo-sans/scripts/greek/kern.py [--report]
"""
from __future__ import annotations

import argparse
import math
import sys
import unicodedata
from pathlib import Path

import numpy as np
import ufoLib2
from fontTools.misc import plistlib
from fontTools.pens.recordingPen import DecomposingRecordingPen

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gen import ORDER  # noqa: E402

SRC = Path(__file__).resolve().parents[2] / "sources"
MASTERS = ["CondensedRegular", "CondensedBold", "Regular", "Bold",
           "CondensedItalic", "CondensedBoldItalic", "Italic", "BoldItalic"]

# greek: (right side like, left side like); None = that side is not kerned
ANALOG = {
    # capitals that are Latin shapes
    "Alpha": ("A", "A"), "Beta": ("B", "B"), "Epsilon": ("E", "E"), "Zeta": ("Z", "Z"), "Eta": ("H", "H"),
    "Iota": ("I", "I"), "Kappa": ("K", "K"), "Mu": ("M", "M"), "Nu": ("N", "N"), "Omicron": ("O", "O"),
    "Rho": ("P", "P"), "Tau": ("T", "T"), "Upsilon": ("Y", "Y"), "Chi": ("X", "X"),
    # capitals drawn for Greek
    "Gamma": ("T", "F"), "Delta": ("A", "A"), "Theta": ("O", "O"), "Lambda": ("A", "A"), "Xi": ("Z", "Z"),
    "Pi": ("H", "H"), "Sigma": ("E", "Z"), "Phi": ("O", "O"), "Psi": ("U", "U"), "Omega": ("O", "O"),
    "Iotadieresis": ("I", "I"), "Upsilondieresis": ("Y", "Y"),
    # capitals with tonos: the tonos stands in front, so the left side is its own
    "Alphatonos": ("A", None), "Epsilontonos": ("E", None), "Etatonos": ("H", None), "Iotatonos": ("I", None),
    "Omicrontonos": ("O", None), "Upsilontonos": ("Y", None), "Omegatonos": ("O", None),
    # lowercase. Round right sides (υ ω) kern like o, ι's tail like t, λ's
    # right side (a diagonal ending at the baseline) like A, its near-upright
    # left side like l, and κ's stem like n's.
    "alpha": ("u", "o"), "beta": ("b", "h"), "gamma": ("v", "v"), "delta": ("o", "o"), "epsilon": ("c", "c"),
    "zeta": ("c", "c"), "eta": ("n", "n"), "theta": ("o", "o"), "iota": ("t", "idotless"), "kappa": ("k", "n"),
    "lambda": ("A", "l"), "mu": ("u", "u"), "nu": ("v", "v"), "xi": ("c", "c"), "omicron": ("o", "o"),
    "pi": ("n", "n"), "rho": ("p", "o"), "sigma": ("r", "o"), "sigmafinal": ("c", "c"), "tau": ("t", "t"),
    "upsilon": ("o", "u"), "phi": ("o", "o"), "chi": ("x", "x"), "psi": ("u", "u"), "omega": ("o", "u"),
    # accented: the left side kerns like the Latin letter with the same accent,
    # which Instrument kerns with care against T, V, Y and the like
    "alphatonos": ("u", "oacute"), "epsilontonos": ("c", "eacute"), "etatonos": ("n", "nacute"),
    "iotatonos": ("t", "iacute"), "omicrontonos": ("o", "oacute"), "upsilontonos": ("o", "uacute"),
    "omegatonos": ("o", "uacute"), "iotadieresis": ("t", "idieresis"), "upsilondieresis": ("o", "udieresis"),
    "iotadieresistonos": ("t", "idieresis"), "upsilondieresistonos": ("o", "udieresis"),
    # punctuation
    "questiongreek": ("semicolon", "semicolon"), "anoteleia": ("period", "period"),
    # symbols drawn from the Greek
    "micro": ("u", "u"), "horizontalbar": ("emdash", "emdash"), "horizontalbar.case": ("emdash.case", "emdash.case"),
}
# round-dot variants kern like their base
for _g in list(ANALOG):
    if _g + ".ss01" in ORDER:
        ANALOG[_g + ".ss01"] = ANALOG[_g]
ANALOG["questiongreek.ss01"] = ("semicolon.ss01", "semicolon.ss01")
ANALOG["anoteleia.ss01"] = ("period.ss01", "period.ss01")

TONOS_CAPS = ["Alphatonos", "Epsilontonos", "Etatonos", "Iotatonos", "Omicrontonos", "Upsilontonos", "Omegatonos"]
# (left, right): value at Regular weight and width, scaled per master by o's width
OVERRIDES = {
    ("phi", "chi"): 0, ("phi", "lambda"): 0,  # φ's bowl is wider than o
    ("lambda", "rho"): 0,  # λ's foot stands where ρ's stem crosses the baseline
}
# a T-like arm level with the bar of ζ and ξ: keep the two bars apart
for _l in ("Tau", "Gamma", "Upsilon", "Upsilondieresis"):
    for _r in ("zeta", "xi"):
        OVERRIDES[(_l, _r)] = -30
# a T-like arm over a capital's tonos
for _l, _v in (("Tau", 40), ("Gamma", 40), ("Upsilon", 40), ("Psi", 30), ("Rho", 20)):
    for _r in TONOS_CAPS:
        OVERRIDES[(_l, _r)] = _v
# an opening bracket, quote or guillemet before a capital with tonos: the
# tonos may hang into the space, or the letter sits visibly apart («Έκθεση)
for _l in ("parenleft", "parenleft.case", "bracketleft", "bracketleft.case", "braceleft", "braceleft.case",
           "quoteleft", "quotedblleft", "guillemetleft", "guilsinglleft"):
    for _r in TONOS_CAPS:
        OVERRIDES[(_l, _r)] = -45

GREEK = set(ANALOG)


UNICODES = {}


def is_letter(name):
    """Letters (Latin, Greek, ...) by the code point of the glyph or its base."""
    base = name.split(".")[0].split("_")[0]
    u = UNICODES.get(base)
    if u is None:
        return base[:1].isalpha() and len(base) <= 2
    return unicodedata.category(chr(u)).startswith("L")


class Kern:
    """UFO kerning lookup: glyph-glyph, glyph-group, group-glyph, group-group."""

    def __init__(self, kerning, groups):
        self.k = kerning
        self.g1 = {g: n for n, ms in groups.items() if n.startswith("public.kern1.") for g in ms}
        self.g2 = {g: n for n, ms in groups.items() if n.startswith("public.kern2.") for g in ms}

    def __call__(self, a, b):
        ga, gb = self.g1.get(a), self.g2.get(b)
        for x, y in ((a, b), (a, gb), (ga, b), (ga, gb)):
            if x is not None and y is not None and y in self.k.get(x, {}):
                return self.k[x][y]
        return 0


# ---------------------------------------------------------------- outlines

class Profiles:
    """Left and right ink profiles of each glyph, in bands of 8 units."""

    STEP = 8
    Y = np.arange(-300, 1050, STEP, dtype=float)

    def __init__(self, font):
        self.font = font
        self.cache = {}

    def get(self, name):
        if name not in self.cache:
            pen = DecomposingRecordingPen(self.font)
            self.font[name].draw(pen)
            polys, cur, start = [], [], None
            for op, args in pen.value:
                if op == "moveTo":
                    cur = [args[0]]
                elif op == "lineTo":
                    cur.append(args[0])
                elif op in ("curveTo", "qCurveTo"):
                    p0 = cur[-1]
                    pts = [p0] + list(args)
                    if op == "curveTo" and len(pts) == 4:
                        for i in range(1, 9):
                            t = i / 8
                            mt = 1 - t
                            cur.append((mt ** 3 * pts[0][0] + 3 * mt * mt * t * pts[1][0] + 3 * mt * t * t * pts[2][0] + t ** 3 * pts[3][0],
                                        mt ** 3 * pts[0][1] + 3 * mt * mt * t * pts[1][1] + 3 * mt * t * t * pts[2][1] + t ** 3 * pts[3][1]))
                    else:
                        cur.extend(args)
                elif op in ("closePath", "endPath"):
                    if cur:
                        polys.append(cur)
                    cur = []
            lo = np.full(len(self.Y), np.inf)
            hi = np.full(len(self.Y), -np.inf)
            for poly in polys:
                p = np.array(poly, dtype=float)
                q = np.roll(p, -1, axis=0)
                for (x0, y0), (x1, y1) in zip(p, q):
                    if y0 == y1:
                        idx = np.where(np.abs(self.Y - y0) <= self.STEP / 2)[0]
                        for i in idx:
                            lo[i] = min(lo[i], x0, x1)
                            hi[i] = max(hi[i], x0, x1)
                        continue
                    ya, yb = min(y0, y1), max(y0, y1)
                    idx = np.where((self.Y >= ya) & (self.Y <= yb))[0]
                    if len(idx):
                        xs = x0 + (self.Y[idx] - y0) * (x1 - x0) / (y1 - y0)
                        lo[idx] = np.minimum(lo[idx], xs)
                        hi[idx] = np.maximum(hi[idx], xs)
            ok = np.isfinite(lo)
            self.cache[name] = (self.Y[ok], lo[ok], hi[ok], self.font[name].width)
        return self.cache[name]

    def distance(self, a, b, kern):
        """Closest approach of a's right side to b's left side, set as a pair."""
        ya, _, ra, wa = self.get(a)
        yb, lb, _, _ = self.get(b)
        if not len(ya) or not len(yb):
            return math.inf
        dx = (lb[None, :] + wa + kern) - ra[:, None]
        dy = yb[None, :] - ya[:, None]
        h = np.where(np.abs(dy) <= 200, np.hypot(dx, dy), np.inf)
        d = float(h.min())
        same = dy == 0
        if same.any():
            overlap = float(dx[same].min())
            if overlap < 0:
                return overlap
        return d


def latin_floor(prof, kern, names):
    """Just over half the 2nd percentile of the distances between kerned Latin
    letter pairs: closer than that, two letters read as touching."""
    lower = [n for n in names if n in "abcdefghijklmnopqrstuvwxyz"]
    upper = [n for n in names if n in "ABCDEFGHIJKLMNOPQRSTUVWXYZ"]
    ds = []
    for group in (lower, upper):
        for a in group:
            for b in group:
                ds.append(prof.distance(a, b, kern(a, b)))
    return 0.55 * float(np.percentile(ds, 2))


# ---------------------------------------------------------------- optical rules

X, CAP = 510, 720


def rows(prof, name, y0, y1):
    Y, lo, hi, w = prof.get(name)
    sel = (Y >= y0) & (Y <= y1)
    return Y[sel], lo[sel], hi[sel], w


def gap_same(prof, a, b, kern, y0, y1):
    """Narrowest white between a and b, set as a pair, measured level (the
    same height on both sides) within y0..y1. None if they share no height."""
    ya, _, ra, wa = rows(prof, a, y0, y1)
    yb, lb, _, _ = rows(prof, b, y0, y1)
    common = np.intersect1d(ya, yb)
    if not len(common):
        return None
    return float((lb[np.isin(yb, common)] + wa + kern - ra[np.isin(ya, common)]).min())


def gap_bands(prof, a, b, kern, band_a, band_b):
    """White from a's rightmost ink within band_a to b's leftmost within band_b
    (a T-like arm over a lowercase letter)."""
    ya, _, ra, wa = rows(prof, a, *band_a)
    yb, lb, _, _ = rows(prof, b, *band_b)
    if not len(ya) or not len(yb):
        return None
    return float(lb.min() + wa + kern - ra.max())


def bar_floor(prof, latin):
    """The white Instrument keeps between two t's crossbars at the x-height:
    the tightest bar-to-bar meeting the Latin has."""
    return gap_same(prof, "t", "t", latin("t", "t"), X - 100, X)


# ζ ξ: c's right side, but their x-height body sits well left of the bar and
# hook that set the sidebearing
OPEN_RIGHT = ("zeta", "xi")
# υ ω and their accented forms: o on the right against round letters, but the
# arm rises to the x-height like u's stem
ARM_RIGHT = ("upsilon", "omega", "upsilontonos", "omegatonos", "upsilondieresis", "upsilondieresistonos")
DIAGONAL_LEFT = {"nu": "v", "gamma": "v", "chi": "x"}
BAR_RIGHT = ("pi", "sigma")
BAR_LEFT = ("pi", "tau")


def optical(t, prof, latin, wf, letters, an):
    lower = [g for g in letters if g[0].islower() and g not in ("micro",)]

    def top(g):
        Y, _, _, _ = prof.get(g)
        return Y.max()

    # ζ ξ before lowercase: the white of c before the same letter, and a
    # little more (their open right side is not a bowl)
    for a in OPEN_RIGHT:
        for b in lower:
            B = an(b, 2)
            if B is None:
                continue
            ref = gap_same(prof, "c", B, latin("c", B), 0, X)
            cur = gap_same(prof, a, b, 0, 0, X)
            if ref is not None and cur is not None:
                t[(a, b)] = min(t[(a, b)], round(ref + 40 * wf - cur))
    # υ ω before ν γ χ: the white u keeps before v and x
    for a in lower:
        if a.split(".")[0] not in ARM_RIGHT:
            continue
        for b, B in DIAGONAL_LEFT.items():
            ref = gap_same(prof, "u", B, latin("u", B), 0, X)
            cur = gap_same(prof, a, b, 0, 0, X)
            if ref is not None and cur is not None:
                t[(a, b)] = max(t[(a, b)], round(ref - cur))
    # Γ's arm reaches twice as far as T's right half: over an x-height letter
    # it closes to the white T's arm keeps over the same letter; over an
    # accented one it also keeps T's distance from the accent
    arm = (CAP - 150, CAP)
    for b in lower:
        B = an(b, 2)
        if B is None:
            continue
        ref = gap_bands(prof, "T", B, latin("T", B), arm, (-300, X + 20))
        cur = gap_bands(prof, "Gamma", b, 0, arm, (-300, X + 20))
        if ref is None or cur is None:
            continue
        v = round(ref - cur)
        if top(b) > X + 20:
            clear = prof.distance("T", B, latin("T", B))
            while prof.distance("Gamma", b, v) < clear and v < 300:
                v += 2
        t[("Gamma", b)] = v
    # a bar at the x-height (π's and σ's on the right, π's and τ's on the
    # left) never comes closer to the next letter than t's bar comes to the
    # next t's, or πτ πν πγ read as one stroke
    bar = bar_floor(prof, latin)
    for a in lower:
        for b in lower:
            if a not in BAR_RIGHT and b not in BAR_LEFT:
                continue
            g = gap_same(prof, a, b, t[(a, b)], X - 100, X)
            if g is not None and g < bar:
                t[(a, b)] += math.ceil(bar - g)


OPENING = ("parenleft", "bracketleft", "braceleft", "quoteleft", "quotedblleft", "guillemetleft", "guilsinglleft")
CLOSING = ("parenright", "bracketright", "braceright", "quoteright", "quotedblright", "guillemetright", "guilsinglright")


def punct_guard(t, prof, latin, letters, punct, an, opened, m):
    """Never let a Greek letter come much closer to punctuation than its
    Latin analogue does: at least 70% of the analogue's distance. Only takes
    back negative kerning; it never spaces wider than the plain advances."""
    for p in punct:
        for g in letters:
            for a, b, A, B in ((p, g, p, an(g, 2)), (g, p, an(g, 1), p)):
                if A is None or B is None or (a, b) not in t:
                    continue
                v = t[(a, b)]
                if v >= 0:
                    continue
                want = 0.70 * prof.distance(A, B, latin(A, B))
                if prof.distance(a, b, v) >= want:
                    continue
                nv = v
                while nv < 0 and prof.distance(a, b, nv) < want:
                    nv += 2
                nv = min(nv, 0)
                opened.append((m, a, b, v, nv, round(prof.distance(a, b, v))))
                t[(a, b)] = nv


# ---------------------------------------------------------------- main

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--report", action="store_true", help="print the opened-up pairs")
    args = ap.parse_args()

    regular = SRC / "TiloSans-Regular.ufo"
    groups = plistlib.loads((regular / "groups.plist").read_bytes())
    # 1. strip every Greek membership, then join the analogues' groups
    for n in list(groups):
        groups[n] = [g for g in groups[n] if g not in GREEK]
    k1 = {g: n for n, ms in groups.items() if n.startswith("public.kern1.") for g in ms}
    k2 = {g: n for n, ms in groups.items() if n.startswith("public.kern2.") for g in ms}
    for greek, (right, left) in ANALOG.items():
        if right in k1:
            groups[k1[right]].insert(0, greek)
        if left in k2:
            groups[k2[left]].insert(0, greek)
    data = plistlib.dumps(groups, sort_keys=True)

    fonts = {m: ufoLib2.Font.open(SRC / f"TiloSans-{m}.ufo") for m in MASTERS}
    kernings = {}
    for m in MASTERS:
        kerning = plistlib.loads((SRC / f"TiloSans-{m}.ufo" / "kerning.plist").read_bytes())
        for first in list(kerning):
            if first in GREEK:
                del kerning[first]
                continue
            kerning[first] = {s: v for s, v in kerning[first].items() if s not in GREEK}
            if not kerning[first]:
                del kerning[first]
        kernings[m] = kerning

    names = list(fonts["Regular"].keys())
    UNICODES.update({g.name: g.unicodes[0] for g in fonts["Regular"] if g.unicodes})
    # punctuation and symbols kerned in the Latin, on either side
    punct = sorted({g for n, ms in groups.items() for g in ms if g not in GREEK and not is_letter(g)}
                   | {g for k in kernings["Regular"].values() for g in k if not g.startswith("public.") and not is_letter(g)}
                   | {g for g in kernings["Regular"] if not g.startswith("public.") and not is_letter(g)})
    greek = [g for g in ORDER if g in GREEK]
    letters = [g for g in greek if g not in ("questiongreek", "anoteleia", "questiongreek.ss01", "anoteleia.ss01")]

    def an(g, side):
        if g in ANALOG:
            return ANALOG[g][0 if side == 1 else 1]
        return g

    pairs = [(a, b) for a in greek for b in greek] + [(a, p) for a in greek for p in punct] + [(p, b) for p in punct for b in greek]
    targets = {}
    opened = []
    for m in MASTERS:
        latin = Kern(kernings[m], groups)
        wf = fonts[m]["o"].width / 584.0
        t = {}
        for a, b in pairs:
            ra, lb = an(a, 1), an(b, 2)
            if ra is None or lb is None:
                v = 0
            else:
                v = latin(ra, lb)
            if (a, b) in OVERRIDES:
                v = round(OVERRIDES[(a, b)] * wf)
            t[(a, b)] = v
        prof = Profiles(fonts[m])
        # 4. where a Greek side differs from its Latin analogue, set the pair
        # by the outlines against the analogue's white
        optical(t, prof, latin, wf, letters, an)
        # 5. collision guard on letter pairs
        floor = latin_floor(prof, latin, names)
        for a in letters:
            for b in letters:
                v = t[(a, b)]
                d = prof.distance(a, b, v)
                if d < floor:
                    lo, hi = v, v + 300
                    for _ in range(20):
                        mid = (lo + hi) / 2
                        if prof.distance(a, b, mid) < floor:
                            lo = mid
                        else:
                            hi = mid
                    nv = math.ceil(hi)
                    opened.append((m, a, b, v, nv, round(d)))
                    t[(a, b)] = nv
        # 6. punctuation: a Greek letter beside a bracket or quote keeps most
        # of the white its Latin analogue has there (ζ and ξ reach up and down
        # into the bracket's curl, where c does not)
        punct_guard(t, prof, latin, letters, punct, an, opened, m)
        targets[m] = (t, floor)

    # 2./3. glyph pairs wherever the groups don't already give the target, in any master
    with_groups = {m: Kern(kernings[m], groups) for m in MASTERS}
    need = set()
    for m in MASTERS:
        t, _ = targets[m]
        for (a, b), v in t.items():
            if with_groups[m](a, b) != v:
                need.add((a, b))
    for m in MASTERS:
        t, floor = targets[m]
        kerning = kernings[m]
        for a, b in sorted(need):
            kerning.setdefault(a, {})[b] = t[(a, b)]
        (SRC / f"TiloSans-{m}.ufo" / "groups.plist").write_bytes(data)
        (SRC / f"TiloSans-{m}.ufo" / "kerning.plist").write_bytes(plistlib.dumps(kerning, sort_keys=True))
        print(f"{m}: Latin floor {floor:.0f} units, {len(need)} Greek glyph pairs")
    if args.report:
        for m, a, b, v, nv, d in opened:
            print(f"  {m:20} {a:>22} {b:<22} {v:5} -> {nv:5}  (was {d} apart)")
    else:
        print(f"{len(opened)} pair/master combinations opened up by the collision guard (--report lists them)")


if __name__ == "__main__":
    main()
