# /// script
# requires-python = ">=3.11"
# dependencies = ["ufoLib2", "fonttools[ufo]>=4.60", "skia-pathops"]
# ///
"""Construct the Cyrillic glyphs of Tilo Sans from Instrument Sans' own
drawings, in all eight masters.

    uv run vendor/fonts/tilo-sans/scripts/cyrillic/construct.py            # every Cyrillic glyph + kerning
    uv run vendor/fonts/tilo-sans/scripts/cyrillic/construct.py Zhe-cy ... # just these glyphs, kerning untouched

Every letter is built from the Latin outlines of the same master (italic
masters are slanted back upright first, constructed, then slanted again), with
stroke-preserving moves and zone maps instead of plain scaling, so stems,
bars, terminals and curve tension stay Instrument's in every master and the
masters stay compatible. See README.md, "Cyrillic".

Running it rewrites the .glif files of the glyphs it builds (and, without
glyph arguments, the Cyrillic groups and kerning). A glyph refined by hand
afterwards should either be refined here or no longer be passed to it.
"""

from __future__ import annotations

import math
import re
import sys
from pathlib import Path

import ufoLib2
from fontTools.ufoLib.glifLib import GlyphSet

sys.path.insert(0, str(Path(__file__).parent))
import geom as G  # noqa: E402
import kern  # noqa: E402
import checks  # noqa: E402
from ctx import ITALIC, UPRIGHT, Ctx, Glyph, pmap, xcuts, ycuts, zmap  # noqa: E402
from parts import bowl_for_counter, foot_leg, p_bowl, p_params  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
SOURCES = ROOT / "sources"

DESC = -160  # Cyrillic descender tails (Ц Щ Д Ҷ ...)
UPTURN = 870  # top of the Ґ upturn
UPTURN_LC = 650

BUILDERS: dict[str, tuple] = {}


def glyph(name, uni=None):
    def deco(fn):
        BUILDERS[name] = (uni, fn)
        return fn
    return deco


# ---------------------------------------------------------------- helpers


def zm(contours, xpairs=None, ypairs=None):
    """A zone map whose curves are kept within Instrument's tension: a
    piecewise map can stretch a handle further than its curve."""
    return G.cap_tension(zmap(contours, xpairs, ypairs), 0.78)


def light_V(c, lc=True):
    """The stem of this master's light (400) master: overhangs and gaps
    measured in it keep their size as the weight grows, as Instrument's
    sidebearings and joins do, instead of widening the letter."""
    return c.light.lc["V"] if lc else c.light.cap["V"]


def anchors(x, top, bottom_x=None):
    return {"top": (x, top), "bottom": (bottom_x if bottom_x is not None else x, 0)}


def centre(contours):
    b = G.bounds(contours)
    return (b[0] + b[2]) / 2


def place(contours, lsb, rsb):
    """Shift so the left extreme sits at lsb; return (contours, width)."""
    b = G.bounds(contours)
    dx = lsb - b[0]
    out = G.shift(contours, dx)
    return out, b[2] + dx + rsb, dx


def a_perp(c):
    """Perpendicular thickness of A's left leg, relative to the cap stem."""
    A = c.g("A")[0]
    lo, hi = xcuts([A], 60), xcuts([A], 660)
    w = ((lo[1] - lo[0]) + (hi[1] - hi[0])) / 2
    ang = math.atan2((hi[0] + hi[1]) / 2 - (lo[0] + lo[1]) / 2, 600)
    return w * math.cos(ang) / c.cap["V"]


def leg_to_counter(c, xb, yb, stem_left, yt, counter, perp):
    """Leg whose top leaves `counter` units of open bar before the stem."""
    w = perp
    xt = stem_left - counter - w
    for _ in range(8):
        xt = max(stem_left - counter - w, xb + 0.05 * (yt - yb))
        ang = math.atan2(xt - xb, yt - yb)
        w = perp / math.cos(ang)
    return leg(xb, yb, xt, yt, perp) + (xt,)


def leg(xb, yb, xt, yt, perp):
    """Straight diagonal from the bottom-left corner (xb, yb) to the top-left
    corner (xt, yt), cut horizontally, `perp` thick across the stroke."""
    ang = math.atan2(xt - xb, yt - yb)
    w = perp / math.cos(ang)
    return G.poly((xb, yb), (xb + w, yb), (xt + w, yt), (xt, yt)), w


# ---------------------------------------------------------------- shared Latin forms

LOOKALIKES = {
    "A-cy": ("A", 0x0410), "Ve-cy": ("B", 0x0412), "Ie-cy": ("E", 0x0415), "Ka-cy": ("K", 0x041A),
    "Em-cy": ("M", 0x041C), "En-cy": ("H", 0x041D), "O-cy": ("O", 0x041E), "Er-cy": ("P", 0x0420),
    "Es-cy": ("C", 0x0421), "Te-cy": ("T", 0x0422), "Ha-cy": ("X", 0x0425),
    "Dze-cy": ("S", 0x0405), "I-cy": ("I", 0x0406), "Je-cy": ("J", 0x0408), "Ustraight-cy": ("Y", 0x04AE),
    "a-cy": ("a", 0x0430), "ie-cy": ("e", 0x0435), "o-cy": ("o", 0x043E), "er-cy": ("p", 0x0440),
    "es-cy": ("c", 0x0441), "u-cy": ("y", 0x0443), "ha-cy": ("x", 0x0445),
    "dze-cy": ("s", 0x0455), "i-cy": ("i", 0x0456), "je-cy": ("j", 0x0458), "shha-cy": ("h", 0x04BB),
}

# precomposed letters: (code point, base, marks); offsets from anchors
COMPOSITES = {
    "Iegrave-cy": (0x0400, "E", ["gravecomb"]),
    "Io-cy": (0x0401, "E", ["dieresiscomb"]),
    "Gje-cy": (0x0403, "Ge-cy", ["acutecomb"]),
    "Kje-cy": (0x040C, "Ka-cy", ["acutecomb"]),
    "Iigrave-cy": (0x040D, "Ii-cy", ["gravecomb"]),
    "Ushort-cy": (0x040E, "U-cy", ["brevecomb"]),
    "Iishort-cy": (0x0419, "Ii-cy", ["brevecomb"]),
    "iishort-cy": (0x0439, "ii-cy", ["brevecomb"]),
    "iegrave-cy": (0x0450, "e", ["gravecomb"]),
    "io-cy": (0x0451, "e", ["dieresiscomb"]),
    "gje-cy": (0x0453, "ge-cy", ["acutecomb"]),
    "kje-cy": (0x045C, "ka-cy", ["acutecomb"]),
    "iigrave-cy": (0x045D, "ii-cy", ["gravecomb"]),
    "ushort-cy": (0x045E, "y", ["brevecomb"]),
}
# Latin composites whose exact component offsets we reuse
LATIN_COPIES = {"Io-cy": "Edieresis", "io-cy": "edieresis", 
                "Iegrave-cy": "Egrave", "iegrave-cy": "egrave"}


# ================================================================ UPPERCASE


@glyph("Pe-cy", 0x041F)
def Pe(c):
    m = c.cap
    H = c.g("H")
    top = G.rect(m["Lx0"] + m["V"] / 2, 720 - m["arm"], m["Rx1"] - m["V"] / 2, 720)
    return Glyph(m["W"], [H[0], H[1], top], anchors(m["W"] / 2, 720))


@glyph("Ge-cy", 0x0413)
def Ge(c):
    F = c.g("F")
    out = [F[0], F[2]]
    b = G.bounds(out)
    return Glyph(c.w("F"), out, anchors((b[0] + b[2]) / 2, 720, (F[0][0][0] + F[0][1][0]) / 2))


@glyph("Geupturn-cy", 0x0490)
def Geupturn(c):
    return with_upturn(c, Ge(c), c.cap, 720, UPTURN)


UPTURN_WEIGHT = 0.85  # Ґ ґ: the upturn's width, in stems
UPTURN_FREE = 0.7  # the arm runs free of the stem for at least this many stems


def with_upturn(c, g, mt, height, top):
    """Ґ ґ: Г with an upturn at the end of its arm, a little lighter than
    the stem. The arm is lengthened where needed so that it runs free
    between stem and upturn for UPTURN_FREE stems in every master; without
    that the bold upturn starts inside the stem and the letter reads as a
    notched block."""
    V = mt["V"]
    stem, arm = g.contours[0], g.contours[1]
    stem_r = G.bounds([stem])[2]
    end = G.bounds([arm])[2]
    up_w = UPTURN_WEIGHT * V
    need = stem_r + UPTURN_FREE * V + up_w
    if end < need:
        arm = G.stretch([arm], "x", stem_r + 1, need - end)[0]
        g.width += need - end
        end = need
    up = G.rect(end - up_w, height - mt["arm"] / 2, end, top)
    g.contours = [stem, arm] + g.contours[2:] + [up]
    return g


@glyph("Gestroke-cy", 0x0492)
def Gestroke(c):
    g = Ge(c)
    m = c.cap
    VL = light_V(c, False)
    bar = G.rect(m["Lx0"] - 0.62 * VL, m["bar0"] - 20, m["Lx1"] + 1.75 * VL, m["bar1"] - 20)
    cons = g.contours + [bar]
    cons, width, dx = place(cons, m["lsb"] * 0.42, c.w("F") - G.bounds(g.contours)[2])
    return Glyph(width, cons, {k: (x + dx, y) for k, (x, y) in g.anchors.items()})


def soft_bowl(c, stem_in, counter=None, top=None):
    """Ь's bowl: P's bowl turned over (its bars, side, curve length and handle
    ratios), hung on a stem whose inner edge is stem_in. `counter` is the
    width of the counter; P's own by default."""
    p = p_params(c)
    top = p["Y1"] - p["Y0"] if top is None else top
    if counter is None:
        counter = p["xr"] - p["side"] - p["stem1"]
    return bowl_for_counter(c, stem_in, counter, 0, top, p["t_bot"], p["t_top"], p["side"])


def p_counter(c):
    p = p_params(c)
    return p["xr"] - p["side"] - p["stem1"]


def h_counter(c):
    m = c.cap
    return m["Rx0"] - m["Lx1"]


def p_stem(c, x0=None, top=720):
    p = p_params(c)
    x0 = p["stem0"] if x0 is None else x0
    return G.rect(x0, 0, x0 + p["stem1"] - p["stem0"], top)


def p_rsb(c):
    return c.w("P") - p_params(c)["xr"]


@glyph("Be-cy", 0x0411)
def Be(c):
    E = c.g("E")
    p = p_params(c)
    bowl = [p_stem(c), soft_bowl(c, p["stem1"])]
    arm = E[3]
    return Glyph(c.w("P"), bowl + [arm], anchors(centre(bowl + [arm]), 720, centre(bowl)))


@glyph("Softsign-cy", 0x042C)
def Softsign(c):
    p = p_params(c)
    bowl = [p_stem(c), soft_bowl(c, p["stem1"])]
    return Glyph(c.w("P"), bowl, anchors(centre(bowl), 720))


@glyph("Hardsign-cy", 0x042A)
def Hardsign(c):
    m = c.cap
    p = p_params(c)
    T = c.g("T")
    tb = G.bounds(T)
    half = (tb[2] - tb[0]) / 2 - m["V"] / 2  # T's bar beyond its stem
    armlen = half * 0.78
    bowl = [p_stem(c), soft_bowl(c, p["stem1"])]
    sx0 = p["stem0"]
    arm = G.rect(sx0 - armlen, 720 - m["arm"], sx0 + m["V"] / 2, 720)
    cons = bowl + [arm]
    cons, width, dx = place(cons, tb[0], p_rsb(c))
    return Glyph(width, cons, anchors(centre(cons), 720))


