# /// script
# dependencies = ["ufoLib2", "fonttools"]
# ///
"""Draw the Greek glyphs into all eight masters.

Each glyph is constructed per master from that master's own Latin parts and
measured stems, in upright space; italic masters are sheared on write.
This is the source of the Greek: it overwrites the Greek .glif files, so
change a Greek glyph here, not in the UFO.

    uv run --with ufoLib2 --with fonttools python gen.py [glyph ...]
"""
from __future__ import annotations

import copy
import math
import sys
from pathlib import Path

from fontTools.ufoLib import glifLib

from geom import (P, Stroke, area, ccw, cw, mirror_x, poly, rect, reverse, rounded, skew, transform, unit,
                  translate, TAN, segments)
from master import Master, UPRIGHT, ITALIC, Bounds

GLYPHS = {}
ORDER = []


def glyph(name, uni=None):
    def deco(fn):
        GLYPHS[name] = (fn, uni)
        ORDER.append(name)
        return fn
    return deco


def spec(width, contours=(), components=(), anchors=None, **kw):
    d = dict(width=width, contours=list(contours), components=list(components), anchors=anchors or {})
    d.update(kw)
    return d


def fit(contour, src, x0, y0, x1, y1):
    """Map a contour linearly so the box `src` (a Bounds) lands on x0..x1, y0..y1."""
    sx = (x1 - x0) / src.w
    sy = (y1 - y0) / (src.y1 - src.y0)
    return transform(contour, lambda x, y: (x0 + (x - src.x0) * sx, y0 + (y - src.y0) * sy))


def ring(m, x0, y0, x1, y1, V=None, H=None, caps=False):
    """A bowl drawn from the master's own o (or O): its outer and inner contours
    stretched to the box, the counter inset by the stroke weights. Italic
    masters read the italic o unsheared, so the bowl keeps Instrument's italic
    curve correction when it is sheared back."""
    V = V if V is not None else (m.SO if caps else m.So)
    H = H if H is not None else (m.HO if caps else m.Ho)
    name = "O" if caps else "o"
    outer, inner = m.c(name)
    ob, ib = (m.O_out, m.O_in) if caps else (m.o_out, m.o_in)
    return [ccw(fit(outer, ob, x0, y0, x1, y1)),
            cw(fit(inner, ib, x0 + V, y0 + H, x1 - V, y1 - H))]


def stroke(m, caps=False, V=None, H=None):
    slant = TAN if m.italic else 0.0
    if caps:
        return Stroke(V or m.SO, H or m.HO, m.KH, m.KV, slant=slant)
    return Stroke(V or m.So, H or m.Ho, m.kh, m.kv, slant=slant)


X, CAP, ASC, DESC, OV = 510, 720, 720, -205, 10


def set_y(contour, pred, y):
    for p in contour:
        if pred(p):
            p[1] = y
    return contour


# =====================================================================
# Lowercase
# =====================================================================


@glyph("omicron", 0x03BF)
def omicron(m):
    return spec(m.Wo, components=[("o", 0, 0)])


@glyph("eta", 0x03B7)
def eta(m):
    cs = m.c("n")
    set_y(cs[1], lambda p: abs(p[1]) < 1, DESC)
    return spec(m.Wn, cs, anchors={"top": m.anchor("n", "top")})


@glyph("iota", 0x03B9)
def iota(m):
    st = m.c("t", 0)
    b = Bounds(st)
    # stem left edge = the bottom-left on-curve x of the vertical
    stem_l = min(p[0] for p in st if p[2] == "line" and p[1] > 300)
    set_y(st, lambda p: p[1] > X, X)
    # tail a little shorter than t's: compress everything right of the stem
    stem_r = stem_l + m.S
    for p in st:
        if p[0] > stem_r:
            p[0] = stem_r + (p[0] - stem_r) * 0.82
    dx = m.lsb_n - stem_l
    st = translate(st, dx)
    b = Bounds(st)
    rsb = m.width("t") - Bounds(m.c("t", 0)).x1
    w = b.x1 + rsb + 6 * m.wf
    return spec(round(w), [st], anchors={"top": (m.lsb_n + m.S / 2, X), "bottom": (m.lsb_n + m.S / 2, 0)})


@glyph("kappa", 0x03BA)
def kappa(m):
    cs = m.c("k")
    set_y(cs[0], lambda p: p[1] > X, X)
    w = m.width("k")
    return spec(w, cs, anchors={"top": (w / 2 - 10 * m.wf, X), "bottom": (w / 2, 0)})


@glyph("mu", 0x03BC)
def mu(m):
    cs = m.c("u")
    c0 = cs[0]
    top = [p for p in c0 if abs(p[1] - X) < 1 and p[2]]
    xl, xr = min(p[0] for p in top), max(p[0] for p in top)
    cs.append(rect(xl, DESC, xr, 200))
    return spec(m.width("u"), cs, anchors={"top": m.anchor("u", "top")})


@glyph("rho", 0x03C1)
def rho(m):
    cs = m.c("o")
    ob = m.o_out
    cy = (ob.y0 + ob.y1) / 2
    # the stem continues the bowl's left side: same weight as the bowl side
    cs.append(rect(ob.x0, DESC, ob.x0 + m.So, cy))
    return spec(m.Wo, cs, anchors={"top": m.anchor("o", "top")})


@glyph("sigma", 0x03C3)
def sigma(m):
    cs = m.c("o")
    ob = m.o_out
    cy = (ob.y0 + ob.y1) / 2
    xc = (ob.x0 + ob.x1) / 2
    # the bowl tops out flat at the x-height, level with the bar: no overshoot
    # above the bar, and the top of the bowl flattens into it
    for c in cs:
        for p in c:
            if p[1] > cy:
                p[1] -= OV
    outer = cs[0]
    for p in outer:
        # pull the top-right handle out along the bar so the bowl runs into it
        if p[2] is None and p[1] > X - 1 and p[0] > xc:
            p[0] = xc + (p[0] - xc) * 1.25
    # the bar runs well past the bowl so σ never reads as ο at text sizes
    bar_end = ob.x1 + (118 + 12 * m.bold) * m.wf
    cs.append(rect(xc, X - m.Ho, bar_end, X))
    w = bar_end + 24 * m.wf
    return spec(round(w), cs, anchors={"top": (xc, X)})


@glyph("tau", 0x03C4)
def tau(m):
    cs = m.c("t")
    set_y(cs[0], lambda p: p[1] > X, X)
    w = m.width("t")
    stem_l = min(p[0] for p in cs[0] if p[2] == "line" and p[1] > 300)
    return spec(w, cs, anchors={"top": (stem_l + m.S / 2, X)})


def arm_top(s, x, side, m, first, counter):
    """An arm of υ ω that rises straight and leans in over its top quarter:
    the tip stands in by a fraction of the counter (about 20 units in
    Regular, less where the counter is narrow), cut level at the x-height.
    side: -1 for a left arm, +1 for a right one. The node runs on above the
    x-height and the outline is cut there."""
    din = 4 + 0.06 * counter
    lean = 13 * din / 20  # degrees off vertical at the tip
    ext = 24
    tip = (x - side * (din + ext * math.tan(math.radians(lean))), X + ext)
    ang = 90 + side * lean
    if first:
        s.add(*tip, ang, k=0.5)
        s.add(x, 0.70 * X, "v", k=0.62)
    else:
        s.add(x, 0.70 * X, "v", k=0.62)
        s.add(*tip, ang, k=0.5)
    return s


@glyph("upsilon", 0x03C5)
def upsilon(m):
    lsb = (m.lsb_o + m.lsb_n) / 2
    body = 0.85 * (m.o_out.w) + 0.15 * m.So
    xl = lsb + m.So / 2
    xr = lsb + body - m.So / 2
    cx = (xl + xr) / 2
    yb = -OV + m.Ho / 2
    s = stroke(m)
    counter = xr - xl - m.So
    arm_top(s, xl, -1, m, True, counter)
    s.add(xl, 250, "v", seg="line").add(cx, yb, "h").add(xr, 250, "v")
    s.add(xr, 0.70 * X, "v", seg="line")
    arm_top(s, xr, 1, m, False, counter)
    # arm_top added the straight node itself for the right arm: drop the duplicate
    s.nodes.pop(-2)
    w = lsb * 2 + body
    return spec(round(w), [s.outline(start_cut=("y", X), end_cut=("y", X))], anchors={"top": (cx, X)})


