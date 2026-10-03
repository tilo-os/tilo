"""Per-master context: Latin parts (unskewed for italics) and measured parameters."""
from __future__ import annotations

import copy
import math
from pathlib import Path

import ufoLib2
from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.recordingPen import DecomposingRecordingPen

from geom import skew, TAN, SKEW_Y

SRC = Path(__file__).resolve().parents[2] / "sources"

UPRIGHT = ["CondensedRegular", "CondensedBold", "Regular", "Bold"]
ITALIC = ["CondensedItalic", "CondensedBoldItalic", "Italic", "BoldItalic"]
PAIR = dict(zip(ITALIC, UPRIGHT))


class Bounds:
    def __init__(self, contour):
        xs, ys = [], []
        # sample curves properly
        from fontTools.misc.bezierTools import calcCubicBounds
        from geom import segments
        for kind, pts in segments(contour):
            if kind == "curve":
                b = calcCubicBounds(*[tuple(p[:2]) for p in pts])
                xs += [b[0], b[2]]; ys += [b[1], b[3]]
            else:
                for p in pts:
                    xs.append(p[0]); ys.append(p[1])
        self.x0, self.x1, self.y0, self.y1 = min(xs), max(xs), min(ys), max(ys)

    @property
    def w(self):
        return self.x1 - self.x0


class Master:
    def __init__(self, name):
        self.name = name
        self.italic = "Italic" in name
        self.path = SRC / f"TiloSans-{name}.ufo"
        self.font = ufoLib2.Font.open(self.path)
        self.cond = name.startswith("Condensed")
        self.bold = "Bold" in name
        self._measure()
        if self.italic:
            up = Master(PAIR[name])
            for k in ("kh", "kv", "KH", "KV", "ZD"):
                setattr(self, k, getattr(up, k))

    # ---- reading Latin parts in upright space
    def c(self, name, i=None):
        g = self.font[name]
        out = []
        for contour in g.contours:
            pts = []
            for p in contour.points:
                x, y = p.x, p.y
                if self.italic:
                    x, y = skew(x, y, -1)
                pts.append([x, y, p.type if p.type != "move" else "line", p.smooth])
            out.append(pts)
        if i is not None:
            return copy.deepcopy(out[i])
        return copy.deepcopy(out)

    def width(self, name):
        return self.font[name].width

    def anchor(self, name, aname):
        for a in self.font[name].anchors:
            if a.name == aname:
                x, y = a.x, a.y
                if self.italic:
                    x, y = skew(x, y, -1)
                return x, y
        return None

    def bounds(self, name, i):
        return Bounds(self.c(name, i))

    # ---- measurements
    def _measure(self):
        ob, ib = self.bounds("o", 0), self.bounds("o", 1)
        self.o_out, self.o_in = ob, ib
        self.So = ib.x0 - ob.x0
        self.Ho = ib.y0 - ob.y0
        Ob, Ib = self.bounds("O", 0), self.bounds("O", 1)
        self.O_out, self.O_in = Ob, Ib
        self.SO = Ib.x0 - Ob.x0
        self.HO = Ib.y0 - Ob.y0
        n0 = self.bounds("n", 0)
        self.S = n0.w  # n stem incl. nothing else (contour 0 is the stem)
        self.lsb_n = n0.x0
        self.Wn = self.width("n")
        self.Wo = self.width("o")
        self.lsb_o = ob.x0
        H0 = self.bounds("H", 0)
        self.SH = H0.w
        self.lsb_H = H0.x0
        hb = self.bounds("H", 2)
        self.HB = hb.y1 - hb.y0
        tb = self.bounds("t", 1)
        self.TB = tb.y1 - tb.y0
        eb = self.bounds("E", 1)
        self.EB = eb.y1 - eb.y0
        # handle ratios of o (outer, first quadrant: bottom -> right)
        oc = self.c("o", 0)
        p0, p1, p2, p3 = oc[0], oc[1], oc[2], oc[3]
        self.kh = abs(p1[0] - p0[0]) / abs(p3[0] - p0[0])
        self.kv = abs(p3[1] - p2[1]) / abs(p3[1] - p0[1])
        Oc = self.c("O", 0)
        p0, p1, p2, p3 = Oc[0], Oc[1], Oc[2], Oc[3]
        self.KH = abs(p1[0] - p0[0]) / abs(p3[0] - p0[0])
        self.KV = abs(p3[1] - p2[1]) / abs(p3[1] - p0[1])
        # Z diagonal thickness (perpendicular)
        z = self.c("Z", 0)
        a, b = z[4], z[5]          # 136,31 -> 559,658
        c = z[10]                   # 483,689 on the other edge
        dx, dy = b[0] - a[0], b[1] - a[1]
        L = math.hypot(dx, dy)
        self.ZD = abs((c[0] - a[0]) * dy - (c[1] - a[1]) * dx) / L
        # v arm horizontal width at the top
        v = self.c("v", 0)
        self.v = v
        # width factor vs Regular
        self.wf = self.Wo / 584.0

    def __repr__(self):
        return (f"{self.name}: S={self.S:.0f} So={self.So:.0f} Ho={self.Ho:.0f} SO={self.SO:.0f} HO={self.HO:.0f} "
                f"HB={self.HB:.0f} TB={self.TB:.0f} EB={self.EB:.0f} kh={self.kh:.3f} kv={self.kv:.3f} "
                f"KH={self.KH:.3f} KV={self.KV:.3f} ZD={self.ZD:.1f} Wo={self.Wo} Wn={self.Wn} lsb_n={self.lsb_n:.0f} lsb_o={self.lsb_o:.0f}")


if __name__ == "__main__":
    for n in UPRIGHT + ITALIC:
        print(Master(n))