@glyph("Yeru-cy", 0x042B)
def Yeru(c):
    m = c.cap
    p = p_params(c)
    bowl = [p_stem(c), soft_bowl(c, p["stem1"], 0.86 * p_counter(c))]
    b = G.bounds(bowl)
    gap = m["lsb"] * 1.12
    stem = G.rect(b[2] + gap, 0, b[2] + gap + m["V"], 720)
    cons = bowl + [stem]
    return Glyph(b[2] + gap + m["V"] + m["lsb"], cons, anchors(centre(cons), 720))


def mirrored(c, name, roman=False):
    """`name` mirrored about its advance's centre. In the italic, outlines
    are mirrored upright (slanted back) and slanted again afterwards; with
    `roman`, the upright master's drawing is mirrored instead, for curves
    whose italic stress must not turn backwards."""
    src = c.roman if roman else c
    w = src.w(name)
    return G.mirror_x(src.g(name), w / 2), w


@glyph("Ii-cy", 0x0418)
def Ii(c):
    cons, w = mirrored(c, "N")
    return Glyph(w, cons, anchors(w / 2, 720))


@glyph("Ya-cy", 0x042F)
def Ya(c):
    cons, w = mirrored(c, "R", roman=True)
    return Glyph(w, cons, anchors(centre(cons), 720, centre(cons)))


def round_e(c, name, centre, bar_t, reverse, share):
    """Э Є э є from C or c: the terminals shortened along their own curves
    so both openings next to the bar stay at least 0.65 of the bar's
    thickness, the bar inside the counter from the inner side to under the
    terminals (`share` of the width from the open side, at least clear of
    the tips). Reversed in the italic, the upright C or c is mirrored and
    the whole slanted, so the stress follows the italic O's and not a
    backward-leaning mirror of the italic C."""
    src = c.roman if (reverse and c.italic) else c
    con = src.g(name)
    assert len(con) == 1, f"{name} topology changed"
    con = con[0]
    w = src.w(name)
    lo, hi = G.terminal_cuts(con)
    g = 0.65 * bar_t
    new_hi = max(hi, centre + bar_t / 2 + g)
    new_lo = min(lo, centre - bar_t / 2 - g)
    con = G.trim_terminals(con, {hi: new_hi, lo: new_lo})
    cons = [con]
    if reverse:
        cons = G.mirror_x(cons, w / 2)
    # the terminal tips: the two ends of the upper cut
    tips = sorted(p[0] for p in cons[0] if G.is_on(p) and abs(p[1] - new_hi) < 0.5)
    inner = xcuts(cons, centre)
    b = G.bounds(cons)
    reach = share * (b[2] - b[0])  # how far the bar reaches into the counter
    if reverse:
        x0, x1 = max(b[0] + reach, tips[-1] + 0.2 * bar_t), inner[-2] + 10
    else:
        x0, x1 = inner[1] - 10, min(b[2] - reach, tips[0] - 0.2 * bar_t)
    bar = G.rect(x0, centre - bar_t / 2, x1, centre + bar_t / 2)
    return cons, bar, w


def lc_bar(c):
    """Thickness of e's crossbar."""
    e = c.g("e")
    ys = ycuts(e, c.w("e") * 0.5)
    return ys[3] - ys[2]


@glyph("Ereversed-cy", 0x042D)
def Ereversed(c):
    m = c.cap
    cons, bar, w = round_e(c, "C", (m["bar0"] + m["bar1"]) / 2 - 15, m["bar1"] - m["bar0"], True, 0.33)
    narrow = 0.04 * w
    cons = G.shift(scale_about(c, cons + [bar], 0.96, 1, w / 2, 360), -narrow / 2)
    return Glyph(w - narrow, cons, anchors(centre(cons), 720))


@glyph("E-cy", 0x0404)
def Eukr(c):
    m = c.cap
    cons, bar, w = round_e(c, "C", (m["bar0"] + m["bar1"]) / 2 - 15, m["bar1"] - m["bar0"], False, 0.33)
    narrow = 0.04 * w
    cons = G.shift(scale_about(c, cons + [bar], 0.96, 1, w / 2, 360), -narrow / 2)
    return Glyph(w - narrow, cons, anchors(centre(cons), 720))


@glyph("Ze-cy", 0x0417)
def Ze(c):
    t = c.g("three")
    w = c.w("three")
    wider = 0.05 * w
    cons = G.stretch(t, "x", w * 0.55, wider)
    return Glyph(w + wider, cons, anchors(centre(cons), 720))


def el_parts(c, height, V, counter, arm, reach, x_term):
    """Л: a vertical left leg whose foot turns left as J's tail does
    (terminal at x_term), a top bar and a right stem, `counter` apart."""
    x_left = x_term + reach
    lg = foot_leg(c, x_left, V, height, reach)
    stem_l = x_left + V + counter
    bar = G.rect(x_left + V / 2, height - arm, stem_l + V / 2, height)
    stem = G.rect(stem_l, 0, stem_l + V, height)
    return [lg, bar, stem], stem_l + V


def el_reach(V, V_light):
    """How far Л's foot reaches left of its leg: about a stem in the light
    masters, growing at a sixth of the stem's rate, so the bold letters
    keep their width (the foot of a heavier J is no longer either)."""
    return 0.9 * V_light + 0.15 * (V - V_light)


@glyph("El-cy", 0x041B)
def El(c):
    m = c.cap
    V = m["V"]
    reach = el_reach(V, c.light.cap["V"])
    cons, xr = el_parts(c, 720, V, el_counter(c) * h_counter(c), m["arm"], reach, 0.32 * m["lsb"])
    return Glyph(xr + m["lsb"], cons, anchors(centre(cons), 720))


def el_counter(c, share=0.86):
    """Л л Љ љ: the counter as a share of H's or n's, a little narrower in
    the bold masters, where the foot already takes room on the left."""
    return share - 0.06 * c.wt
DE_COUNTER, DE_OVERHANG = 0.82, 0.25  # Д: counter and base overhang as shares of H's counter
DE_COUNTER_LC, DE_OVERHANG_LC = 0.85, 0.28  # д: the same, of n's counter


def de_parts(c, height, V, arm, sb, counter, ov):
    """Д: two vertical stems `counter` apart under a top bar, standing on a
    base that runs `ov` past each of them and descends at both ends. The
    left stem meets the base square, as Ц's stems do; counter and overhang
    are shares of H's or n's counter, so the letter keeps H's or n's width
    in every master."""
    tail_w = 0.9 * V
    xl = sb
    x_left = xl + ov
    lg = G.rect(x_left, 0, x_left + V, height)
    stem_l = x_left + V + counter
    stem_r = stem_l + V
    xr = stem_r + ov
    top = G.rect(x_left + V / 2, height - arm, stem_l + V / 2, height)
    stem = G.rect(stem_l, 0, stem_r, height)
    base = G.rect(xl, 0, xr, arm)
    tl = G.rect(xl, DESC, xl + tail_w, arm)
    tr = G.rect(xr - tail_w, DESC, xr, arm)
    return [lg, top, stem, base, tl, tr], xr


@glyph("De-cy", 0x0414)
def De(c):
    m = c.cap
    hc = h_counter(c)
    sb = m["lsb"] * 0.36
    cons, xr = de_parts(c, 720, m["V"], m["arm"], sb, DE_COUNTER * hc, DE_OVERHANG * hc)
    return Glyph(xr + sb, cons, anchors(centre(cons), 720))


def tail_right(xr_stem, V, arm, ext, tail_w, x_from):
    """Base bar from x_from to past the right stem plus a descending tail.
    Callers give `ext` in the light master's stems (see light_V), so the
    tail does not widen the letter faster than its stems do."""
    xr = xr_stem + ext
    return [G.rect(x_from, 0, xr, arm), G.rect(xr - tail_w, DESC, xr, arm)], xr


@glyph("Tse-cy", 0x0426)
def Tse(c):
    m = c.cap
    V = m["V"]
    H = c.g("H")
    extra, xr = tail_right(m["Rx1"], V, m["arm"], light_V(c, False) * 0.5, V * 0.92, m["Lx0"] + V / 2)
    cons = [H[0], H[1]] + extra
    return Glyph(xr + m["lsb"] * 0.4, cons, anchors(m["W"] / 2, 720))


def sha_parts(c, m, height, counter_ratio):
    V = m["V"]
    counter = counter_ratio * (m["Rx0"] - m["Lx1"])
    x0 = m["Lx0"]
    stems = [G.rect(x0 + i * (V + counter), 0, x0 + i * (V + counter) + V, height) for i in range(3)]
    xr = x0 + 3 * V + 2 * counter
    base = G.rect(x0 + V / 2, 0, xr - V / 2, m["arm"])
    return stems + [base], xr


@glyph("Sha-cy", 0x0428)
def Sha(c):
    m = c.cap
    cons, xr = sha_parts(c, m, 720, 0.72)
    return Glyph(xr + m["lsb"], cons, anchors((xr + m["lsb"]) / 2, 720))


@glyph("Shcha-cy", 0x0429)
def Shcha(c):
    m = c.cap
    V = m["V"]
    cons, xr = sha_parts(c, m, 720, 0.72)
    extra, xr2 = tail_right(xr, V, m["arm"], light_V(c, False) * 0.5, V * 0.92, xr - V / 2)
    return Glyph(xr2 + m["lsb"] * 0.4, cons + extra, anchors((xr + m["lsb"]) / 2, 720))


@glyph("Dzhe-cy", 0x040F)
def Dzhe(c):
    m = c.cap
    V = m["V"]
    H = c.g("H")
    base = G.rect(m["Lx0"] + V / 2, 0, m["Rx1"] - V / 2, m["arm"])
    cx = m["W"] / 2
    tail = G.rect(cx - V * 0.46, DESC, cx + V * 0.46, m["arm"])
    return Glyph(m["W"], [H[0], H[1], base, tail], anchors(cx, 720))


@glyph("Che-cy", 0x0427)
def Che(c):
    m = c.cap
    V = m["V"]
    u = c.g("u")
    l = c.lc
    W = 0.92 * m["W"]
    stem_r = W - m["lsb"]
    ub = G.bounds([u[0]])
    ycuts([u[0]], (l["Lx0"] + l["Lx1"]) / 2)  # bowl bottom outer/inner under the left stroke? use measured
    bowl_b = 0.36 * 720
    t = m["arm"] * 0.98
    u_bot_out = ub[1]
    u_bot_in = min(y for y in ycuts([u[0]], (ub[0] + l["Rx0"]) / 2) if y > u_bot_out + 5)
    us = G.bounds([u[1]])
    xpairs = [(ub[0], m["Lx0"]), (ub[0] + l["V"], m["Lx1"]), (us[0], stem_r - V), (us[2], stem_r)]
    k = 1.25
    ypairs = [(u_bot_out, bowl_b), (u_bot_in, bowl_b + t), (l_top_curve(c), bowl_b + (l_top_curve(c) - u_bot_out) * k), (510, 720)]
    bowl = zm([u[0]], xpairs, ypairs)
    stem = G.rect(stem_r - V, 0, stem_r, 720)
    cons = bowl + [stem]
    return Glyph(W, cons, anchors(W / 2, 720, stem_r - V / 2))


def l_top_curve(c):
    """Height where u's left stroke stops being straight."""
    u = c.g("u")[0]
    ys = [p[1] for p in u if G.is_on(p) and p[3] and p[1] > 60]
    return max(ys) if ys else 176


def scale_about(c, cons, fx, fy, xc, yc):
    """Scale about (xc, yc). In the italic masters the scaling happens in the
    slanted design, so vertical tangents stay vertical there; an affine map
    either way, so every handle ratio survives."""
    if c.italic:
        cons = G.reslant(cons)
        xc = xc + G.SLANT * (yc - G.SLANT_Y0)
    out = G.scale(cons, fx, fy, xc, yc)
    return G.unslant(out) if c.italic else out