@glyph("chi", 0x03C7)
def chi(m):
    w = m.width("x")
    sb = 24 * m.wf + (m.S - 80) * 0.1
    # the weight of x's long stroke, measured across it, carried onto χ's
    # longer, steeper diagonals (horizontal width alone would make χ heavier)
    c = m.c("x", 0)
    bl, br, tl = c[0], c[1], c[4]
    weight = (br[0] - bl[0]) * math.cos(math.atan((bl[0] - tl[0]) / (tl[1] - bl[1])))
    span = w - 2 * sb
    if m.italic:
        # the shear stands one diagonal up and lays the other down; size each
        # so that, sheared, both carry the same weight across the stroke
        def hw(dx):
            dxs = dx - (X - DESC) * TAN
            return 0.98 * weight * math.hypot(dxs, X - DESC) / (X - DESC)
        da, db = hw(span), hw(-span)   # top-left to bottom-right, and the other
    else:
        da = db = weight / math.cos(math.atan(span / (X - DESC)))
    a = poly((sb, X), (sb + da, X), (w - sb, DESC), (w - sb - da, DESC))
    b = poly((w - sb - db, X), (w - sb, X), (sb + db, DESC), (sb, DESC))
    return spec(round(w), [a, b], anchors={"top": (w / 2, X)})


@glyph("pi", 0x03C0)
def pi(m):
    # the bar overhangs both stems by well over a stem width (π, not п) and is
    # as light as Instrument's other horizontals
    f = m.wf
    sb = 22 * f
    over = 80 * f + 0.5 * m.S
    counter = (0.86 - 0.12 * m.bold) * (m.Wn - 2 * m.lsb_n + 6 * f - 2 * m.S)  # tighter than n's
    bar = 0.92 * m.Ho
    xl = sb + over
    xr = xl + m.S + counter
    w = xr + m.S + over + sb
    cs = [rect(sb, X - bar, w - sb, X),
          rect(xl, 0, xl + m.S, X - bar / 2),
          rect(xr, 0, xr + m.S, X - bar / 2)]
    return spec(round(w), cs, anchors={"top": (w / 2, X)})


@glyph("alpha", 0x03B1)
def alpha(m):
    cs = m.c("a.ss02")
    bowl = cs[0]
    # drop the stem: the bowl closes along the stem's inner edge
    bowl = [p for p in bowl if not (p[2] == "line" and (abs(p[1]) < 1 or abs(p[1] - X) < 1))]
    cs[0] = bowl
    stem_l = min(p[0] for p in bowl if p[2] == "line")
    t = m.c("t", 0)
    t_l = min(p[0] for p in t if p[2] == "line" and p[1] > 300)
    set_y(t, lambda p: p[1] > X, X)
    t_r = t_l + m.S
    for p in t:
        if p[0] > t_r:
            p[0] = t_r + (p[0] - t_r) * 0.72
    t = translate(t, stem_l - t_l)
    cs.append(t)
    b = Bounds(t)
    w = b.x1 + 26 * m.wf
    return spec(round(w), cs, anchors={"top": m.anchor("a.ss02", "top")})


@glyph("gamma", 0x03B3)
def gamma(m):
    """v's arms, steepened so they meet above the baseline. The right arm
    runs on without a break and bends down into the tail, which leans a
    little left; the left arm runs into it. No stalk hung from the vertex."""
    v = m.c("v", 0)
    # v: [bl, br, top-right outer, top-right inner, crotch, crotch, top-left inner, top-left outer]
    bl, br, tro, tri, c1, c2, tli, tlo = [p[:2] for p in v]
    w = m.width("v")
    lift = 110 + 0.4 * (m.Ho - 68)
    sq = lambda p: (p[0], lift + p[1] * (X - lift) / X)
    bl, br, c1, c2 = sq(bl), sq(br), sq(c1), sq(c2)
    # right arm centre line, top to vertex
    rt = ((tro[0] + tri[0]) / 2, X)
    rb = ((bl[0] + br[0]) / 2 + (br[0] - bl[0]) * 0.0, lift)
    d = unit(rb[0] - rt[0], rb[1] - rt[1])
    ang = math.degrees(math.atan2(d[1], d[0]))
    # the right arm keeps v's weight across the stroke
    arm_w = (tro[0] - tri[0]) * math.cos(math.atan(abs(d[0] / d[1])))
    s = stroke(m)
    pen_w = math.sqrt((s.V * d[1]) ** 2 + (s.H * d[0]) ** 2)
    wa = arm_w / pen_w
    tail = 0.90 * m.S / s.V   # p's stem less a tenth
    yj = lift + 0.35 * (X - lift)
    xj = rt[0] + (yj - X) * d[0] / d[1]
    xb = (bl[0] + br[0]) / 2 - 0.05 * w
    s.add(rt[0] - 30 * d[0] / d[1] * -1, X + 30, ang, w=wa)
    s.add(xj, yj, ang, seg="line", w=wa, k=0.5)
    s.add(xb, DESC - 30, -96, w=tail, k=0.45)
    right = s.outline(start_cut=("y", X), end_cut=("y", DESC))
    # left arm: v's, on into the right stroke
    cl = [(rt[0], X), (xj, yj)]
    po = cross(tlo, bl, *cl)
    pi_ = cross(tli, c2, *cl)
    # carry the outer edge a little past the centre line so the outer
    # crotch is the tail's own edge, not a notch
    left = poly(tlo, tli, pi_, (po[0] + 0.25 * (po[0] - tlo[0]) * 0.0, po[1]))
    return spec(w, [left, right], anchors={"top": (w / 2, X)})


def line_x(p, q, y):
    """x on the line through p and q at height y."""
    return p[0] + (q[0] - p[0]) * (y - p[1]) / (q[1] - p[1])


def cross(p1, p2, p3, p4):
    """Intersection of the lines p1-p2 and p3-p4."""
    d = (p1[0] - p2[0]) * (p3[1] - p4[1]) - (p1[1] - p2[1]) * (p3[0] - p4[0])
    a = p1[0] * p2[1] - p1[1] * p2[0]
    b = p3[0] * p4[1] - p3[1] * p4[0]
    return ((a * (p3[0] - p4[0]) - (p1[0] - p2[0]) * b) / d, (a * (p3[1] - p4[1]) - (p1[1] - p2[1]) * b) / d)


