"""Ink profiles of glyphs as construct.py builds them, for kerning that
depends on where the ink is (scripts/cyrillic/kern.py, FLOOR1/FLOOR2).

Profiles are measured in the upright-equivalent space the construction works
in (the italic slanted back): a shear moves every point at one height by the
same amount, so horizontal gaps between two glyphs are the same as in the
slanted italic.
"""

from __future__ import annotations

import math
import unicodedata

import numpy as np

import geom as G
from ctx import flatten

BAND = 4
YMIN, YMAX = -320, 1160
NB = (YMAX - YMIN) // BAND


class Ink:
    def __init__(self, c, built):
        self.c = c
        self.built = built
        self._prof = {}
        self._cmap = None

    # ------------------------------------------------------------ outlines
    def outline(self, name):
        """(contours, advance) of `name` as it will be written."""
        g = self.built.get(name)
        if g is None:
            return self.c.g(name), self.c.w(name)
        cons = G.copy(g.contours)
        for base, dx, dy in g.components:
            sub, _ = self.outline(base)
            # component offsets are given in the final (slanted) space
            cons += G.shift(sub, dx - (G.SLANT * dy if self.c.italic else 0), dy)
        return cons, g.width

    def profile(self, name):
        """(left, right, advance): per 4-unit band the leftmost and rightmost
        ink (None where the band is empty)."""
        if name not in self._prof:
            cons, w = self.outline(name)
            left = [math.inf] * NB
            right = [-math.inf] * NB
            for con in cons:
                pts = flatten(con, 16)
                for (x0, y0), (x1, y1) in zip(pts, pts[1:] + pts[:1]):
                    ya, yb = min(y0, y1), max(y0, y1)
                    i0 = max(0, int((ya - YMIN) // BAND))
                    i1 = min(NB - 1, int((yb - YMIN) // BAND))
                    for i in range(i0, i1 + 1):
                        lo, hi = YMIN + i * BAND, YMIN + (i + 1) * BAND
                        if y1 == y0:
                            xa, xb = x0, x1
                        else:
                            ta = (max(lo, ya) - y0) / (y1 - y0)
                            tb = (min(hi, yb) - y0) / (y1 - y0)
                            xa, xb = x0 + ta * (x1 - x0), x0 + tb * (x1 - x0)
                        left[i] = min(left[i], xa, xb)
                        right[i] = max(right[i], xa, xb)
            self._prof[name] = (np.array(left), np.array(right), w)
        return self._prof[name]

    def gap(self, a, b, kern=0.0, ylo=YMIN, yhi=YMAX, reach=48):
        """The shortest distance between the inks of a and b set side by
        side with `kern`, looking at ink between heights ylo and yhi only:
        horizontal within a band, diagonal to bands up to `reach` units
        above or below (so a flag over a neighbour's corner counts); negative
        where they overlap; +inf when nothing in the range faces anything."""
        la, ra, wa = self.profile(a)
        lb, rb, wb = self.profile(b)
        ys = YMIN + BAND * (np.arange(NB) + 0.5)
        inside = (ys >= ylo) & (ys <= yhi)
        ra = np.where(inside, ra, -np.inf)
        lb = np.where(inside, lb, np.inf)
        best = math.inf
        k = int(reach // BAND)
        for j in range(-k, k + 1):
            if j >= 0:
                d = lb[j:] + wa + kern - ra[:NB - j]
            else:
                d = lb[:NB + j] + wa + kern - ra[-j:]
            d = d[np.isfinite(d)]
            if not len(d):
                continue
            m = float(d.min())
            if j == 0:
                best = min(best, m)
            else:  # ink over (or under) the neighbour's: the vertical distance
                best = min(best, math.hypot(max(m, 0.0), abs(j) * BAND))
        return best

    # ------------------------------------------------------------ classes
    def kind(self, name):
        """'cyrl' for a Cyrillic letter, 'mark' for punctuation, figures and
        symbols (they join a Cyrillic run), None for other scripts' letters
        and anything unencoded we cannot place."""
        if name.split(".")[0].endswith("-cy"):
            return "cyrl"
        font = self.c.font
        base = name.split(".")[0]
        g = font[base] if base in font else None
        if g is None or not g.unicodes:
            return None
        cat = unicodedata.category(chr(g.unicodes[0]))
        if cat[0] == "L" or cat[0] == "M":
            return None
        return "mark"