def fit_ring(c, ring, x0, x1, y0=None, y1=None, side=None, top=None):
    """Fit an o-shaped ring (outer, inner contour) to the outer box x0..x1
    (y0..y1), keeping the side thickness `side` and top thickness `top`
    (the source's own by default). Outer and inner are each scaled as a
    whole, so their curves keep the source's tension."""
    outer, inner = ring
    ob, ib = G.bounds([outer]), G.bounds([inner])
    side = (ib[0] - ob[0] + ob[2] - ib[2]) / 2 if side is None else side
    top = (ob[3] - ib[3] + ib[1] - ob[1]) / 2 if top is None else top
    y0 = ob[1] if y0 is None else y0
    y1 = ob[3] if y1 is None else y1
    out = []
    for con, b, (tx0, ty0, tx1, ty1) in ((outer, ob, (x0, y0, x1, y1)),
                                         (inner, ib, (x0 + side, y0 + top, x1 - side, y1 - top))):
        cx, cy = (b[0] + b[2]) / 2, (b[1] + b[3]) / 2
        con = scale_about(c, [con], (tx1 - tx0) / (b[2] - b[0]), (ty1 - ty0) / (b[3] - b[1]), cx, cy)
        out += G.shift(con, (tx0 + tx1) / 2 - cx, (ty0 + ty1) / 2 - cy)
    return out


YU_RING = 0.9  # Ю ю: the O's width in every master, as a share of O's


@glyph("Yu-cy", 0x042E)
def Yu(c):
    """Ю: H's stem and bar, and O narrowed about its centre: one width
    ratio to O in every master."""
    m = c.cap
    V = m["V"]
    H = c.g("H")
    O = c.g("O")
    ob = G.bounds([O[0]])
    gap = 0.55 * light_V(c, False) + 0.1 * h_counter(c)
    ox0 = m["Lx1"] + gap
    ox1 = ox0 + YU_RING * (ob[2] - ob[0])
    W = ox1 + m["Olsb"]
    O = fit_ring(c, O, ox0, ox1)
    ob, oin = G.bounds([O[0]]), G.bounds([O[1]])
    bar = G.rect(m["Lx1"] - V / 2, m["bar0"] - 15, ob[0] + (oin[0] - ob[0]) / 2, m["bar1"] - 15)
    cons = [H[0], bar] + O
    return Glyph(W, cons, anchors(centre(cons), 720))


@glyph("Ef-cy", 0x0424)
def Ef(c):
    """Ф: a flattened O crossed by the stem; in the bold masters the oval is
    drawn from a lighter O so the counters stay open."""
    m = c.cap
    V = m["V"]
    O = c.blend("O", c.wt * 0.72) if c.wt > 0 else c.g("O")
    ob, ib = G.bounds([O[0]]), G.bounds([O[1]])
    side = (ib[0] - ob[0] + ob[2] - ib[2]) / 2
    # each half-counter at least 0.42 of O's counter
    Oc = c.g("O")
    oc = G.bounds([Oc[1]])[2] - G.bounds([Oc[1]])[0]
    ow = max((ob[2] - ob[0]) * (1.06 - 0.06 * c.wt), 2 * 0.38 * oc + 2 * side + V)
    x0 = m["Olsb"] * 0.9
    oval = fit_ring(c, O, x0, x0 + ow, 70, 650, top=0.9 * (ob[3] - ib[3]))
    cx = x0 + ow / 2
    stem = G.rect(cx - V / 2, 0, cx + V / 2, 720)
    return Glyph(x0 * 2 + ow, oval + [stem], anchors(cx, 720))


@glyph("Zhe-cy", 0x0416)
def Zhe(c):
    m = c.cap
    V = m["V"]
    K = c.g("K")
    kstem = G.bounds([K[0]])
    arms = [K[1], K[2]]
    ab = G.bounds(arms)
    extent = ab[2] - kstem[2]
    squeeze = 0.17 * extent
    arms = G.stretch(arms, "x", kstem[2] + 0.45 * extent, -squeeze)
    sb = c.w("K") - ab[2]
    half = extent - squeeze
    cx = sb + half + V / 2
    right = G.shift(arms, cx + V / 2 - kstem[2])
    left = G.mirror_x(right, cx)
    stem = G.rect(cx - V / 2, 0, cx + V / 2, 720)
    cons = left + [stem] + right
    return Glyph(2 * cx, cons, anchors(cx, 720))


@glyph("U-cy", 0x0423)
def U(c):
    """Cyrillic У: Y's arms, the right one running on to the baseline."""
    Y = c.g("Y")
    W = c.w("Y") * 0.97
    perp = diag_perp(Y, 650, 520)
    sb = G.bounds(Y)[0]
    xr_top = W - sb
    xb = 0.2 * W
    stroke, w, cl = diag_by_outer(xb, 0, xr_top, 720, perp)
    arm = arm_into(sb, 720, perp * 1.04, cl, 0.37 * 720, stroke_right_edge(stroke))
    cons = [stroke, arm]
    return Glyph(W, cons, anchors(centre(cons), 720))


def diag_perp(cons, y_hi, y_lo):
    """Perpendicular thickness of the rightmost diagonal stroke."""
    a, b = xcuts(cons, y_hi), xcuts(cons, y_lo)
    w = a[-1] - a[-2]
    ca, cb = (a[-1] + a[-2]) / 2, (b[-1] + b[-2]) / 2
    ang = math.atan2(ca - cb, y_hi - y_lo)
    return w * math.cos(ang)


def diag_by_outer(x_bot_left, y0, x_top_right, y1, perp):
    """Diagonal rising to the right from the bottom-left corner to the
    top-right corner; returns (contour, horizontal width, centreline)."""
    w = perp
    for _ in range(6):
        ang = math.atan2((x_top_right - w / 2) - (x_bot_left + w / 2), y1 - y0)
        w = perp / math.cos(ang)
    con = G.poly((x_bot_left, y0), (x_bot_left + w, y0), (x_top_right, y1), (x_top_right - w, y1))
    cl = ((x_bot_left + w / 2, y0), (x_top_right - w / 2, y1))
    return con, w, cl


def stroke_right_edge(con):
    # points 1 and 2 of diag_by_outer's polygon (after orientation fix)
    pts = sorted([(p[0], p[1]) for p in con], key=lambda p: (p[1], p[0]))
    bottom = max(pts[:2]); top = max(pts[2:])
    return bottom, top


def line_x_at(p0, p1, y):
    (x0, y0), (x1, y1) = p0, p1
    return x0 + (y - y0) * (x1 - x0) / (y1 - y0)


def intersect(p0, p1, q0, q1):
    (x1, y1), (x2, y2) = p0, p1
    (x3, y3), (x4, y4) = q0, q1
    d = (x1 - x2) * (y3 - y4) - (y1 - y2) * (x3 - x4)
    t = ((x1 - x3) * (y3 - y4) - (y1 - y3) * (x3 - x4)) / d
    return x1 + t * (x2 - x1), y1 + t * (y2 - y1)


def arm_into(x_top_left, y_top, perp, cl, y_join, edge, inset=3):
    """Arm from the top-left down into a rising stroke whose centreline is cl,
    meeting it at y_join; the arm ends on the stroke's right edge (hidden)."""
    jx = line_x_at(cl[0], cl[1], y_join)
    w = perp
    for _ in range(6):
        ang = math.atan2(jx - (x_top_left + w / 2), y_top - y_join)
        w = perp / math.cos(ang)
    tl = (x_top_left, y_top)
    tr = (x_top_left + w, y_top)
    dirx, diry = jx - (x_top_left + w / 2), y_join - y_top
    bl0 = (tl[0] + dirx, tl[1] + diry)
    br0 = (tr[0] + dirx, tr[1] + diry)
    e0, e1 = edge
    e0 = (e0[0] - inset, e0[1]); e1 = (e1[0] - inset, e1[1])
    bl = intersect(tl, bl0, e0, e1)
    br = intersect(tr, br0, e0, e1)
    return G.poly(tl, bl, br, tr)


@glyph("Tshe-cy", 0x040B)
def Tshe(c):
    return tshe_dje(c, dje=False)


@glyph("Dje-cy", 0x0402)
def Dje(c):
    return tshe_dje(c, dje=True)


def tshe_dje(c, dje):
    m = c.cap
    V = m["V"]
    T = c.g("T")
    tb = G.bounds(T)
    h = c.g("h")
    l = c.lc
    left_over = 0.62 * ((tb[2] - tb[0]) / 2 - V / 2)
    bar_l = tb[0]
    sx0 = bar_l + left_over
    stem = G.rect(sx0, 0, sx0 + V, 720)
    bar_r = sx0 + V + left_over * 0.95
    bar = G.rect(bar_l, 720 - m["arm"], bar_r, 720)
    arch = [h[1]]
    arch = G.shift(arch, sx0 + V - l["Lx1"])
    arch_top = 0.64 * 720
    arch = G.move_points(arch, lambda x, y: y > 200, dy=arch_top - 520)
    ab = G.bounds(arch)
    widen = 0.06 * (ab[2] - ab[0])
    arch = G.stretch(arch, "x", (ab[0] + ab[2]) / 2, widen)
    ab = G.bounds(arch)
    cons = [bar, stem] + arch
    if dje:
        counter = xcuts(arch, 100)[0] - (sx0 + V)
        # the hook ends nearly half the counter clear of the stem, so the
        # counter stays open at small sizes
        cons = [bar, stem, arch_with_tail(arch[0], c.g("J")[0], reach=0.55 * counter, depth=0.85)]
    return Glyph(ab[2] + m["lsb"], cons, anchors(centre([bar, stem]), 720, centre(cons)))


def arch_with_tail(arch, tail, reach=None, depth=1.0):
    """h's arch whose right leg runs on into J's (or j's) tail, as one
    contour: the leg's inner and outer edges become the tail's stem edges,
    the tail reaches `reach` left of the leg and its curve is scaled in
    height by `depth`."""
    assert len(arch) == 16 and len(tail) == 16, "h or J topology changed"
    xi, xo = arch[0][0], arch[1][0]
    t = [(p[0], p[1]) for p in tail]
    s_in, s_out, term = t[5][0], t[4][0], t[12][0]
    reach = s_in - term if reach is None else reach
    xf = pmap([(term, xi - reach), (s_in, xi), (s_out, xo)])
    y_bot, y_in, y_curve = t[0][1], t[9][1], max(t[3][1], t[6][1])
    yf = pmap([(y_bot, y_bot), (y_in, y_in), (y_curve, y_in + (y_curve - y_in) * depth)])
    T = [[xf(p[0]), yf(p[1]), p[2], p[3]] for p in tail]
    return [p[:] for p in arch[2:]] + T[6:] + T[:4]


@glyph("Lje-cy", 0x0409)
def Lje(c):
    m = c.cap
    V = m["V"]
    reach = el_reach(V, c.light.cap["V"])
    cons, xr = el_parts(c, 720, V, el_counter(c, 0.8) * h_counter(c), m["arm"], reach, 0.32 * m["lsb"])
    bowl = soft_bowl(c, xr, 0.86 * p_counter(c))
    cons = cons + [bowl]
    return Glyph(G.bounds([bowl])[2] + p_rsb(c), cons, anchors(centre(cons), 720))