@glyph("lambda", 0x03BB)
def lambda_(m):
    """y's long stroke turned over, reaching the ascender. The short leg is
    its own stroke: it meets the long one a little under half way up, and
    its foot stands well inside the long stroke's, so the base is narrower
    than a small Λ's and λλ, κλ keep their white at the baseline."""
    y = m.c("y")
    # y's long stroke, turned over: its descender end becomes the top at the
    # ascender, its arm's top end the foot. Straight edges (y's notch where
    # the arm joins would show above the lower join)
    ends = [(p[0], X - p[1]) for p in y[0] if abs(p[1] - DESC) < 1 or abs(p[1] - X) < 1]
    tl, tr = sorted([(x, ASC) for x, yy in ends if yy > X], key=lambda p: p[0])
    bl, br = sorted([p for p in ends if abs(p[1]) < 1], key=lambda p: p[0])
    # the top keeps the descender's horizontal width, the slope runs on to ASC
    long_ = poly(tl, tr, br, bl)
    # perpendicular weight of y's short arm, carried onto the leg
    arm = y[1]
    al = sorted(arm, key=lambda p: p[1])
    lo_pts = sorted(al[:2], key=lambda p: p[0])
    hi_pts = sorted(al[2:], key=lambda p: p[0])
    hw = hi_pts[1][0] - hi_pts[0][0]
    slope0 = (hi_pts[0][0] - lo_pts[0][0]) / (hi_pts[0][1] - lo_pts[0][1])
    # a touch lighter than the long stroke, so the heavy joins stay open
    perp = abs(hw) * math.cos(math.atan(abs(slope0))) * (0.92 if m.bold else 0.97)
    # crotch (leg's inner edge meets the long stroke) at about 41% of the
    # height, so the leg's centre joins at about 47%
    foot_r = hi_pts[1][0] + 28 * m.wf
    ls = (tl[0] - bl[0]) / ASC   # the long stroke's left edge leans this way
    yc = 0.40 * ASC
    for _ in range(30):
        xc = line_x(tl, bl, yc)
        slope = (xc - foot_r) / yc
        fw = perp / math.cos(math.atan(slope))
        yj = yc + fw / (slope - ls)  # the leg's outer edge meets the long stroke
        yc += 0.5 * (0.47 * ASC - (yc + yj) / 2)
    foot_l = foot_r - fw
    # both edges run on into the long stroke and stop on its centre line
    cl_t = ((tl[0] + tr[0]) / 2, ASC)
    cl_b = ((bl[0] + br[0]) / 2, 0)
    pi_ = cross((foot_r, 0), (xc, yc), cl_t, cl_b)
    po = cross((foot_l, 0), (foot_l + slope * yc, yc), cl_t, cl_b)
    leg = ccw([P(foot_l, 0), P(foot_r, 0), P(*pi_), P(*po)])
    # y's advance: the foot drawn in is white gained at the baseline (λλ, κλ);
    # the heavy masters give the long stroke's foot a little more room, as A has
    w = m.width("y") + 0.15 * (m.S - 80 * m.wf)
    return spec(round(w), [long_, leg], anchors={"top": (w / 2 - 60 * m.wf, ASC + 20)})


# =====================================================================
# Uppercase
# =====================================================================

for _g, _l, _u in [("Alpha", "A", 0x0391), ("Beta", "B", 0x0392), ("Epsilon", "E", 0x0395), ("Zeta", "Z", 0x0396),
                   ("Eta", "H", 0x0397), ("Iota", "I", 0x0399), ("Kappa", "K", 0x039A), ("Mu", "M", 0x039C),
                   ("Nu", "N", 0x039D), ("Omicron", "O", 0x039F), ("Rho", "P", 0x03A1), ("Tau", "T", 0x03A4),
                   ("Upsilon", "Y", 0x03A5), ("Chi", "X", 0x03A7)]:
    def _mk(latin):
        return lambda m: spec(m.width(latin), components=[(latin, 0, 0)])
    glyph(_g, _u)(_mk(_l))


@glyph("Gamma", 0x0393)
def Gamma(m):
    cs = m.c("F")
    return spec(m.width("F") - 6 * m.wf, [cs[0], cs[2]], anchors={"top": (m.anchor("E", "top")[0] - 40 * m.wf, CAP)})


@glyph("Delta", 0x0394)
def Delta(m):
    cs = m.c("A")
    l, r = cs[0], cs[1]
    # the bar's ends follow the diagonals' outer edges: no step at the corners
    lb = min((p for p in l if abs(p[1]) < 1), key=lambda p: p[0])
    lt = min((p for p in l if abs(p[1] - CAP) < 1), key=lambda p: p[0])
    rb = max((p for p in r if abs(p[1]) < 1), key=lambda p: p[0])
    rt = max((p for p in r if abs(p[1] - CAP) < 1), key=lambda p: p[0])
    eb = m.EB
    bar = poly((lb[0], 0), (rb[0], 0), (line_x(rb, rt, eb), eb), (line_x(lb, lt, eb), eb))
    return spec(m.width("A"), [l, r, bar], anchors={"top": m.anchor("A", "top")})


@glyph("Theta", 0x0398)
def Theta(m):
    cs = m.c("O")
    ib = m.O_in
    cy = (ib.y0 + ib.y1) / 2
    hb = m.HB * 0.96
    cs.append(rect(ib.x0 - 8, cy - hb / 2, ib.x1 + 8, cy + hb / 2))
    return spec(m.width("O"), cs, anchors={"top": m.anchor("O", "top")})


@glyph("Lambda", 0x039B)
def Lambda(m):
    cs = m.c("A")
    return spec(m.width("A"), [cs[0], cs[1]], anchors={"top": m.anchor("A", "top")})


@glyph("Xi", 0x039E)
def Xi(m):
    e = m.c("E")
    top, bot = Bounds(e[3]), Bounds(e[1])
    lsb = 64 * m.wf
    w = m.width("E") - 10 * m.wf
    inset = 0.11 * (w - 2 * lsb)
    eb = m.EB
    cs = [rect(lsb, 0, w - lsb + 4 * m.wf, eb),
          rect(lsb + inset, (CAP - eb * 0.96) / 2, w - lsb - inset, (CAP + eb * 0.96) / 2),
          rect(lsb + 6 * m.wf, CAP - eb, w - lsb - 2 * m.wf, CAP)]
    return spec(round(w), cs, anchors={"top": (w / 2, CAP)})


@glyph("Pi", 0x03A0)
def Pi(m):
    h = m.c("H")
    l, r = h[0], h[1]
    bl, br = Bounds(l), Bounds(r)
    return spec(m.width("H"), [l, r, rect(bl.x0 + m.SH / 2, CAP - m.EB, br.x1 - m.SH / 2, CAP)],
                anchors={"top": m.anchor("H", "top")})


@glyph("Sigma", 0x03A3)
def Sigma(m):
    z = m.c("Z", 0)
    zb = Bounds(z)
    left = zb.x0
    right_top = zb.x1 - 8 * m.wf
    right_bot = zb.x1
    eb = m.EB
    T = m.ZD * 0.98
    span = right_bot - left
    vx_in = left + 0.58 * span
    # solve for the diagonal slope s so the horizontal band equals T*sqrt(1+s^2)
    half = CAP / 2
    lo, hi = 0.0, 2.0
    for _ in range(60):
        s = (lo + hi) / 2
        width = (vx_in - left) - half * s
        if width > T * math.sqrt(1 + s * s):
            lo = s
        else:
            hi = s
    vx_out = left + s * half          # concave vertex on the left edge
    xt = vx_in - s * (CAP - eb - half)  # counter edge meets the top bar
    xb = vx_in - s * (CAP - eb - half)
    c = [P(left, 0), P(right_bot, 0), P(right_bot, eb), P(xb, eb), P(vx_in, half), P(xt, CAP - eb),
         P(right_top, CAP - eb), P(right_top, CAP), P(left, CAP), P(vx_out, half)]
    w = m.width("Z") - 4 * m.wf
    return spec(round(w), [ccw(c)], anchors={"top": (w / 2, CAP)})



# =====================================================================
# Lowercase drawn from skeletons
# =====================================================================


def term_y(y_ext, y_far, frac):
    """A cut between an extreme (y_ext) and the next extreme (y_far)."""
    return y_ext + (y_far - y_ext) * frac


def hook(s, m, xl, xr, body, yb):
    """The descender hook of ζ ξ ς: round the bottom, run down the right and
    curl back left under the baseline. The stroke thins towards its end and
    stops on the curve with a vertical cut, like the tail of j."""
    s.add(xl + 0.46 * (xr - xl), yb + 4, "h")
    s.add(xr - 0.03 * body, -68, "v")
    s.add(xl + 0.40 * (xr - xl), DESC + 0.78 * m.Ho / 2 + 4, "h", w=0.78)
    return s


def top_bar(m):
    """ζ and ξ's bar: a touch lighter than t's in the heavy masters, so the
    lobes under it keep their counters."""
    return m.TB * (0.86 if m.bold else 1.0)


def bar_join(m, spine, bar_l, tb):
    """Bar from bar_l to where the spine leaves it: the bar's right end is the
    spine's own right edge, so nothing sticks out past the join."""
    xs = [p[0] for p in spine if p[2] is not None and abs(p[1] - ASC) < 0.5]
    return rect(bar_l, ASC - tb, max(xs) - 2, ASC)


