"""Per-master context: Latin source outlines (italic ones slanted back
upright), measured stems and helpers shared by the Cyrillic builders."""

from __future__ import annotations

import bisect
from dataclasses import dataclass, field

import ufoLib2
from fontTools.misc.transform import Transform

import geom as G

UPRIGHT = ["CondensedRegular", "CondensedBold", "Regular", "Bold"]
ITALIC = ["CondensedItalic", "CondensedBoldItalic", "Italic", "BoldItalic"]
LIGHT = {"CondensedRegular": "CondensedRegular", "CondensedBold": "CondensedRegular", "Regular": "Regular", "Bold": "Regular",
         "CondensedItalic": "CondensedItalic", "CondensedBoldItalic": "CondensedItalic", "Italic": "Italic", "BoldItalic": "Italic"}
HEAVY = {"CondensedRegular": "CondensedBold", "CondensedBold": "CondensedBold", "Regular": "Bold", "Bold": "Bold",
         "CondensedItalic": "CondensedBoldItalic", "CondensedBoldItalic": "CondensedBoldItalic", "Italic": "BoldItalic", "BoldItalic": "BoldItalic"}


@dataclass
class Glyph:
    width: float
    contours: list = field(default_factory=list)
    anchors: dict = field(default_factory=dict)  # name -> (x, y), upright-equivalent space
    components: list = field(default_factory=list)  # (base, dx, dy) in final space
    lib: dict = field(default_factory=dict)


def flatten(con, steps=32):
    pts = []
    n = len(con)
    on = [i for i, p in enumerate(con) if G.is_on(p)]
    for k, i in enumerate(on):
        prev = on[k - 1]
        seg = []
        j = prev
        while True:
            seg.append(con[j % n])
            if j % n == i and len(seg) > 1:
                break
            j += 1
        if len(seg) == 4:
            for s in range(1, steps + 1):
                pts.append(G._cubic(seg, s / steps))
        else:
            pts.append((seg[-1][0], seg[-1][1]))
    return pts


def xcuts(contours, y):
    xs = []
    for con in contours:
        pts = flatten(con)
        for (x0, y0), (x1, y1) in zip(pts, pts[1:] + pts[:1]):
            if (y0 <= y < y1) or (y1 <= y < y0):
                xs.append(x0 + (y - y0) * (x1 - x0) / (y1 - y0))
    return sorted(xs)


def ycuts(contours, x):
    ys = []
    for con in contours:
        pts = flatten(con)
        for (x0, y0), (x1, y1) in zip(pts, pts[1:] + pts[:1]):
            if (x0 <= x < x1) or (x1 <= x < x0):
                ys.append(y0 + (x - x0) * (y1 - y0) / (x1 - x0))
    return sorted(ys)


def pmap(pairs):
    """Piecewise-linear monotonic map through (src, dst) pairs; slope 1 outside."""
    pairs = sorted(pairs)
    src = [p[0] for p in pairs]

    def f(v):
        if v <= src[0]:
            return pairs[0][1] + (v - src[0])
        if v >= src[-1]:
            return pairs[-1][1] + (v - src[-1])
        i = bisect.bisect_right(src, v) - 1
        (s0, d0), (s1, d1) = pairs[i], pairs[i + 1]
        return d0 + (v - s0) * (d1 - d0) / (s1 - s0)

    return f


def zmap(contours, xpairs=None, ypairs=None):
    fx = pmap(xpairs) if xpairs else (lambda v: v)
    fy = pmap(ypairs) if ypairs else (lambda v: v)
    return [[[fx(p[0]), fy(p[1]), p[2], p[3]] for p in con] for con in contours]


class Ctx:
    def __init__(self, master: str, fonts: dict[str, ufoLib2.Font]):
        self.master = master
        self.fonts = fonts
        self.font = fonts[master]
        self.italic = "Italic" in master
        self.bold = "Bold" in master
        self.condensed = "Condensed" in master
        self._cache = {}

    # ------------------------------------------------------------ sources
    def outline(self, name, master=None):
        master = master or self.master
        key = (name, master)
        if key not in self._cache:
            font = self.fonts[master]
            cons = self._flat(font, name, Transform())
            if "Italic" in master:
                cons = G.unslant(cons)
            self._cache[key] = cons
        return G.copy(self._cache[key])

    def _flat(self, font, name, t):
        glyph = font[name]
        out = []
        for c in glyph.contours:
            con = []
            for p in c.points:
                x, y = t.transformPoint((p.x, p.y))
                con.append([x, y, p.type, p.smooth])
            out.append(con)
        for comp in glyph.components:
            out += self._flat(font, comp.baseGlyph, t.transform(comp.transformation))
        return out

    def g(self, name):
        return self.outline(name)

    def w(self, name):
        return self.font[name].width

    def anchor(self, name, anchor):
        for a in self.font[name].anchors:
            if a.name == anchor:
                x, y = a.x, a.y
                if self.italic:
                    x = x - G.SLANT * (y - G.SLANT_Y0)
                return x, y
        raise KeyError(f"{name} has no anchor {anchor}")

    def b(self, contours):
        return G.bounds(contours)

    def other(self, master):
        """The context of another master of the same style."""
        key = ("ctx", master)
        if key not in self._cache:
            self._cache[key] = self if master == self.master else Ctx(master, self.fonts)
        return self._cache[key]

    @property
    def light(self):
        return self.other(LIGHT[self.master])

    @property
    def heavy(self):
        return self.other(HEAVY[self.master])

    @property
    def wt(self):
        """Where this master sits between its light and bold masters (0..1),
        by lowercase stem."""
        lo, hi = self.light.lc["V"], self.heavy.lc["V"]
        return (self.lc["V"] - lo) / (hi - lo)

    def blend(self, name, t):
        """The outline of `name` interpolated from this master's light master
        (t=0) to its bold master (t=1): a lighter drawing of the same width
        class, for parts that must stay lighter than the stems."""
        return G.interpolate(self.light.g(name), self.heavy.g(name), t)

    # ------------------------------------------------------------ metrics
    @property
    def cap(self):
        if "cap" not in self._cache:
            H = self.g("H")
            L, R, bar = G.bounds([H[0]]), G.bounds([H[1]]), G.bounds([H[2]])
            E = self.g("E")
            arm = G.bounds([E[1]])
            top = G.bounds([E[3]])
            O = self.g("O")
            ob = G.bounds([O[0]])
            ib = G.bounds([O[1]])
            self._cache["cap"] = dict(
                W=self.w("H"), lsb=L[0], V=L[2] - L[0], Lx0=L[0], Lx1=L[2], Rx0=R[0], Rx1=R[2],
                bar0=bar[1], bar1=bar[3], arm=arm[3] - arm[1], top0=top[1], armEnd=top[2],
                Oside=ib[0] - ob[0], Otop=ob[3] - ib[3], Olsb=ob[0], OW=self.w("O"),
            )
        return self._cache["cap"]

    @property
    def lc(self):
        if "lc" not in self._cache:
            n = self.g("n")
            s = G.bounds([n[0]])
            r = xcuts([n[1]], 100)
            o = self.g("o")
            ob, ib = G.bounds([o[0]]), G.bounds([o[1]])
            t = self.g("t")
            tb = G.bounds([t[1]])
            self._cache["lc"] = dict(
                W=self.w("n"), lsb=s[0], V=s[2] - s[0], Lx0=s[0], Lx1=s[2], Rx0=r[0], Rx1=r[1],
                H=ob[3] - ib[3], arm=tb[3] - tb[1], oside=ib[0] - ob[0], olsb=ob[0], oW=self.w("o"),
            )
        return self._cache["lc"]