@glyph("Nje-cy", 0x040A)
def Nje(c):
    m = c.cap
    V = m["V"]
    p = p_params(c)
    H = c.g("H")
    stem_l = m["Lx1"] + 0.9 * h_counter(c)
    stem = G.rect(stem_l, 0, stem_l + V, 720)
    bowl = soft_bowl(c, stem_l + V, 0.86 * p_counter(c))
    top = p["Y1"] - p["Y0"]
    bar = G.rect(m["Lx1"] - V / 2, top - p["t_bot"], stem_l + V / 2, top)
    cons = [H[0], bar, stem, bowl]
    return Glyph(G.bounds([bowl])[2] + p_rsb(c), cons, anchors(centre(cons), 720))


@glyph("Schwa-cy", 0x04D8)
def Schwa(c):
    m = c.cap
    e = c.g("e")
    w = c.w("e")
    r = G.rotate180(e, w / 2, 255)
    rb = G.bounds(r)
    # stroke edges of the rotated e
    vx = ycuts(r, w / 2 + 30)  # bottom out/in, bar lo/hi, top in/out
    hx = xcuts(r, 150)
    W = 0.92 * c.w("O")
    x0 = m["Olsb"]
    x1 = W - m["Olsb"]
    side = m["Oside"]
    xp = [(rb[0], x0), (hx[1], x0 + side), (hx[-2], x1 - side), (rb[2], x1)]
    barc = -10 + (((vx[2] + vx[3]) / 2) + 10) / 530 * 740
    yp = [(vx[0], -10), (vx[1], -10 + m["Otop"]), (vx[2], barc - m["arm"] / 2), (vx[3], barc + m["arm"] / 2),
          (vx[4], 730 - m["Otop"]), (vx[5], 730)]
    cons = zm(r, xp, yp)
    return Glyph(W, cons, anchors(W / 2, 720))


@glyph("Obarred-cy", 0x04E8)
def Obarred(c):
    m = c.cap
    O = c.g("O")
    ib = G.bounds([O[1]])
    bar = G.rect(ib[0] - 10, 360 - m["arm"] / 2, ib[2] + 10, 360 + m["arm"] / 2)
    return Glyph(c.w("O"), O + [bar], anchors(c.w("O") / 2, 720))


@glyph("Ustraightstroke-cy", 0x04B0)
def Ustraightstroke(c):
    m = c.cap
    Y = c.g("Y")
    G.bounds(Y)
    sx = xcuts(Y, 100)
    cx = (sx[0] + sx[1]) / 2
    bw = 0.56 * c.w("Y")
    bar = G.rect(cx - bw / 2, 235 - m["arm"] / 2, cx + bw / 2, 235 + m["arm"] / 2)
    return Glyph(c.w("Y"), Y + [bar], anchors(cx, 720))


@glyph("Shha-cy", 0x04BA)
def Shha(c):
    m = c.cap
    H = c.g("H")
    h = c.g("h")
    l = c.lc
    arch = G.shift([h[1]], m["Lx1"] - l["Lx1"])
    arch = G.move_points(arch, lambda x, y: y > 200, dy=0.64 * 720 - 520)
    ab = G.bounds(arch)
    target_r = 0.93 * m["W"] - m["lsb"]
    arch = G.stretch(arch, "x", (ab[0] + ab[2]) / 2, target_r - ab[2])
    cons = [H[0]] + arch
    return Glyph(target_r + m["lsb"], cons, anchors(centre(cons), 720))


@glyph("Kadescender-cy", 0x049A)
def Kadescender(c):
    m = c.cap
    V = m["V"]
    K = c.g("K")
    foot = xcuts([K[2]], 1)
    extra, xr = tail_right(foot[1], V, m["arm"], light_V(c, False) * 0.42, V * 0.92, foot[0] + 0.25 * (foot[1] - foot[0]))
    cons = K + extra
    return Glyph(max(c.w("K"), xr + m["lsb"] * 0.3), cons, anchors(c.anchor("K", "top")[0], 720))


@glyph("Endescender-cy", 0x04A2)
def Endescender(c):
    m = c.cap
    V = m["V"]
    H = c.g("H")
    extra, xr = tail_right(m["Rx1"], V, m["arm"], light_V(c, False) * 0.5, V * 0.92, m["Rx0"] + V / 2)
    return Glyph(xr + m["lsb"] * 0.4, H + extra, anchors(m["W"] / 2, 720))


@glyph("numero", 0x2116)
def numero(c):
    N = c.g("N")
    o = c.g("ordmasculine")
    ob = G.bounds(o)
    dx = c.w("N") - 0.55 * c.cap["lsb"] - ob[0]
    o = G.shift(o, dx)
    return Glyph(G.bounds(o)[2] + ob[0], N + o)


# ================================================================ lowercase


@glyph("ge-cy", 0x0433)
def ge(c):
    l = c.lc
    V = l["V"]
    stem = G.rect(l["Lx0"], 0, l["Lx1"], 510)
    W = 0.96 * c.w("r")  # r's width: the same stem and arm, in every master
    end = W - l["lsb"] * 0.38
    bar = G.rect(l["Lx0"] + V / 2, 510 - l["arm"], end, 510)
    return Glyph(W, [stem, bar], anchors((l["Lx0"] + end) / 2, 510, l["Lx0"] + V / 2))


@glyph("geupturn-cy", 0x0491)
def geupturn(c):
    return with_upturn(c, ge(c), c.lc, 510, UPTURN_LC)


@glyph("gestroke-cy", 0x0493)
def gestroke(c):
    g = ge(c)
    l = c.lc
    VL = light_V(c)
    bar = G.rect(l["Lx0"] - 0.62 * VL, 255 - l["arm"] / 2, l["Lx1"] + 1.6 * VL, 255 + l["arm"] / 2)
    cons = g.contours + [bar]
    cons, width, dx = place(cons, l["lsb"] * 0.4, g.width - G.bounds(g.contours)[2])
    return Glyph(width, cons, {k: (x + dx, y) for k, (x, y) in g.anchors.items()})


@glyph("pe-cy", 0x043F)
def pe(c):
    l = c.lc
    V = l["V"]
    x1 = l["Rx1"]
    stems = [G.rect(l["Lx0"], 0, l["Lx1"], 510), G.rect(x1 - V, 0, x1, 510)]
    top = G.rect(l["Lx0"] + V / 2, 510 - l["arm"], x1 - V / 2, 510)
    W = x1 + l["lsb"]
    return Glyph(W, stems + [top], anchors(W / 2, 510))


@glyph("en-cy", 0x043D)
def en(c):
    l = c.lc
    V = l["V"]
    x1 = l["Rx1"]
    stems = [G.rect(l["Lx0"], 0, l["Lx1"], 510), G.rect(x1 - V, 0, x1, 510)]
    bar = G.rect(l["Lx0"] + V / 2, 262 - l["arm"] / 2, x1 - V / 2, 262 + l["arm"] / 2)
    W = x1 + l["lsb"]
    return Glyph(W, stems + [bar], anchors(W / 2, 510))


@glyph("tse-cy", 0x0446)
def tse(c):
    l = c.lc
    V = l["V"]
    x1 = l["Rx1"]
    stems = [G.rect(l["Lx0"], 0, l["Lx1"], 510), G.rect(x1 - V, 0, x1, 510)]
    extra, xr = tail_right(x1, V, l["arm"], light_V(c) * 0.5, V * 0.92, l["Lx0"] + V / 2)
    return Glyph(xr + l["lsb"] * 0.4, stems + extra, anchors((x1 + 2 * l["lsb"]) / 2, 510))


def lc_sha(c, share=0.8):
    """ш: three stems whose counters are a share of m's, which narrow with
    weight faster than n's do, so ш widens with weight as m does."""
    l = c.lc
    m = dict(V=l["V"], Lx0=l["Lx0"], Lx1=l["Lx1"], Rx0=l["Rx0"], arm=l["arm"])
    xs = xcuts(c.g("m"), 100)
    m_counter = ((xs[2] - xs[1]) + (xs[4] - xs[3])) / 2
    return sha_parts(c, m, 510, share * m_counter / n_counter(c))


@glyph("sha-cy", 0x0448)
def sha(c):
    l = c.lc
    cons, xr = lc_sha(c)
    return Glyph(xr + l["lsb"], cons, anchors((xr + l["lsb"]) / 2, 510))


@glyph("shcha-cy", 0x0449)
def shcha(c):
    l = c.lc
    V = l["V"]
    cons, xr = lc_sha(c)
    extra, xr2 = tail_right(xr, V, l["arm"], light_V(c) * 0.5, V * 0.92, xr - V / 2)
    return Glyph(xr2 + l["lsb"] * 0.4, cons + extra, anchors((xr + l["lsb"]) / 2, 510))


@glyph("dzhe-cy", 0x045F)
def dzhe(c):
    l = c.lc
    V = l["V"]
    x1 = l["Rx1"]
    stems = [G.rect(l["Lx0"], 0, l["Lx1"], 510), G.rect(x1 - V, 0, x1, 510)]
    base = G.rect(l["Lx0"] + V / 2, 0, x1 - V / 2, l["arm"])
    W = x1 + l["lsb"]
    cx = W / 2
    tail = G.rect(cx - V * 0.46, DESC, cx + V * 0.46, l["arm"])
    return Glyph(W, stems + [base, tail], anchors(cx, 510))


@glyph("te-cy", 0x0442)
def te(c):
    T = c.g("T")
    w = c.w("T")
    l = c.lc
    cons = G.move_points(T, lambda x, y: y > 300, dy=-210)
    # T's bar is a cap arm; lowercase arms are a touch lighter
    m = c.cap
    cons = G.move_points(cons, lambda x, y: 300 < y < 470, dy=(m["arm"] - l["arm"]))
    narrow = 0.22 * w
    cons = G.stretch(cons, "x", w / 2 + 60, -narrow / 2)
    cons = G.stretch(cons, "x", w / 2 - 60, narrow / 2, below=True)
    cons = G.shift(cons, -narrow / 2)
    W = w - narrow
    # stem weight: lowercase stem
    sx = xcuts(cons, 100)
    d = (l["V"] - (sx[1] - sx[0])) / 2
    cons = G.move_points(cons, lambda x, y: y < 450 and abs(x - sx[0]) < 1, dx=-d)
    cons = G.move_points(cons, lambda x, y: y < 450 and abs(x - sx[1]) < 1, dx=d)
    return Glyph(W, cons, anchors(W / 2, 510))


@glyph("ii-cy", 0x0438)
def ii(c):
    cons, w = mirrored(c, "N")
    l = c.lc
    cons = G.move_points(cons, lambda x, y: y > 300, dy=-210)
    target = l["Rx1"] + l["lsb"]  # н's width
    shrink = w - target
    cons = G.stretch(cons, "x", w / 2, -shrink)
    cons, W, dx = place(cons, l["lsb"], l["lsb"])
    return Glyph(W, cons, anchors(W / 2, 510))


EM_VERTEX = 36  # м: the vertex stands this far above the baseline in every master