def bar_fillet(m, spine, tb):
    """A small round where the bar's underside meets the spine's left edge,
    so the inner corner is not a sharp notch (it fills in at heavy weights)."""
    yb = ASC - tb
    a, b = span_at(spine, yb - 4)[0], span_at(spine, yb - 64)[0]
    slope = (a - b) / 60.0                      # dx per unit y along the edge
    cx = a + slope * 4                          # the edge at the bar's underside
    r = (0.12 + 0.30 * m.bold) * m.S          # only the heavy masters need much of it
    dy = r / math.hypot(1, slope)               # r along the edge
    A = (cx - r, yb)
    B = (cx - slope * dy, yb - dy)
    k = 0.55
    return ccw([P(*A, "curve"), P(cx + 2, yb + 2), P(*B),
                P(B[0] + (cx - B[0]) * k, B[1] + (yb - B[1]) * k, None),
                P(A[0] + (cx - A[0]) * k, yb, None)])


TONGUE_CUT = 100  # degrees: the tongue of ε and ξ ends on a cut leaning back a little


@glyph("epsilon", 0x03B5)
def epsilon(m):
    f = m.wf
    lsb = m.lsb_o + 6 * f
    body = 0.82 * m.o_out.w + 0.18 * m.So + 0.4 * (m.So - 82 * m.wf)
    x0, x1 = lsb, lsb + body
    yb = -OV + m.Ho / 2
    yt = X + OV - m.Ho / 2
    # the waist sits low enough that the smaller upper bowl keeps a counter;
    # the heavy masters drop it further, their strokes take more of the bowl
    yw = 284 - (12 if m.bold else 0) - (8 if m.cond and m.bold else 0)
    lo_w = 0.92 if m.bold else 1.0   # the heavy bowls a little lighter than the stem
    up_w = (0.74 if m.cond else 0.80) if m.bold else 0.92  # and the upper one lighter still
    waist = 0.62 if m.bold else 0.76
    xll, xrl = x0 + m.So / 2, x1 - m.So / 2
    xcl = (xll + xrl) / 2 + 4 * f
    yl = (yw + yb) / 2
    # the tongue is where the two bowls part, run on a little and cut on a
    # slant: a join, not a bar (not a turned 3)
    xt = xll + 0.40 * (xrl - xll) + 0.35 * (m.So - 82 * f)
    run = 0.6 * m.Ho
    cut = ("line", (xt, yw), TONGUE_CUT)
    lo = stroke(m)
    lo.add(xt + run, yw, "h", w=waist).add(xll, yl, "v", w=lo_w).add(xcl, yb, "h").add(xrl, yl, "v", w=lo_w)
    low = lo.outline(start_cut=cut, end_cut=("y", term_y(yl, yb + m.Ho / 2, 0.42)))
    # upper bowl: narrower, set in from both sides, opening wider at the top
    xlu = xll + 0.06 * body
    xru = xrl - 0.09 * body
    xcu = (xlu + xru) / 2 + 4 * f
    yu = (yw + yt) / 2 + 4
    up = stroke(m)
    up.add(xru, yu, "v", w=up_w).add(xcu, yt, "h", w=up_w).add(xlu, yu, "v", w=up_w)
    up.add(xt + run, yw, "h", w=waist)
    upper = up.outline(start_cut=("y", term_y(yu, yt - m.Ho / 2, 0.58)), end_cut=cut)
    w = x1 + m.lsb_o + 8 * f
    return spec(round(w), [low, upper], anchors={"top": (xcu, X)})


@glyph("sigmafinal", 0x03C2)
def sigmafinal(m):
    f = m.wf
    lsb = m.lsb_o
    body = 0.86 * m.o_out.w + 0.14 * m.So
    x0, x1 = lsb, lsb + body
    xl, xr = x0 + m.So / 2, x1 - m.So / 2
    xc = (xl + xr) / 2 + 6 * f
    yt = X + OV - m.Ho / 2
    yb = -OV + m.Ho / 2
    yc = 262
    s = stroke(m)
    s.add(xr, yc, "v").add(xc, yt, "h").add(xl, yc - 20, "v")
    hook(s, m, xl, xr, body, yb + 2)
    c = s.outline(start_cut=("y", term_y(yc, yt - m.Ho / 2, 0.42)))
    w = x1 + 26 * f
    return spec(round(w), [c], anchors={"top": (xc, X)})


def spine_start(m, x0, body):
    """Where the ζ/ξ spine leaves the bar: near its right end, heading down
    and left, a little lighter than the stroke so the join stays open."""
    return (x0 + 0.88 * body, ASC + 70 + 0.25 * m.So)


@glyph("zeta", 0x03B6)
def zeta(m):
    f = m.wf
    lsb = m.lsb_o + 10 * f
    body = 0.80 * m.o_out.w + 0.2 * m.So
    x0, x1 = lsb, lsb + body
    xl, xr = x0 + m.So / 2, x1 - m.So / 2
    tb = top_bar(m)
    yb = -OV + m.Ho / 2
    sx, sy = spine_start(m, x0, body)
    s = stroke(m)
    # the spine stays a diagonal well down the x-height and turns late into
    # the bowl, still leaning as it passes the left side: no upright wall
    s.add(sx, sy, -128 if m.cond else -132, k=0.34, w=0.84)
    s.add(xl + 0.02 * body, 176 if m.cond else 196, -98, k=0.58, w=0.94 if m.bold else 1.0)
    hook(s, m, xl, xr, body, yb)
    c = s.outline(start_cut=("y", ASC))
    bar = bar_join(m, c, x0 + 0.04 * body, tb)
    w = x1 + 22 * f
    return spec(round(w), [bar, c, bar_fillet(m, c, tb)], anchors={"top": ((x0 + x1) / 2, ASC + 20)})


@glyph("xi", 0x03BE)
def xi(m):
    f = m.wf
    lsb = m.lsb_o + 10 * f
    body = 0.80 * m.o_out.w + 0.2 * m.So
    x0, x1 = lsb, lsb + body
    xl, xr = x0 + m.So / 2, x1 - m.So / 2
    tb = top_bar(m)
    yb = -OV + m.Ho / 2
    # the heavy masters set the waist lower and the strokes round the upper
    # lobe lighter, so its counter stays open
    yw = 404 + (m.Ho - 68) * 0.3 - (26 if m.bold else 0)
    tongue = 0.62 if m.bold else 0.76
    lobe = (0.80 if m.cond else 0.84) if m.bold else 1.0
    xt = xl + 0.50 * (xr - xl) + 0.35 * (m.So - 82 * f)
    run = 0.6 * m.Ho
    cut = ("line", (xt, yw), TONGUE_CUT)
    sx, sy = spine_start(m, x0, body)
    # upper lobe: leaves the bar near its right end like ζ's spine, turns at
    # the left and runs back into the tongue; the lower bowl leaves the
    # tongue the same way ε's does, so the waist is one turning stroke
    up = stroke(m)
    up.add(sx, sy, -138 if m.cond else -142, k=0.30, w=0.82)
    up.add(xl + (0.0 if m.bold else 0.03) * body, (ASC - tb + yw) / 2 - 4, "v", w=lobe)
    up.add(xt + run, yw, "h", w=tongue)
    upper = up.outline(start_cut=("y", ASC), end_cut=cut)
    lo = stroke(m)
    lo.add(xt + run, yw, "h", w=tongue).add(xl, 196, "v")
    hook(lo, m, xl, xr, body, yb)
    bar = bar_join(m, upper, x0 + 0.04 * body, tb)
    w = x1 + 10 * f  # the open right side needs less room than c's
    return spec(round(w), [bar, upper, lo.outline(start_cut=cut)], anchors={"top": ((x0 + x1) / 2, ASC + 20)})


@glyph("beta", 0x03B2)
def beta(m):
    """One stroke: the stem rises and rolls over into the upper bowl, which
    pinches in at the waist; the larger lower bowl swings out from the waist
    and runs back into the stem."""
    f = m.wf
    lsb = m.lsb_n
    xs = lsb + m.So / 2
    V = m.So * (0.91 if m.bold else 1.0)
    top = ASC + OV - m.Ho / 2
    yb = -OV + m.Ho / 2
    yw = 404 - (10 if m.bold else 0) - (16 if m.cond and m.bold else 0)
    xr_l = lsb + 0.80 * m.o_out.w + 0.2 * m.So + 0.55 * (m.So - 82 * f) + (14 * f if m.cond and m.bold else 0)
    xr_u = xs + (0.92 if m.cond and m.bold else 0.88 if m.bold else 0.82) * (xr_l - xs)
    pinch = (0.58 if m.cond and m.bold else 0.64) * m.S
    xw = xs + 0.40 * (xr_u - xs)
    Vu = m.So * ((0.78 if m.cond else 0.84) if m.bold else 1.0)  # the small upper bowl lighter still
    a = stroke(m, V=Vu, H=m.Ho * (0.92 if m.bold else 1.0))
    a.add(xs, DESC, "v", w=m.So / Vu).add(xs, top - 0.42 * (top - yw), "v", seg="line", w=m.So / Vu)
    a.add((xs + xr_u - V / 2) / 2, top, "h")
    a.add(xr_u - Vu / 2, (top + yw) / 2 + 10, "v")
    a.add(xw, yw, "h", wl=pinch, wr=pinch).add(xs, yw, "h", seg="line", wl=pinch, wr=pinch)
    b = stroke(m, V=V)
    b.add(xs, yw, "h", wl=pinch, wr=pinch).add(xw + 6 * f, yw, "h", seg="line", wl=pinch, wr=pinch)
    b.add(xr_l - V / 2, (yw + yb) / 2 - 6, "v")
    b.add((xs + xr_l - V / 2) / 2 + 4 * f, yb, "h")
    # back into the stem a little lighter than the bowl: the join stays clean
    b.add(xs + 0.05 * m.So, 190, "v", w=0.86)
    w = xr_l + m.lsb_o + 4 * f
    return spec(round(w), [a.outline(), b.outline()], anchors={"top": ((xs + xr_l) / 2, ASC + 20)})


@glyph("delta", 0x03B4)
def delta(m):
    f = m.wf
    lsb = m.lsb_o
    body = 0.93 * m.o_out.w + 0.07 * m.So
    x0, x1 = lsb, lsb + body
    bowl = ring(m, x0, -OV, x1, X + OV)
    xc = (x0 + x1) / 2
    yt = X + OV - m.Ho / 2
    top = ASC + OV - m.Ho / 2
    s = stroke(m)
    # the neck leaves the bowl thinned, so the S-join doesn't knot in Bold
    s.add(xc + 10 * f, yt, "h", w=0.72 if m.bold else 0.82)
    s.add(x0 + 0.20 * body, (yt + top) / 2 + 4, "v", w=0.88 if m.bold else 0.95)
    s.add(x0 + 0.48 * body, top, "h").add(x0 + 0.80 * body, top, "h", seg="line")
    w = x1 + lsb
    return spec(round(w), bowl + [s.outline()], anchors={"top": (xc, ASC + 20)})


@glyph("theta", 0x03B8)
def theta(m):
    f = m.wf
    lsb = m.lsb_o + 4 * f
    body = (0.88 + (0.08 if m.cond and m.bold else 0)) * m.o_out.w + 0.12 * m.So
    x0, x1 = lsb, lsb + body
    cb = m.cond and m.bold
    V = m.So * (0.90 if cb else 1.0)
    cs = ring(m, x0, -OV, x1, ASC + OV, V=V)
    # the bar is a horizontal like e's: lighter than the bowl, and a little
    # under the middle in the heavy masters so both counters look alike
    cy = ASC / 2 - (8 if m.bold else 0)
    hb = m.Ho * ((0.66 if cb else 0.74) if m.bold else 0.92)
    cs.append(rect(x0 + V - 6, cy - hb / 2, x1 - V + 6, cy + hb / 2))
    w = x1 + lsb
    return spec(round(w), cs, anchors={"top": ((x0 + x1) / 2, ASC + 20)})


def phi_bowl(m, caps=False):
    """φ/Φ's bowl is wider than o so the counters either side of the stem stay
    open, and lighter top and bottom where the stem crosses it."""
    wide = 1.0 + 0.10 * m.bold + 0.06 * m.cond + 0.08 * (m.cond and m.bold)
    V = (m.SO if caps else m.So) * (0.92 if m.bold else 1.0)
    H = (m.HO if caps else m.Ho) * (0.86 if m.bold else 0.94)
    return wide, V, H


@glyph("phi", 0x03C6)
def phi(m):
    f = m.wf
    lsb = m.lsb_o
    wide, V, H = phi_bowl(m)
    body = 1.10 * wide * m.o_out.w
    x0, x1 = lsb, lsb + body
    cs = ring(m, x0, -OV, x1, X + OV, V=V, H=H)
    xc = (x0 + x1) / 2
    inner = m.S * (0.80 if m.bold else 0.94)
    cs.append(rect(xc - m.S / 2, DESC, xc + m.S / 2, -OV + H / 2))
    cs.append(rect(xc - inner / 2, -OV + H / 4, xc + inner / 2, X + OV - H / 4))
    cs.append(rect(xc - m.S / 2, X + OV - H / 2, xc + m.S / 2, ASC))
    w = x1 + lsb
    return spec(round(w), cs, anchors={"top": (xc, ASC + 20)})


def u_shape(m, xl, xr, ytop, y_turn, ybot, caps=False, bottom_w=1.0, V=None):
    s = stroke(m, caps, V=V)
    cx = (xl + xr) / 2
    s.add(xl, ytop, "v").add(xl, y_turn, "v", seg="line").add(cx, ybot, "h", w=bottom_w)
    s.add(xr, y_turn, "v").add(xr, ytop, "v", seg="line")
    return s.outline()


@glyph("psi", 0x03C8)
def psi(m):
    f = m.wf
    lsb = (m.lsb_o + m.lsb_n) / 2
    # the heavy masters widen so the counters beside the stem stay open
    body = (1.06 + 0.13 * m.bold + 0.12 * (m.cond and m.bold)) * m.o_out.w
    V = m.So * (0.92 if m.bold else 1.0)
    xl, xr = lsb + V / 2, lsb + body - V / 2
    xc = (xl + xr) / 2
    u = u_shape(m, xl, xr, X, 250, -OV + m.Ho / 2, bottom_w=0.80 if m.bold else 0.9, V=V)
    st = m.S * (0.84 if m.bold else 0.96)
    stem = rect(xc - st / 2, DESC, xc + st / 2, ASC)
    w = lsb * 2 + body
    return spec(round(w), [u, stem], anchors={"top": (xc, ASC + 20)})


@glyph("omega", 0x03C9)
def omega(m):
    f = m.wf
    lsb = m.lsb_o + 6 * f
    body = (1.36 + 0.08 * (m.cond and m.bold)) * m.o_out.w
    xl, xr = lsb + m.So / 2, lsb + body - m.So / 2
    xc = (xl + xr) / 2
    yb = -OV + m.Ho / 2
    # the middle stroke is short, light and set high: the two bowls read as
    # one letter, not as a turned m
    mid_top = 345 - (10 if m.bold else 0)
    sw = m.S * (0.68 if m.bold else 0.76)
    bw = 0.84 if m.bold else 0.94
    a = stroke(m)
    counter = (xr - xl) / 2 - m.So / 2 - sw / 2
    arm_top(a, xl, -1, m, True, counter)
    a.add(xl, 240, "v", seg="line").add((xl + xc) / 2 - 4 * f, yb, "h", w=bw)
    a.add(xc, 170, "v", wl=sw, wr=sw).add(xc, mid_top, "v", seg="line", wl=sw, wr=sw)
    b = stroke(m)
    b.add(xc, mid_top, "v", wl=sw, wr=sw).add(xc, 170, "v", seg="line", wl=sw, wr=sw)
    b.add((xc + xr) / 2 + 4 * f, yb, "h", w=bw).add(xr, 240, "v")
    arm_top(b, xr, 1, m, False, counter)
    b.nodes[-2]["seg"] = "line"
    w = lsb * 2 + body
    return spec(round(w), [a.outline(start_cut=("y", X)), b.outline(end_cut=("y", X))], anchors={"top": (xc, X)})