@glyph("em-cy", 0x043C)
def em(c):
    """м: M's construction redrawn at x-height, M's proportions to H
    carried over to n. Stems and the notches where the diagonals leave them
    follow M; the vertex stands a little above the baseline in every master,
    as Russian м usually does; the diagonals carry M's weight relative to
    the stem (about 0.72 of it) in every master."""
    l = c.lc
    V = l["V"]
    M = c.g("M")[0]
    assert len(M) == 16 and all(G.is_on(p) for p in M), "M topology changed"
    X = [p[0] for p in M]
    Y = [p[1] for p in M]
    mL, mR = X[1], X[8]  # M's stem inner edges
    mc = (X[4] + X[5]) / 2
    fy = 510 / 720
    W = 0.95 * c.w("M") * c.w("n") / c.w("H")
    x0, x3 = l["lsb"], W - l["lsb"]
    L, R = x0 + V, x3 - V
    xc = (x0 + x3) / 2
    yv = EM_VERTEX

    def draw(fth):
        def stem_pt(i, side):
            base, inner = (L, mL) if side < 0 else (R, mR)
            return base + (X[i] - inner) * fth

        pts = [None] * 16
        pts[0], pts[1] = (x0, 0), (L, 0)
        pts[2] = (L, 510 - (720 - Y[2]) * fy)
        pts[3] = (stem_pt(3, -1), 510 - (720 - Y[3]) * fy)
        pts[4] = (xc + (X[4] - mc) * fth, yv)
        pts[5] = (xc + (X[5] - mc) * fth, yv)
        pts[6] = (stem_pt(6, 1), 510 - (720 - Y[6]) * fy)
        pts[7] = (R, 510 - (720 - Y[7]) * fy)
        pts[8], pts[9], pts[10] = (R, 0), (x3, 0), (x3, 510)
        pts[11] = (stem_pt(11, 1), 510)
        pts[12] = (xc + (X[12] - mc) * fth, yv + Y[12] * fth)
        pts[13] = (xc + (X[13] - mc) * fth, yv + Y[13] * fth)
        pts[14] = (stem_pt(14, -1), 510)
        pts[15] = (x0, 510)
        return pts

    con = [G.P(x, y) for x, y in draw(V / c.cap["V"])]
    return Glyph(W, [con], anchors(W / 2, 510))


@glyph("ka-cy", 0x043A)
def ka(c):
    k = c.g("k")
    # k's ascender ends at 740 in Regular and 720 elsewhere: cut it to the
    # x-height itself, not by a fixed distance
    stem = [[[p[0], 510.0 if p[1] > 600 else p[1], p[2], p[3]] for p in k[0]]]
    cons = stem + [k[1]]
    return Glyph(c.w("k"), cons, anchors(centre(cons), 510))


@glyph("zhe-cy", 0x0436)
def zhe(c, stem_top=510):
    l = c.lc
    V = l["V"]
    k = c.g("k")
    kstem = G.bounds([k[0]])
    arms = [k[1]]
    ab = G.bounds(arms)
    extent = ab[2] - kstem[2]
    squeeze = 0.12 * extent
    arms = G.stretch(arms, "x", kstem[2] + 0.45 * extent, -squeeze)
    sb = c.w("k") - ab[2]
    half = extent - squeeze
    cx = sb + half + V / 2
    right = G.shift(arms, cx + V / 2 - kstem[2])
    left = G.mirror_x(right, cx)
    stem = G.rect(cx - V / 2, 0, cx + V / 2, stem_top)
    cons = left + [stem] + right
    return Glyph(2 * cx, cons, anchors(cx, 510))


def n_counter(c):
    l = c.lc
    return l["Rx0"] - l["Lx1"]


@glyph("el-cy", 0x043B)
def el(c):
    l = c.lc
    V = l["V"]
    reach = el_reach(V, c.light.lc["V"])
    cons, xr = el_parts(c, 510, V, el_counter(c) * n_counter(c), l["arm"], reach, 0.3 * l["lsb"])
    return Glyph(xr + l["lsb"], cons, anchors(centre(cons), 510))


@glyph("de-cy", 0x0434)
def de(c):
    l = c.lc
    V = l["V"]
    nc = n_counter(c)
    sb = l["lsb"] * 0.34
    cons, xr = de_parts(c, 510, V, l["arm"], sb, DE_COUNTER_LC * nc, DE_OVERHANG_LC * nc)
    return Glyph(xr + sb, cons, anchors(centre(cons), 510))


def soft_lc_metrics(c):
    """Bars and bowl height of the lowercase Ь bowl. The bars thin a little
    as the weight rises and the bowl grows, so the counter stays open."""
    l, lo = c.lc, c.light.lc
    bar = l["H"] - 0.2 * (l["H"] - lo["H"])
    top = 0.6 * 510 + 1.1 * (bar - lo["H"])
    return bar, top


def soft_lc(c, stem_in, counter_ratio=0.92):
    """The lowercase Ь bowl, from P's bowl drawn at x-height proportions,
    hung on a stem whose inner edge is stem_in."""
    l = c.lc
    bar, top = soft_lc_metrics(c)
    return bowl_for_counter(c, stem_in, counter_ratio * n_counter(c), 0, top, bar, bar, l["oside"])


def lc_stem(c, x0=None, top=510):
    l = c.lc
    x0 = l["Lx0"] if x0 is None else x0
    return G.rect(x0, 0, x0 + l["V"], top)


@glyph("softsign-cy", 0x044C)
def softsign(c):
    l = c.lc
    cons = [lc_stem(c), soft_lc(c, l["Lx1"])]
    b = G.bounds(cons)
    return Glyph(b[2] + l["olsb"], cons, anchors(centre(cons), 510))


@glyph("hardsign-cy", 0x044A)
def hardsign(c):
    l = c.lc
    t = c.g("t")
    tb = G.bounds([t[1]])
    cons = [lc_stem(c), soft_lc(c, l["Lx1"])]
    armlen = 0.75 * (c.w("t") / 2)
    arm = G.rect(l["Lx0"] - armlen, 510 - l["arm"], l["Lx0"] + l["V"] / 2, 510)
    cons = cons + [arm]
    cons, W, dx = place(cons, tb[0], l["olsb"])
    return Glyph(W, cons, anchors(centre(cons), 510))


@glyph("yeru-cy", 0x044B)
def yeru(c):
    l = c.lc
    cons = [lc_stem(c), soft_lc(c, l["Lx1"], 0.8)]
    b = G.bounds(cons)
    gap = l["lsb"] * 1.05
    stem = G.rect(b[2] + gap, 0, b[2] + gap + l["V"], 510)
    cons = cons + [stem]
    return Glyph(b[2] + gap + l["V"] + l["lsb"], cons, anchors(centre(cons), 510))


@glyph("ve-cy", 0x0432)
def ve(c):
    _m, l = c.cap, c.lc
    B = c.g("B")
    stemb = G.bounds([B[1]])
    # B's bars: bottom, middle, top (outer/inner pairs) under the stem's right
    xs_probe = stemb[2] + 60
    ys = ycuts([B[0]], xs_probe)  # 0, arm, mid0, mid1, top-arm, 720
    lo = xcuts([B[0]], 180)
    xcuts([B[0]], 560)
    W = 0.84 * (l["Rx1"] + l["lsb"])
    out_lo = W - l["olsb"]
    side = l["oside"]
    lo[-1] - out_lo
    xp = [(stemb[0], l["Lx0"]), (stemb[2], l["Lx1"]), (lo[-2], out_lo - side), (lo[-1], out_lo)]
    mid = 0.5 * 510 + 8
    yp = [(ys[0], 0), (ys[1], l["H"]), (ys[2], mid - l["H"] / 2), (ys[3], mid + l["H"] / 2),
          (ys[4], 510 - l["H"]), (ys[5], 510)]
    cons = zm(B, xp, yp)
    b = G.bounds(cons)
    return Glyph(b[2] + l["olsb"] + 6, cons, anchors(centre(cons), 510))


@glyph("ze-cy", 0x0437)
def ze(c):
    l = c.lc
    t = c.g("three")
    tb = G.bounds(t)
    cx = (tb[0] + tb[2]) / 2
    ys = ycuts(t, cx + 20)  # bottom out/in, mid lo/hi, top in/out
    xcuts(t, 560)
    xs_lo = xcuts(t, 180)
    W = 0.86 * (l["Rx1"] + l["lsb"])
    lsb = tb[0] * 0.95
    x_out = W - lsb * 1.05
    side = l["oside"]
    xp = [(tb[0], lsb), (xs_lo[-2], x_out - side), (xs_lo[-1], x_out)]
    mid = 0.5 * 510
    yp = [(ys[0], -10), (ys[1], -10 + l["H"]), (ys[2], mid - l["H"] / 2 + 5), (ys[3], mid + l["H"] / 2 + 5),
          (ys[-2], 520 - l["H"]), (ys[-1], 520)]
    cons = zm(t, xp, yp)
    return Glyph(W, cons, anchors(centre(cons), 510))


def r_leg(c):
    """R's leg: run per unit of height of its inner edge, bottom and top widths."""
    leg = c.g("R")[1]
    assert len(leg) == 4, "R topology changed"
    (bx0, by), (bx1, _), (tx1, ty), (tx0, _) = [(p[0], p[1]) for p in leg]
    return (bx0 - tx0) / (ty - by), bx1 - bx0, tx1 - tx0


@glyph("ya-cy", 0x044F)
def ya(c):
    """я: R's bowl and leg mirrored, drawn at x-height proportions: the bowl
    from P's bowl parameters, the leg with R's stroke widths, leaving the
    bowl's lower bar away from the stem."""
    m, l = c.cap, c.lc
    V = l["V"]
    lo = c.light.lc
    bar = l["H"] - 0.2 * (l["H"] - lo["H"])
    bowl_h = 0.56 * 510 + 2.0 * (bar - lo["H"])
    Y0 = 510 - bowl_h
    counter = 0.9 * n_counter(c)
    p = p_params(c)
    S = 1000.0  # inner edge of the stem; placed below
    bowl = p_bowl(c, -p["ov"], counter + l["oside"], Y0, 510, bar, bar, l["oside"], mirrored=True)
    bowl = G.mirror_x([bowl], S / 2)[0]
    stem = G.rect(S, 0, S + V, 510)
    k, wb, wt = r_leg(c)
    f = V / m["V"]
    # R's stroke thickness across the leg (its widths are horizontal cuts)
    cos_r = math.cos(math.atan(k))
    pb, pt = wb * f * cos_r, wt * f * cos_r
    # the leg leaves the bowl's lower bar 0.3 of the bowl's width out from
    # the stem, so the white between leg and stem stays a open wedge rather
    # than a slit that fills in at text sizes, and its foot reaches just past
    # the bowl, as R's does
    bowl_l = G.bounds([bowl])[0]
    yt = Y0 + bar / 2
    xr_t = S - 0.3 * (S - bowl_l)
    xl_b = bowl_l - 0.08 * (S - bowl_l)
    wb, wt = pb, pt
    for _ in range(6):  # horizontal cuts of a leg at this angle
        cosa = math.cos(math.atan2((xr_t - wt / 2) - (xl_b + wb / 2), yt))
        wb, wt = pb / cosa, pt / cosa
    legc = G.poly((xl_b, 0), (xl_b + wb, 0), (xr_t, yt), (xr_t - wt, yt))
    cons = [bowl, stem, legc]
    cons, W, dx = place(cons, l["olsb"] * 0.35, l["lsb"])
    return Glyph(W, cons, anchors(centre(cons), 510))


@glyph("ereversed-cy", 0x044D)
def ereversed(c):
    cons, bar, w = round_e(c, "c", 262, lc_bar(c), True, 0.3)
    return Glyph(w, cons + [bar], anchors(centre(cons), 510))


@glyph("e-cy", 0x0454)
def eukr(c):
    cons, bar, w = round_e(c, "c", 262, lc_bar(c), False, 0.3)
    return Glyph(w, cons + [bar], anchors(centre(cons), 510))


@glyph("yu-cy", 0x044E)
def yu(c, stem_top=510):
    l = c.lc
    V = l["V"]
    o = c.g("o")
    ob = G.bounds([o[0]])
    gap = 0.5 * light_V(c) + 0.1 * n_counter(c)
    ox0 = l["Lx1"] + gap
    W = ox0 + YU_RING * (ob[2] - ob[0]) + l["olsb"]
    o = fit_ring(c, o, ox0, W - l["olsb"])
    ob, ib = G.bounds([o[0]]), G.bounds([o[1]])
    stem = G.rect(l["Lx0"], 0, l["Lx1"], stem_top)
    bar = G.rect(l["Lx1"] - V / 2, 262 - l["arm"] / 2, ob[0] + (ib[0] - ob[0]) / 2, 262 + l["arm"] / 2)
    cons = [stem, bar] + o
    return Glyph(W, cons, anchors(centre(cons), 510))


def ef_half(c, name, f, t):
    """One bowl of ф: p's (right) or q's (left) bowl, from a lighter drawing
    in the bold masters, narrowed towards the stem by f: the outer contour's
    bowl points move towards the stem edge, the counter is scaled as a
    whole so the outer side keeps the source's thickness. In the italic the
    narrowing happens in the slanted design (see scale_about). Returns the
    contours and the stem edge they hang on."""
    src = c.blend(name, t) if c.wt > 0 else c.g(name)
    st = xcuts([src[0]], -100)  # the descender stem
    right = name == "p"
    edge = st[1] if right else st[0]
    outer, counter = src
    ob, cb = G.bounds([outer]), G.bounds([counter])
    side = (ob[2] - cb[2]) if right else (cb[0] - ob[0])
    sh = G.SLANT * (1 - f) if c.italic else 0.0
    beyond = (lambda x: x > edge + 0.5) if right else (lambda x: x < edge - 3)
    outer = [[edge + (q[0] - edge) * f - sh * (q[1] - 255) if beyond(q[0]) else q[0], q[1], q[2], q[3]]
             for q in outer]
    ext = edge + ((ob[2] if right else ob[0]) - edge) * f
    if right:
        cw = (ext - side) - cb[0]
        cc = scale_about(c, [counter], cw / (cb[2] - cb[0]), 1, cb[0], (cb[1] + cb[3]) / 2)
    else:
        cw = cb[2] - (ext + side)
        cc = scale_about(c, [counter], cw / (cb[2] - cb[0]), 1, cb[2], (cb[1] + cb[3]) / 2)
    return [outer] + cc, edge


@glyph("ef-cy", 0x0444)
def ef(c):
    """ф: p's bowl on the right of one stem and, on the left, its mirror in
    the upright and q's own bowl in the italic (a mirrored italic bowl would
    lean backwards). The bowls are narrowed as a whole (outer and counter
    each scaled), and in the bold masters drawn from a lighter p and q,
    thinning where they meet the stem as b and d do."""
    l = c.lc
    V = l["V"]
    t = c.wt * 0.7
    f = 0.96 if c.condensed else 0.84
    right, edge = ef_half(c, "p", f, t)
    cx = 0.0
    right = G.shift(right, cx + V / 2 - edge)
    if c.italic:
        left, ledge = ef_half(c, "q", f, t)
        left = G.shift(left, cx - V / 2 - ledge)
    else:
        left = G.mirror_x(right, cx)
    stem = G.rect(cx - V / 2, -205, cx + V / 2, 720)
    cons = left + right + [stem]
    cons, W, dx = place(cons, l["olsb"], l["olsb"])
    return Glyph(W, cons, anchors(centre(cons), 720))


@glyph("che-cy", 0x0447)
def che(c):
    l = c.lc
    V = l["V"]
    u = c.g("u")
    bowl = [u[0]]
    up = 0.36 * 510 + 10
    bowl = G.shift(bowl, 0, up)
    bowl = G.move_points(bowl, lambda x, y: y > 510 + up - 20, dy=-up)
    G.bounds(u)
    stem = G.rect(l["Rx1"] - V - (l["Rx1"] - G.bounds([u[1]])[2]), 0, G.bounds([u[1]])[2], 510)
    sb = G.bounds([u[1]])
    stem = G.rect(sb[0], 0, sb[2], 510)
    cons = bowl + [stem]
    return Glyph(c.w("u"), cons, anchors(c.w("u") / 2, 510, (sb[0] + sb[2]) / 2))


@glyph("dje-cy", 0x0452)
def dje(c):
    """ђ: ћ whose right leg runs on into j's tail."""
    h = c.g("h")
    hb = c.g("hbar")
    bars = [con for con in hb if G.bounds([con])[1] > 510 and G.bounds([con])[3] - G.bounds([con])[1] < 150]
    assert len(bars) == 1, "hbar: no single ascender bar"
    cons = [h[0], bars[0], arch_with_tail(h[1], c.g("jdotless")[0])]
    return Glyph(c.w("h"), cons, anchors(c.w("h") / 2, 510))


@glyph("tshe-cy", 0x045B)
def tshe(c):
    return Glyph(c.w("hbar"), components=[("hbar", 0, 0)])


@glyph("lje-cy", 0x0459)
def lje(c):
    l = c.lc
    V = l["V"]
    reach = el_reach(V, c.light.lc["V"])
    cons, xr = el_parts(c, 510, V, el_counter(c, 0.8) * n_counter(c), l["arm"], reach, 0.3 * l["lsb"])
    bowl = soft_lc(c, xr, 0.84)
    cons = cons + [bowl]
    return Glyph(G.bounds([bowl])[2] + l["olsb"], cons, anchors(centre(cons), 510))


@glyph("nje-cy", 0x045A)
def nje(c):
    """њ: н whose right stem carries the Ь bowl; н's counter as in н, the
    crossbar level with the bowl's top bar and ending in the right stem."""
    l = c.lc
    V = l["V"]
    bar_t, top = soft_lc_metrics(c)
    stem_l = l["Lx1"] + 0.9 * n_counter(c)
    stem = lc_stem(c, stem_l)
    bowl = soft_lc(c, stem_l + V, 0.84)
    bar = G.rect(l["Lx1"] - V / 2, top - bar_t, stem_l + V / 2, top)
    cons = [lc_stem(c), bar, stem, bowl]
    return Glyph(G.bounds([bowl])[2] + l["olsb"], cons, anchors(centre(cons), 510))


@glyph("schwa-cy", 0x04D9)
def schwa(c):
    e = c.g("e")
    w = c.w("e")
    r = G.rotate180(e, w / 2, 255)
    return Glyph(w, r, anchors(w / 2, 510))


@glyph("obarred-cy", 0x04E9)
def obarred(c):
    l = c.lc
    o = c.g("o")
    ib = G.bounds([o[1]])
    bar = G.rect(ib[0] - 10, 255 - l["arm"] / 2, ib[2] + 10, 255 + l["arm"] / 2)
    return Glyph(c.w("o"), o + [bar], anchors(c.w("o") / 2, 510))


@glyph("ustraight-cy", 0x04AF)
def ustraight(c):
    l = c.lc
    v = c.g("v")
    G.bounds(v)
    bx = xcuts(v, 2)
    cx = (bx[0] + bx[-1]) / 2
    stem = G.rect(cx - l["V"] / 2, -205, cx + l["V"] / 2, 40)
    return Glyph(c.w("v"), v + [stem], anchors(cx, 510))


@glyph("ustraightstroke-cy", 0x04B1)
def ustraightstroke(c):
    l = c.lc
    g = ustraight(c)
    cx = g.anchors["top"][0]
    bw = 0.5 * g.width
    bar = G.rect(cx - bw / 2, -95 - l["arm"] / 2, cx + bw / 2, -95 + l["arm"] / 2)
    g.contours.append(bar)
    return g


@glyph("kadescender-cy", 0x049B)
def kadescender(c):
    l = c.lc
    V = l["V"]
    k = ka(c)
    foot = xcuts([k.contours[1]], 1)
    extra, xr = tail_right(foot[-1], V, l["arm"], light_V(c) * 0.42, V * 0.92, foot[-2] + 0.25 * (foot[-1] - foot[-2]))
    return Glyph(max(k.width, xr + l["lsb"] * 0.3), k.contours + extra, k.anchors)


@glyph("endescender-cy", 0x04A3)
def endescender(c):
    l = c.lc
    V = l["V"]
    g = en(c)
    x1 = l["Rx1"]
    extra, xr = tail_right(x1, V, l["arm"], light_V(c) * 0.5, V * 0.92, x1 - V / 2)
    return Glyph(xr + l["lsb"] * 0.4, g.contours + extra, g.anchors)


@glyph("be-cy", 0x0431)
def be(c):
    """б: o, with a straight stroke rising from its left side into f's
    top curve, which runs on as a level flag to the bowl's right edge and
    ends in a vertical cut, f's top thickness (about the H bar's)."""
    o = c.g("o")
    w = c.w("o")
    ob = G.bounds([o[0]])
    f = c.g("f")[0]
    assert len(f) == 16, "f topology changed"
    f = G.shift([f], ob[0] - f[0][0])[0]
    ymid = (ob[1] + ob[3]) / 2
    xe = ob[2] - 0.03 * w
    P = G.P
    stroke = [P(f[0][0], ymid), P(f[1][0], ymid)] + [q[:] for q in f[2:6]] + [
        P(xe, f[5][1]), P(xe, f[12][1]), P(f[12][0], f[12][1], "line", True)] + [q[:] for q in f[13:16]]
    cons = o + [stroke]
    return Glyph(w, cons, anchors(centre(o), 510))


def yi_form(c, base, source):
    """ї Ї: the base with its own dieresis, dots set closer than the Latin
    one and kept inside the advance (widening it if needed), so that in
    'її' the dots of neighbouring letters stay apart."""
    src = c.g(source)
    bw = c.w(base)
    stem = G.bounds(c.g(base))
    dots = sorted(src[-2:], key=lambda d: G.bounds([d])[0])
    d0 = G.bounds([dots[0]])
    dw, dh = d0[2] - d0[0], d0[3] - d0[1]
    k = 0.86 - 0.16 * c.wt  # dot size, relative to the Latin dieresis
    gap = 0.38 * k * dw
    span = 2 * k * dw + gap
    adv = max(bw, span + 0.5 * gap)  # at least a quarter of the gap clear on each side
    shift = (adv - bw) / 2
    cx = (stem[0] + stem[2]) / 2 + shift
    out = []
    for i, d in enumerate(dots):
        b = G.bounds([d])
        dcx, dcy = (b[0] + b[2]) / 2, (b[1] + b[3]) / 2
        d = scale_about(c, [d], k, k, dcx, dcy)
        target = cx + (-1 if i == 0 else 1) * (gap / 2 + k * dw / 2)
        # keep the dots' tops where the Latin dieresis has them
        out += G.shift(d, target - dcx, (d0[3] - (dcy + k * dh / 2)))
    return Glyph(adv, out, components=[(base, shift, 0)])


@glyph("yi-cy", 0x0457)
def yi(c):
    return yi_form(c, "idotless", "idieresis")


@glyph("Yi-cy", 0x0407)
def Yi(c):
    return yi_form(c, "I", "Idieresis")


def six_de(c):
    """The 6 drawn at x-height with an ascending stroke and a level flag;
    mirrored, the cursive д (∂). The upright 6 is used in the italic too:
    mirrored and slanted it leans forward, a mirrored italic 6 backwards."""
    s = c.roman.g("six")
    w = c.roman.w("six")
    b = G.bounds(s)
    # vertical zones of 6: bowl bottom (out/in), bowl top (in/out), top stroke (in/out)
    cx = (b[0] + b[2]) / 2 + 30
    ys = ycuts(s, cx)
    # ys: -10, 62, 384, 454, 658, 730 (regular)
    yp = [(ys[0], -10), (ys[1], -10 + (ys[1] - ys[0])), (ys[2], 520 - (ys[3] - ys[2])), (ys[3], 520),
          (ys[4], 745 - (ys[5] - ys[4])), (ys[5], 745)]
    cons = zm(s, None, yp)
    cons = flag_terminal(cons, (b[0] + b[2]) / 2, b[2] + 0.01 * w)
    return Glyph(w, cons, anchors(centre(cons), 510))