@glyph("nu", 0x03BD)
def nu(m):
    """v whose right arm bows out and stands up straight at the top."""
    v = m.c("v", 0)
    bl, br, tro, tri, c1, c2, tli, tlo = [p[:2] for p in v]
    w = m.width("v")
    a, b = 0.62, 0.36
    def ctl(p, q):
        # p: bottom end, q: top end of an edge
        return (p[0] + a * (q[0] - p[0]), p[1] + b * (q[1] - p[1])), (q[0], q[1] - 0.42 * (q[1] - p[1]))
    o1, o2 = ctl(br, tro)
    i1, i2 = ctl(c1, tri)
    c = [P(*bl), P(*br), P(*o1, None), P(*o2, None), P(*tro, "curve"), P(*tri),
         P(*i2, None), P(*i1, None), P(*c1, "curve"), P(*c2), P(*tli), P(*tlo)]
    return spec(w, [ccw(c)], anchors={"top": (w / 2, X)})


# =====================================================================
# Uppercase drawn from skeletons
# =====================================================================


@glyph("Phi", 0x03A6)
def Phi(m):
    f = m.wf
    lsb = m.O_out.x0 - 4 * f
    wide, V, H = phi_bowl(m, caps=True)
    body = 1.04 * wide * m.O_out.w
    x0, x1 = lsb, lsb + body
    cs = ring(m, x0, 92, x1, 628, V=V, H=H, caps=True)
    xc = (x0 + x1) / 2
    inner = m.SH * (0.80 if m.bold else 0.94)
    cs.append(rect(xc - m.SH / 2, 0, xc + m.SH / 2, 92 + H / 2))
    cs.append(rect(xc - inner / 2, 92 + H / 4, xc + inner / 2, 628 - H / 4))
    cs.append(rect(xc - m.SH / 2, 628 - H / 2, xc + m.SH / 2, CAP))
    w = x1 + lsb
    return spec(round(w), cs, anchors={"top": (xc, CAP)})


@glyph("Psi", 0x03A8)
def Psi(m):
    f = m.wf
    lsb = m.lsb_H - 10 * f
    body = (0.98 + 0.04 * m.bold + 0.16 * (m.cond and m.bold)) * m.O_out.w
    V = m.SO * ((0.86 if m.cond else 0.92) if m.bold else 1.0)
    xl, xr = lsb + V / 2, lsb + body - V / 2
    xc = (xl + xr) / 2
    u = u_shape(m, xl, xr, CAP, 420, 196, caps=True, bottom_w=0.80 if m.bold else 0.9, V=V)
    st = m.SH * ((0.84 if m.cond else 0.92) if m.bold else 1.0)
    stem = rect(xc - st / 2, 0, xc + st / 2, CAP)
    w = lsb * 2 + body
    return spec(round(w), [u, stem], anchors={"top": (xc, CAP)})


@glyph("Omega", 0x03A9)
def Omega(m):
    """O's bowl, open at the bottom: its sides turn in below the middle and
    run on, still curving, into the feet. The legs taper a little from the
    bowl and never stand upright, so there is no knee and no swell."""
    f = m.wf
    lsb = m.O_out.x0
    body = m.O_out.w
    x0, x1 = lsb, lsb + body
    xl, xr = x0 + m.SO / 2, x1 - m.SO / 2
    xc = (xl + xr) / 2
    top = CAP + OV - m.HO / 2
    leg = 0.86 if m.bold else 0.92
    # the feet stand a third of the counter apart, in every width
    gap = 0.36 * (body - 2 * m.SO)
    lw = leg * m.SO
    xa, xb = xc - (gap + lw) / 2, xc + (gap + lw) / 2
    ymid = 330 + 30 * m.cond
    # the legs reach the feet leaning in, as far as the bowl has come in
    lean = 0.62 * math.degrees(math.atan((xa - xl) / ymid))
    ext = 30
    dx = ext * math.tan(math.radians(lean))
    s = stroke(m, caps=True)
    s.add(xa + dx, -ext, 90 + lean, w=leg, k=0.50)
    s.add(xl, ymid, "v", k=0.56)
    s.add(xc, top, "h").add(xr, ymid, "v", k=0.56)
    s.add(xb - dx, -ext, 90 - lean, w=leg, k=0.50)
    arc = s.outline(start_cut=("y", 0), end_cut=("y", 0))
    xs = crossings(arc, 0.5)
    foot = m.EB
    fl = rect(x0 + 6 * f, 0, xs[1], foot)
    fr = rect(xs[-2], 0, x1 - 6 * f, foot)
    w = x1 + lsb
    return spec(round(w), [arc, fl, fr], anchors={"top": (xc, CAP)})


# =====================================================================
# Tonos, dialytika, punctuation
# =====================================================================

TONOS_BOTTOM = 583


def tonos_shape(m):
    """Instrument's acute, stood up: steeper, a little taller, same weight."""
    a = m.c("acutecomb", 0)
    y0 = min(p[1] for p in a)
    x_left_bottom = min(p[0] for p in a if abs(p[1] - y0) < 1)
    out = []
    for x, y, t, s in a:
        dy = y - y0
        out.append([x - dy * 0.42, y0 + dy * 1.10, t, s])
    out = translate(out, -x_left_bottom)
    return out  # bottom-left corner at x=0, y=TONOS_BOTTOM


def tonos_metrics(m):
    c = tonos_shape(m)
    b = Bounds(c)
    ymid = (b.y0 + b.y1) / 2
    return b, sum(span_at(c, ymid)) / 2


@glyph("tonos", 0x0384)
def tonos(m):
    c = tonos_shape(m)
    b = Bounds(c)
    sb = 60 * m.wf
    return spec(round(b.w + 2 * sb), [translate(c, sb - b.x0)])


def top_anchor(m, name):
    fn, _ = GLYPHS[name] if name in GLYPHS else (None, None)
    if fn is not None:
        g = fn(m)
        if "top" in g["anchors"]:
            return g["anchors"]["top"]
        base = g["components"][0][0]
        return m.anchor(base, "top")
    return m.anchor(name, "top")


def glyph_width(m, name):
    if name in GLYPHS:
        return GLYPHS[name][0](m)["width"]
    return m.width(name)


def tonos_component_offset(m, x_shape_left, dy):
    """Offset for the tonos glyph so its shape's left edge lands at x."""
    t = tonos(m)
    lsb = Bounds(t["contours"][0]).x0
    return x_shape_left - lsb, dy


# ---------------------------------------------------------------- outlines


def outline(m, name):
    """Every contour of a glyph, components resolved, in upright space."""
    if name in GLYPHS:
        g = GLYPHS[name][0](m)
        cs = [copy.deepcopy(c) for c in g["contours"]]
        for base, dx, dy in g["components"]:
            cs += [translate(c, dx, dy) for c in outline(m, base)]
        return cs
    cs = m.c(name)
    for comp in m.font[name].components:
        dx, dy = comp.transformation[4], comp.transformation[5]
        if m.italic:
            dx -= dy * TAN  # component offsets are sheared with the glyph
        cs += [translate(c, dx, dy) for c in outline(m, comp.baseGlyph)]
    return cs


def flatten(contour, steps=12):
    """Polygon through a contour, curves sampled."""
    out = []
    for kind, pts in segments(contour):
        if kind == "curve" and len(pts) == 4:
            p0, p1, p2, p3 = pts
            for i in range(1, steps + 1):
                t = i / steps
                mt = 1 - t
                out.append((mt ** 3 * p0[0] + 3 * mt * mt * t * p1[0] + 3 * mt * t * t * p2[0] + t ** 3 * p3[0],
                            mt ** 3 * p0[1] + 3 * mt * mt * t * p1[1] + 3 * mt * t * t * p2[1] + t ** 3 * p3[1]))
        else:
            out.append(tuple(pts[-1][:2]))
    return out