def flag_terminal(cons, xmid, xend):
    """Turn 6's falling terminal into б's level flag ending in a vertical cut."""
    out = []
    for con in cons:
        n = len(con)
        hit = None
        for i, p in enumerate(con):
            q = con[(i + 1) % n]
            if G.is_on(p) and G.is_on(q) and q[2] == "line" and abs(p[1] - q[1]) < 1 and p[0] > xmid and q[0] > p[0] and p[1] > 400:
                hit = i
        if hit is None:
            out.append(con)
            continue
        i, j = hit, (hit + 1) % n
        # previous on-curve before i and next on-curve after j
        pi = (i - 3) % n
        qi = (j + 3) % n
        assert G.is_on(con[pi]) and G.is_on(con[qi])
        newc = []
        drop = {(i - 2) % n, (i - 1) % n, (j + 1) % n, (j + 2) % n}
        for k, p in enumerate(con):
            if k in drop:
                continue
            p = p[:]
            if k == i:
                p = [xend, con[pi][1], "line", False]
            elif k == j:
                p = [xend, con[qi][1], "line", False]
            elif k == qi:
                p[2] = "line"
            newc.append(p)
        out.append(newc)
    return out



# ================================================================ language forms

BGR_COMPONENTS = {"de-cy.loclBGR": "g", "ii-cy.loclBGR": "u", "ka-cy.loclBGR": "k", "pe-cy.loclBGR": "n", "te-cy.loclBGR": "m"}
for _n, _b in BGR_COMPONENTS.items():
    BUILDERS[_n] = (None, (lambda b: lambda c: Glyph(c.w(b), components=[(b, 0, 0)]))(_b))


@glyph("zhe-cy.loclBGR")
def zhe_bgr(c):
    return zhe(c, stem_top=720)


@glyph("yu-cy.loclBGR")
def yu_bgr(c):
    return yu(c, stem_top=720)


@glyph("el-cy.loclBGR")
def el_bgr(c):
    v = c.g("v")
    w = c.w("v")
    cons = G.rotate180(v, w / 2, 255)
    return Glyph(w, cons, anchors(w / 2, 510))


def u_forms(c, name):
    """u-shaped ц ш щ (Bulgarian upright, Russian italic)."""
    l = c.lc
    V = l["V"]
    u = c.g("u")
    ub = G.bounds([u[1]])
    if name == "tse":
        cons, xr_stem, w = u, ub[2], c.w("u")
    else:
        m_ = c.g("m")
        ms = xcuts(m_, 100)
        step = ms[2] - ms[0]
        cons = [u[0]] + G.shift([u[0]], step) + G.shift([u[1]], step)
        xr_stem = ub[2] + step
        w = c.w("u") + step
    if name in ("tse", "shcha"):
        extra, xr = tail_right(xr_stem, V, l["arm"], light_V(c) * 0.5, V * 0.92, xr_stem - V / 2)
        cons = cons + extra
        w = max(w, xr + l["lsb"] * 0.4)
    return Glyph(w, cons, anchors(centre(cons[:3]), 510))


@glyph("tse-cy.loclBGR")
def tse_bgr(c):
    return u_forms(c, "tse")


@glyph("sha-cy.loclBGR")
def sha_bgr(c):
    return u_forms(c, "sha")


@glyph("shcha-cy.loclBGR")
def shcha_bgr(c):
    return u_forms(c, "shcha")


SRB_UPRIGHT = {"be-cy.loclSRB": "be-cy", "ge-cy.loclSRB": "ge-cy", "de-cy.loclSRB": "de-cy",
               "pe-cy.loclSRB": "pe-cy", "te-cy.loclSRB": "te-cy"}


def srb_italic(c, name):
    l = c.lc
    mac = G.bounds(c.g("macroncomb"))
    # the macron's own weight, its top level with i's dot in every master
    # (macroncomb itself sits at a different height in each)
    my1 = 720
    my0 = my1 - (mac[3] - mac[1])
    if name == "de-cy.loclSRB":
        return Glyph(c.w("g"), components=[("g", 0, 0)])
    if name in ("pe-cy.loclSRB", "te-cy.loclSRB", "ge-cy.loclSRB"):
        base = {"pe-cy.loclSRB": "n", "te-cy.loclSRB": "m", "ge-cy.loclSRB": "idotless"}[name]
        b = G.bounds(c.g(base))
        if base == "idotless":
            cx = (b[0] + b[2]) / 2
            half = 0.62 * (mac[2] - mac[0]) / 2
            x0, x1 = cx - half, cx + half
        else:
            x0, x1 = b[0], b[2]
        bar = G.rect(x0, my0, x1, my1)
        g = Glyph(c.w(base), [bar], components=[(base, 0, 0)])
        return g
    if name == "be-cy.loclSRB":
        o = c.g("o")
        ob = G.bounds(o)
        w = c.w("o")
        top0, top1 = 745 - l["H"], 745
        neck_bot_x = ob[0] + 0.5 * (ob[2] - ob[0])
        neck_top_x = ob[0] + 0.24 * (ob[2] - ob[0])
        perp = l["V"] * 0.96
        lg, lw = leg(neck_bot_x - perp / 2 - 30, 470, neck_top_x, top0 + 4, perp)
        # mirror the leg so it leans left going up: build directly
        x_b, x_t = neck_bot_x, neck_top_x
        ang = math.atan2(x_b - x_t, top1 - 470)
        ww = perp / math.cos(ang)
        neck = G.poly((x_b - ww / 2, 470), (x_b + ww / 2, 470), (x_t + ww / 2, top1), (x_t - ww / 2, top1))
        bar = G.rect(x_t, top0, ob[2] - 0.04 * w, top1)
        return Glyph(w, o + [neck, bar], anchors(w / 2, 510))
    raise KeyError(name)


for _n, _b in SRB_UPRIGHT.items():
    BUILDERS[_n] = (None, (lambda n, b: lambda c: srb_italic(c, n) if c.italic else Glyph(BUILDERS[b][1](c).width, components=[(b, 0, 0)]))(_n, _b))

@glyph("gje-cy.loclSRB")
def gje_srb(c):
    """Macedonian ѓ follows г: in the italic the ī form, with the acute above
    the macron."""
    if not c.italic:
        return Glyph(ge(c).width, components=[("gje-cy", 0, 0)])
    g = srb_italic(c, "ge-cy.loclSRB")
    bar = G.bounds(g.contours)
    ax, ay = c.anchor("acutecomb", "_top")
    _, top = c.anchor("macroncomb", "top")
    dx, dy = (bar[0] + bar[2]) / 2 - ax, top - ay
    return Glyph(g.width, [], components=[("ge-cy.loclSRB", 0, 0), ("acutecomb", dx + G.SLANT * dy, dy)])


COMPOSITES.update({
    "iishort-cy.loclBGR": (None, "u", ["brevecomb"]),
    "iigrave-cy.loclBGR": (None, "u", ["gravecomb"]),
})

# ---------------------------------------------------------------- italic forms


def italic_override(name, c):
    """Cursive italic forms; return None to use the upright construction."""
    if name == "ii-cy":
        return Glyph(c.w("u"), components=[("u", 0, 0)])
    if name == "pe-cy":
        return Glyph(c.w("n"), components=[("n", 0, 0)])
    if name == "te-cy":
        return Glyph(c.w("m"), components=[("m", 0, 0)])
    if name in ("tse-cy", "sha-cy", "shcha-cy"):
        l = c.lc
        V = l["V"]
        u = c.g("u")
        ub = G.bounds([u[1]])
        if name == "tse-cy":
            cons, xr_stem, w = u, ub[2], c.w("u")
        else:
            step = c.lc["Rx0"] - c.lc["Lx0"]
            m_ = c.g("m")
            # m's arch spacing
            ms = xcuts(m_, 100)
            step = ms[2] - ms[0]
            cons = [u[0]] + G.shift([u[0]], step) + G.shift([u[1]], step)
            xr_stem = ub[2] + step
            w = c.w("u") + step
        if name in ("tse-cy", "shcha-cy"):
            extra, xr = tail_right(xr_stem, V, l["arm"], light_V(c) * 0.5, V * 0.92, xr_stem - V / 2)
            cons = cons + extra
            w = max(w, xr + l["lsb"] * 0.4)
        return Glyph(w, cons, anchors(centre(cons), 510))
    if name == "de-cy":
        g = six_de(c)
        w = g.width
        cons = G.mirror_x(g.contours, w / 2)
        return Glyph(w, cons, anchors(centre(cons), 510))
    return None


# ================================================================ writer


def build_all(fonts, master):
    c = Ctx(master, fonts)
    out = {}
    for name, (uni, fn) in BUILDERS.items():
        g = None
        if c.italic:
            g = italic_override(name, c)
        if g is None:
            g = fn(c)
        g.unicode = uni
        out[name] = g
    for name, (base, uni) in LOOKALIKES.items():
        out[name] = Glyph(c.w(base), components=[(base, 0, 0)])
        out[name].unicode = uni
    return c, out


def final_anchor(c, built, name, anchor):
    if name in built and built[name].anchors and anchor in built[name].anchors:
        x, y = built[name].anchors[anchor]
        if c.italic:
            x = x + G.SLANT * (y - G.SLANT_Y0)
        return x, y
    if name in built and built[name].components:
        return final_anchor(c, built, built[name].components[0][0], anchor)
    for a in c.font[name].anchors:
        if a.name == anchor:
            return a.x, a.y
    raise KeyError((name, anchor))


def add_composites(c, built):
    for name, (uni, base, marks) in COMPOSITES.items():
        if name in LATIN_COPIES:
            src = c.font[LATIN_COPIES[name]]
            comps = [(k.baseGlyph, k.transformation[4], k.transformation[5]) for k in src.components]
            g = Glyph(src.width, components=comps)
        else:
            bw = built[base].width if base in built else c.w(base)
            comps = [(base, 0, 0)]
            bx, by = final_anchor(c, built, base, "top")
            for mk in marks:
                mx, my = final_anchor(c, built, mk, "_top")
                comps.append((mk, bx - mx, by - my))
                bx, by = final_anchor(c, built, mk, "top")
                bx, by = bx + comps[-1][1], by + comps[-1][2]
            g = Glyph(bw, components=comps)
        g.unicode = uni
        built[name] = g


def fix_smooth(contours):
    """Re-align handles around smooth points (piecewise maps can bend them)."""
    for con in contours:
        n = len(con)
        for i, p in enumerate(con):
            if not (G.is_on(p) and p[3]):
                continue
            a, b = con[i - 1], con[(i + 1) % n]
            if G.is_on(a) and G.is_on(b):
                continue
            if G.is_on(a) or G.is_on(b):
                # line + curve: align the handle with the line
                line_pt, h = (a, b) if G.is_on(a) else (b, a)
                dx, dy = p[0] - line_pt[0], p[1] - line_pt[1]
                L = math.hypot(dx, dy)
                hl = math.hypot(h[0] - p[0], h[1] - p[1])
                if L and hl:
                    h[0] = p[0] + dx / L * hl
                    h[1] = p[1] + dy / L * hl
                continue
            ax, ay = p[0] - a[0], p[1] - a[1]
            bx, by = b[0] - p[0], b[1] - p[1]
            la, lb = math.hypot(ax, ay), math.hypot(bx, by)
            if not la or not lb:
                continue
            # average direction
            dx, dy = ax / la + bx / lb, ay / la + by / lb
            d = math.hypot(dx, dy)
            if d < 1e-6:
                raise ValueError(f"cusp at a smooth point ({p[0]:.0f}, {p[1]:.0f})")
            dx, dy = dx / d, dy / d
            a[0], a[1] = p[0] - dx * la, p[1] - dy * la
            b[0], b[1] = p[0] + dx * lb, p[1] + dy * lb
    return contours


def write_master(master, c, built):
    path = SOURCES / f"TiloSans-{master}.ufo" / "glyphs"
    gs = GlyphSet(str(path))
    for name, g in built.items():
        cons = fix_smooth(g.contours)
        anc = dict(g.anchors)
        if c.italic:
            cons = G.reslant(cons)
            anc = {k: (x + G.SLANT * (y - G.SLANT_Y0), y) for k, (x, y) in anc.items()}
        cons = G.round_contours(cons)

        class Obj:
            pass

        obj = Obj()
        obj.width = round(g.width)
        obj.unicodes = [g.unicode] if getattr(g, "unicode", None) else []
        obj.anchors = [{"name": k, "x": round(x), "y": round(y)} for k, (x, y) in sorted(anc.items())]
        if g.lib:
            obj.lib = g.lib

        def draw(pen, cons=cons, comps=g.components):
            for con in cons:
                pen.beginPath()
                for x, y, t, s in con:
                    pen.addPoint((x, y), segmentType=t, smooth=s)
                pen.endPath()
            for base, dx, dy in comps:
                pen.addComponent(base, (1, 0, 0, 1, round(dx), round(dy)))

        gs.writeGlyph(name, obj, draw)
    gs.writeContents()


# Counters that must stay open in every master: (axis, at, count, ref, fraction)
# -- along the scanline at least `count` interior gaps of fraction x ref.
# Refs: n / H = the n / H counter, o / O = the o / O counter's width, xh = 510.
PROBES = {
    "softsign-cy": [("y", 150, 1, "n", 0.8)], "hardsign-cy": [("y", 150, 1, "n", 0.8)],
    "yeru-cy": [("y", 150, 2, "n", 0.25), ("y", 150, 1, "n", 0.7)],
    "nje-cy": [("y", 440, 1, "n", 0.85), ("y", 150, 2, "n", 0.75)],
    "lje-cy": [("y", 360, 1, "n", 0.7), ("y", 150, 2, "n", 0.7)],
    "Softsign-cy": [("y", 200, 1, "H", 0.75)], "Hardsign-cy": [("y", 200, 1, "H", 0.75)],
    "Nje-cy": [("y", 600, 1, "H", 0.85), ("y", 200, 2, "H", 0.65)],
    "Lje-cy": [("y", 600, 1, "H", 0.7), ("y", 200, 2, "H", 0.65)],
    "Yeru-cy": [("y", 200, 1, "H", 0.65)],
    "el-cy": [("y", 300, 1, "n", 0.78)], "El-cy": [("y", 400, 1, "H", 0.78)],
    "de-cy": [("y", 300, 1, "n", 0.8)], "De-cy": [("y", 400, 1, "H", 0.8)],
    "em-cy": [("y", 380, 1, "n", 0.3), ("y", 180, 2, "n", 0.15)],
    "ef-cy": [("y", 255, 2, "o", 0.55)], "Ef-cy": [("y", 360, 2, "O", 0.37)],
    "yu-cy": [("y", 255, 1, "o", 0.6)], "Yu-cy": [("y", 360, 1, "O", 0.6)],
    "ya-cy": [("y", 400, 1, "n", 0.6)], "dje-cy": [("y", 300, 1, "n", 0.9)],
    "Dje-cy": [("y", 300, 1, "n", 0.6)],
}


# Curves taken over unchanged in shape from a Latin glyph that itself goes
# past the limits (Instrument's own drawing), checked in the italic only.
INHERITED_ITALIC = {
    "schwa-cy": "e turned round; e's own bowl curve",
    "ze-cy": "the junction of the italic 3, whose handles dip below it too",
}


# Letters with parts above the x-height (or cap height) on purpose.
ASCENDING = {"be-cy", "be-cy.loclSRB", "dje-cy", "tshe-cy", "ef-cy", "zhe-cy.loclBGR", "yu-cy.loclBGR",
             "geupturn-cy", "Geupturn-cy", "gje-cy.loclSRB"}


def check_glyphs(c, built):
    """Run the construction checks on the outlines as they will be written."""
    l, m = c.lc, c.cap
    o, O = c.g("o"), c.g("O")
    refs = {"n": l["Rx0"] - l["Lx1"], "H": m["Rx0"] - m["Lx1"], "xh": 510.0,
            "o": G.bounds([o[1]])[2] - G.bounds([o[1]])[0], "O": G.bounds([O[1]])[2] - G.bounds([O[1]])[0]}
    limit = 0.86 if c.condensed else 0.81
    out = []
    for name, g in built.items():
        if not g.contours:
            continue
        try:
            cons = fix_smooth(G.copy(g.contours))
        except ValueError as e:
            out.append(f"{name}: {e}")
            continue
        final = G.reslant(cons) if c.italic else cons
        if not (c.italic and name in INHERITED_ITALIC):
            out += checks.curve_problems(name, final, limit, check_x=not c.italic, slack=24 if c.italic else 2)
        out += checks.counter_problems(name, cons, PROBES.get(name, []), refs)
        if name not in ("schwa-cy", "Schwa-cy"):  # e turned round: e's one contour draws its eye backwards
            out += checks.self_intersection_problems(name, final)
        if c.italic:
            out += checks.backward_extreme_problems(name, cons, G.SLANT)
        if name not in ASCENDING:
            out += checks.stem_top_problems(name, cons, 510 if name[0].islower() else 720)
    # ђ must carry its ascender bar left of the stem
    d = built["dje-cy"].contours
    stem = G.bounds([d[0]])
    if not checks.has_ink(d, stem[0] - 60, 600, stem[0] - 5, 640):
        out.append("dje-cy: no ascender bar left of the stem")
    # Ґ ґ: the arm runs free between stem and upturn
    for name, V in (("geupturn-cy", l["V"]), ("Geupturn-cy", m["V"])):
        cons = built[name].contours
        free = G.bounds([cons[-1]])[0] - G.bounds([cons[0]])[2]
        if free < UPTURN_FREE * V - 0.5:
            out.append(f"{name}: arm runs free for {free:.0f}, need {UPTURN_FREE * V:.0f}")
    yi = built["yi-cy"]
    b = G.bounds(yi.contours)
    if b[0] < -1.5 or b[2] > yi.width + 1.5:
        out.append(f"yi-cy: dieresis {b[0]:.0f}..{b[2]:.0f} leaves the advance 0..{yi.width:.0f}")
    return [f"{c.master}: {p}" for p in out]


# Advance growth from the light to the bold master, relative to n's (or
# H's), may differ by this share: wider and the bold rhythm turns lumpy,
# narrower and the letter darkens.
GROWTH_TOLERANCE = 0.08
# The Latin letters whose growth a construction should follow, where n or H
# is the wrong model: narrow letters widen faster, three-stem letters
# slower. "b+l" means the letter should widen by as many units as b and l
# together (ы is a b-like bowl and an l-like stem).
GROWTH_REF = {
    "ge-cy": "r", "gje-cy": "r", "geupturn-cy": "r", "ge-cy.loclSRB": "r", "gje-cy.loclSRB": "r",
    "gestroke-cy": "r", "Gestroke-cy": "F", "Ge-cy": "F", "Gje-cy": "F", "Geupturn-cy": "F",
    "yi-cy": "idieresis", "Yi-cy": "Idieresis",
    "sha-cy": "m", "shcha-cy": "m", "sha-cy.loclBGR": "m", "shcha-cy.loclBGR": "m", "Sha-cy": "M", "Shcha-cy": "M",
    "yeru-cy": "b+l", "Yeru-cy": "P+I", "yu-cy": "l+o", "yu-cy.loclBGR": "l+o", "Yu-cy": "I+O",
    "Zhe-cy": "K", "zhe-cy": "k", "zhe-cy.loclBGR": "k", "Kadescender-cy": "K", "kadescender-cy": "k", "ka-cy": "k",
    "Ef-cy": "O", "ef-cy": "o", "U-cy": "Y", "Ustraightstroke-cy": "Y",
    "Ereversed-cy": "C", "E-cy": "C", "ereversed-cy": "c", "e-cy": "c",
    "Lje-cy": "H+P-I", "Nje-cy": "H+P-I", "lje-cy": "n+b-l", "nje-cy": "n+b-l", "hardsign-cy": "b+t-l",
}


# ї Ї: the width is set by the dieresis, which must stay inside the advance.
GROWTH_EXEMPT = {"yi-cy", "Yi-cy",
                 "geupturn-cy", "Geupturn-cy"}  # Ґ ґ: lengthened where the arm must run free (with_upturn)


def growth_problems(results):
    """Every constructed letter must widen with weight about as n or H do,
    in each width and style."""
    by = {master: (c, built) for master, c, built, _ in results}
    out = []
    for light, bold in (("Regular", "Bold"), ("CondensedRegular", "CondensedBold"),
                        ("Italic", "BoldItalic"), ("CondensedItalic", "CondensedBoldItalic")):
        (cl, bl), (cb, bb) = by[light], by[bold]
        for name, g in bl.items():
            if not g.contours or name in GROWTH_EXEMPT:
                continue
            ref = GROWTH_REF.get(name, "n" if name[0].islower() else "H")
            grow = sum((-1 if sign == "-" else 1) * (cb.w(r) - cl.w(r))
                       for sign, r in re.findall(r"([+-]?)([A-Za-z]+)", ref))
            want = (g.width + grow) / g.width
            got = bb[name].width / g.width
            if abs(got / want - 1) > GROWTH_TOLERANCE:
                out.append(f"{light}->{bold}: {name}: advance x{got:.3f} ({g.width:.0f} -> {bb[name].width:.0f}), "
                           f"{ref} x{want:.3f}")
    return out


def dot_edges(c, built, name):
    """Left and right edge of a glyph's top dots, relative to its advance."""
    if name in built and built[name].contours:
        g = built[name]
        b = G.bounds(g.contours)
        return b[0], g.width - b[2], g.width
    dot = c.g(name)[-1]  # i: the dot comes last
    b = G.bounds([dot])
    return b[0], c.w(name) - b[2], c.w(name)


def extra_kerning(c, built):
    """Glyph pairs no group covers: dotted і ї next to each other keep the
    space between their dots wider than the space inside ї's dieresis."""
    out = {}
    g = built["yi-cy"]
    dots = sorted(g.contours, key=lambda d: G.bounds([d])[0])
    inner = G.bounds([dots[1]])[0] - G.bounds([dots[0]])[2]
    for a, b in (("yi-cy", "yi-cy"), ("yi-cy", "i-cy"), ("i-cy", "yi-cy")):
        between = dot_edges(c, built, a)[1] + dot_edges(c, built, b)[0]
        out[(a, b)] = max(0, round(1.7 * inner - between))
    return out


def main():
    """Build and check every master first; write only if all checks pass
    (--force writes anyway, --check only reports)."""
    only = set(a for a in sys.argv[1:] if not a.startswith("-"))
    problems, results = [], []
    fonts = {m: ufoLib2.Font.open(SOURCES / f"TiloSans-{m}.ufo", lazy=True) for m in UPRIGHT + ITALIC}
    for family in (UPRIGHT, ITALIC):
        for master in family:
            c, built = build_all(fonts, master)
            add_composites(c, built)
            problems += check_glyphs(c, built)
            results.append((master, c, built, extra_kerning(c, built)))
    problems += growth_problems(results)
    for p in problems:
        print(p)
    if "--check" in sys.argv or (problems and "--force" not in sys.argv):
        sys.exit(f"{len(problems)} construction problem(s); nothing written" if problems else 0)
    for master, c, built, extra in results:
        if only:
            built = {k: v for k, v in built.items() if k in only}
        write_master(master, c, built)
        if not only:
            kern.update_kerning(SOURCES / f"TiloSans-{master}.ufo", c.italic, list(built),
                                {k: v[0] for k, v in LOOKALIKES.items()}, extra)
    print(f"{len(results)} masters, {len(results[-1][2]) if not only else len(only)} glyphs each")


if __name__ == "__main__":
    main()