def crossings(contours, y):
    """Every x where the outline crosses the horizontal line y, sorted."""
    if contours and isinstance(contours[0][0], (int, float)):
        contours = [contours]
    xs = []
    for c in contours:
        poly_ = flatten(c)
        n = len(poly_)
        for i in range(n):
            (x0, y0), (x1, y1) = poly_[i], poly_[(i + 1) % n]
            if (y0 - y) * (y1 - y) <= 0 and y0 != y1:
                xs.append(x0 + (y - y0) * (x1 - x0) / (y1 - y0))
    return sorted(xs)


def span_at(contours, y):
    """(leftmost, rightmost) ink of the outline on the horizontal line y, or None."""
    if contours and isinstance(contours[0][0], (int, float)):
        contours = [contours]
    xs = []
    for c in contours:
        poly_ = flatten(c)
        n = len(poly_)
        for i in range(n):
            (x0, y0), (x1, y1) = poly_[i], poly_[(i + 1) % n]
            if (y0 - y) * (y1 - y) <= 0 and y0 != y1:
                xs.append(x0 + (y - y0) * (x1 - x0) / (y1 - y0))
    return (min(xs), max(xs)) if xs else None


def ink_x(contours):
    bs = [Bounds(c) for c in contours]
    return min(b.x0 for b in bs), max(b.x1 for b in bs)


def accent_room(m, base, x0, x1):
    """Accents may hang at most ~36 units past the advance on either side; a
    wider one gets the room from extra sidebearing. Returns (shift, width)."""
    w = glyph_width(m, base)
    lim = 36 * m.wf
    left = max(0.0, -lim - x0)
    right = max(0.0, x1 + left - (w + left + lim))
    return left, w + left + right


# ---------------------------------------------------------------- lowercase tonos

for _n, _b, _u in [("alphatonos", "alpha", 0x03AC), ("epsilontonos", "epsilon", 0x03AD), ("etatonos", "eta", 0x03AE),
                   ("iotatonos", "iota", 0x03AF), ("omicrontonos", "omicron", 0x03CC),
                   ("upsilontonos", "upsilon", 0x03CD), ("omegatonos", "omega", 0x03CE)]:
    def _mk(base):
        def f(m):
            b, xc = tonos_metrics(m)
            ax, ay = top_anchor(m, base)
            dx, dy = tonos_component_offset(m, ax - xc + 12 * m.wf, 0)
            return spec(glyph_width(m, base), components=[(base, 0, 0), ("tonos", dx, dy)])
        return f
    glyph(_n, _u)(_mk(_b))


# ---------------------------------------------------------------- dialytika

def dieresis_offset(m, base):
    ax, ay = top_anchor(m, base)
    dx_, dy_ = m.anchor("dieresiscomb", "_top")
    return ax - dx_, ay - dy_


def dieresis_glyph(base, ss01=False):
    mark = "dieresiscomb.ss01" if ss01 else "dieresiscomb"

    def f(m):
        dx, dy = dieresis_offset(m, base)
        # measured on the square dots, so both sets space alike
        x0, x1 = ink_x([translate(c, dx, dy) for c in m.c("dieresiscomb")])
        shift, w = accent_room(m, base, x0, x1)
        return spec(round(w), components=[(base, shift, 0), (mark, dx + shift, dy)])
    return f


for _n, _b, _u in [("iotadieresis", "iota", 0x03CA), ("upsilondieresis", "upsilon", 0x03CB),
                   ("Iotadieresis", "Iota", 0x03AA), ("Upsilondieresis", "Upsilon", 0x03AB)]:
    glyph(_n, _u)(dieresis_glyph(_b))
    glyph(_n + ".ss01")(dieresis_glyph(_b, ss01=True))


def dialytika_tonos(m):
    """The tonos between the two dots of the dialytika, centred on x=0.

    The dots keep the dieresis spacing where they can (at most ~15% wider);
    the tonos stands up between them, narrower and steeper as the weight
    grows, its foot dipping between the tops of the dots. Returns the tonos
    contour and the two dot offsets (for dotaccentcomb)."""
    dots = sorted((Bounds(c) for c in m.c("dieresiscomb")), key=lambda b: b.x0)
    L, R = dots
    gap, dot_h = R.x0 - L.x1, L.y1 - L.y0
    clear = 0.18 * L.w
    base = tonos_shape(m)
    found = None
    for i in range(9):
        f = 1.0 - 0.05 * i
        shape = transform(base, lambda x, y, f=f: (x * f, TONOS_BOTTOM + (y - TONOS_BOTTOM) * 0.9))
        for yb in range(round(L.y0 + 0.3 * dot_h), round(L.y1 - 0.2 * dot_h) + 1, 4):
            sh = translate(shape, 0, yb - TONOS_BOTTOM)
            lo = span_at(sh, yb + 0.5)
            hi = span_at(sh, L.y1)
            need = max(lo[1], hi[1]) - min(lo[0], hi[0]) + 2 * clear
            if need <= 1.15 * gap:
                found = (sh, min(lo[0], hi[0]), max(lo[1], hi[1]), need)
                break
        if found:
            break
    if not found:
        sh = translate(shape, 0, round(L.y1 - 0.2 * dot_h) - TONOS_BOTTOM)
        lo, hi = span_at(sh, sh_y0(sh) + 0.5), span_at(sh, L.y1)
        found = (sh, min(lo[0], hi[0]), max(lo[1], hi[1]), max(lo[1], hi[1]) - min(lo[0], hi[0]) + 2 * clear)
    sh, a, b, need = found
    g = max(gap, need)
    sh = translate(sh, -(a + b) / 2, 0)
    D = Bounds(m.c("dotaccentcomb", 0))
    dy = L.y0 - D.y0
    return sh, (-g / 2 - D.x1, dy), (g / 2 - D.x0, dy)


def sh_y0(c):
    return min(p[1] for p in c)


def dieresistonos_parts(m, ss01=False):
    sh, l, r = dialytika_tonos(m)
    dot = "dotaccentcomb.ss01" if ss01 else "dotaccentcomb"
    return sh, [(dot, l[0], l[1]), (dot, r[0], r[1])]


def dieresistonos_ink(m):
    sh, comps = dieresistonos_parts(m)
    cs = [sh] + [translate(c, dx, dy) for _, dx, dy in comps for c in m.c("dotaccentcomb")]
    return ink_x(cs)


def dieresistonos_glyph(ss01=False):
    def f(m):
        sh, comps = dieresistonos_parts(m, ss01)
        x0, x1 = dieresistonos_ink(m)
        sb = 50 * m.wf
        d = sb - x0
        # the dots are drawn in, not referenced: Condensed Bold's heavy square
        # dots crowd the tonos between them, so they shrink a little there
        k = 0.92 if m.cond and m.bold else 1.0
        dots = []
        for n, dx, dy in comps:
            for c in m.c(n):
                b = Bounds(c)
                cx, cy = (b.x0 + b.x1) / 2, (b.y0 + b.y1) / 2
                c = transform(c, lambda x, y: (cx + (x - cx) * k, cy + (y - cy) * k))
                dots.append(translate(c, dx + d, dy))
        return spec(round(x1 - x0 + 2 * sb), [translate(sh, d, 0)] + dots)
    return f


glyph("dieresistonos", 0x0385)(dieresistonos_glyph())
glyph("dieresistonos.ss01")(dieresistonos_glyph(ss01=True))


def dialytika_tonos_letter(base, ss01=False):
    acc = "dieresistonos.ss01" if ss01 else "dieresistonos"

    def f(m):
        ax, _ = top_anchor(m, base)
        x0, x1 = dieresistonos_ink(m)
        shift, w = accent_room(m, base, ax + x0, ax + x1)
        d = 50 * m.wf - x0  # where the spacing glyph put the axis
        return spec(round(w), components=[(base, shift, 0), (acc, ax + shift - d, 0)])
    return f


for _n, _b, _u in [("iotadieresistonos", "iota", 0x0390), ("upsilondieresistonos", "upsilon", 0x03B0)]:
    glyph(_n, _u)(dialytika_tonos_letter(_b))
    glyph(_n + ".ss01")(dialytika_tonos_letter(_b, ss01=True))


# ---------------------------------------------------------------- capitals with tonos

CAP_TONOS_TOP = 736
UPSILON_TONOS_DROP = 58


def cap_tonos_shift(m, base, sb, dy, ref=None):
    """How far right the letter moves behind its tonos. The tonos leans, so
    even beside Ε's flat stem the white between them is a wedge; it is
    measured across the tonos' height. Ε sets the reference: its stem comes
    to 0.7 of the base gap from the tonos' top corner. With `ref`, the letter
    instead closes in until the mean white matches Ε's (`ref`), so Υ's arm,
    which runs away from the tonos, holds the same white as Ε, though its
    nearest point comes closer (never under 0.35 of the base gap)."""
    sh = translate(tonos_shape(m), sb, dy)
    tb = Bounds(sh)
    letter = outline(m, base)
    gap0 = 56 * m.wf + (m.S - 80 * m.wf) * 0.25
    ys = [tb.y0 + (min(tb.y1, CAP) - tb.y0) * (i + 0.5) / 12 for i in range(12)]
    pairs = []
    for y in ys:
        t, l = span_at(sh, y), span_at(letter, y)
        if t and l:
            pairs.append((t[1], l[0]))
    cap = 2.2 * gap0

    def mean_gap(shift):
        return sum(min(l + shift - t, cap) for t, l in pairs) / len(pairs)

    def worst(shift):
        return min(l + shift - t for t, l in pairs)

    if ref is None:
        # Ε Η Ι Ο: the nearest point at 0.7 of the base gap. Ω's arch turns
        # away from the tonos faster than Ο's bowl: it comes closer.
        near = 0.5 if base == "Omega" else 0.7
        return max(0.0, near * gap0 - worst(0.0))
    target = ref
    lo, hi = -300.0, 600.0
    for _ in range(50):
        mid = (lo + hi) / 2
        if mean_gap(mid) < target:
            lo = mid
        else:
            hi = mid
    shift = hi
    if worst(shift) < 0.35 * gap0:
        shift += 0.35 * gap0 - worst(shift)
    return max(0.0, shift)


def cap_tonos_ref(m, sb, dy):
    """The mean white Ε keeps beside its tonos."""
    sh = translate(tonos_shape(m), sb, dy)
    tb = Bounds(sh)
    shift = cap_tonos_shift(m, "Epsilon", sb, dy)
    letter = translate_all(outline(m, "Epsilon"), shift)
    gap0 = 56 * m.wf + (m.S - 80 * m.wf) * 0.25
    ys = [tb.y0 + (min(tb.y1, CAP) - tb.y0) * (i + 0.5) / 12 for i in range(12)]
    gs = [min(span_at(letter, y)[0] - span_at(sh, y)[1], 2.2 * gap0) for y in ys if span_at(letter, y)]
    return sum(gs) / len(gs)


def translate_all(cs, dx):
    return [translate(c, dx) for c in cs]


for _n, _b, _u in [("Alphatonos", "Alpha", 0x0386), ("Epsilontonos", "Epsilon", 0x0388),
                   ("Etatonos", "Eta", 0x0389), ("Iotatonos", "Iota", 0x038A),
                   ("Omicrontonos", "Omicron", 0x038C), ("Upsilontonos", "Upsilon", 0x038E),
                   ("Omegatonos", "Omega", 0x038F)]:
    def _mk(base):
        def f(m):
            b, xc = tonos_metrics(m)
            dy = CAP_TONOS_TOP - b.y1
            sb = 40 * m.wf
            dx, _ = tonos_component_offset(m, sb, dy)
            # letters whose side falls away under the tonos (Υ's arm, Α's
            # diagonal) match Ε's mean white instead
            ref = cap_tonos_ref(m, sb, dy) if base in ("Upsilon", "Alpha") else None
            if base == "Upsilon":
                # Υ's arm tip stands at cap height and its side runs away
                # below: the tonos drops beside the arm, its top under the
                # tip, so the letter can come in to Ε's white
                dy -= UPSILON_TONOS_DROP
            shift = cap_tonos_shift(m, base, sb, dy, ref)
            return spec(round(glyph_width(m, base) + shift), components=[("tonos", dx, dy), (base, shift, 0)])
        return f
    glyph(_n, _u)(_mk(_b))


# ---------------------------------------------------------------- punctuation

def anoteleia_glyph(period):
    def f(m):
        b = Bounds(m.c("period", 0))
        return spec(m.width("period"), components=[(period, 0, X - b.y1)])
    return f


glyph("anoteleia", 0x0387)(anoteleia_glyph("period"))
glyph("anoteleia.ss01")(anoteleia_glyph("period.ss01"))


@glyph("questiongreek", 0x037E)
def questiongreek(m):
    return spec(m.width("semicolon"), components=[("semicolon", 0, 0)])


@glyph("questiongreek.ss01")
def questiongreek_ss01(m):
    return spec(m.width("semicolon.ss01"), components=[("semicolon.ss01", 0, 0)])


@glyph("numeral-greek", 0x0374)
def numeral_greek(m):
    t = tonos(m)
    return spec(t["width"] - 30 * m.wf, components=[("tonos", -15 * m.wf, CAP_TONOS_TOP - 760)])


@glyph("lowernumeral-greek", 0x0375)
def lowernumeral_greek(m):
    t = tonos(m)
    return spec(t["width"] - 30 * m.wf, components=[("tonos", -15 * m.wf, -TONOS_BOTTOM - 150)])


# ---------------------------------------------------------------- symbols beside the Greek

@glyph("micro", 0x00B5)
def micro(m):
    """MICRO SIGN (µs, µm): the Greek μ."""
    return spec(glyph_width(m, "mu"), components=[("mu", 0, 0)])


@glyph("horizontalbar", 0x2015)
def horizontalbar(m):
    """HORIZONTAL BAR, the Greek dialogue dash: the em dash."""
    return spec(m.width("emdash"), components=[("emdash", 0, 0)])


@glyph("horizontalbar.case")
def horizontalbar_case(m):
    return spec(m.width("emdash.case"), components=[("emdash.case", 0, 0)])


# =====================================================================


def build(m, name):
    fn, uni = GLYPHS[name]
    g = fn(m)
    g["unicode"] = uni
    return g


class GlifObj:
    def __init__(self, width, unicodes, anchors, lib=None):
        self.width = width
        self.unicodes = unicodes
        self.anchors = anchors
        self.lib = lib or {}


def write(m, names, gs):
    for name in names:
        g = build(m, name)
        contours = g["contours"]
        comps = g["components"]
        anchors = g["anchors"]
        if m.italic:
            contours = [transform(c, lambda x, y: skew(x, y, 1)) for c in contours]
            comps = [(b, dx + dy * TAN, dy) for b, dx, dy in comps]
            anchors = {k: skew(v[0], v[1], 1) for k, v in anchors.items()}
        contours = [rounded(c) for c in contours]
        anchor_list = [dict(name=k, x=round(v[0]), y=round(v[1])) for k, v in sorted(anchors.items())]

        def draw(pen, contours=contours, comps=comps):
            for c in contours:
                pen.beginPath()
                for x, y, t, s in c:
                    pen.addPoint((x, y), segmentType=t, smooth=bool(s and t))
                pen.endPath()
            for b, dx, dy in comps:
                pen.addComponent(b, (1, 0, 0, 1, round(dx), round(dy)))

        obj = GlifObj(round(g["width"]), [g["unicode"]] if g["unicode"] else [], anchor_list, g.get("lib"))
        gs.writeGlyph(name, obj, draw, formatVersion=2)


def main():
    names = sys.argv[1:] or ORDER
    for mname in UPRIGHT + ITALIC:
        m = Master(mname)
        gs = glifLib.GlyphSet(str(m.path / "glyphs"))
        write(m, names, gs)
        gs.writeContents()
    print(f"wrote {len(names)} glyphs x 8 masters")


if __name__ == "__main__":
    main()
