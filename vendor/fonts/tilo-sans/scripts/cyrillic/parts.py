"""Parametric parts measured from the same master's Latin drawings.

Each part is drawn from numbers taken off one Instrument glyph (the P bowl,
the J tail, the M vertex), so its curve tension, terminals and stroke
contrast follow that master, while its size is set by the letter being built.
Curves are rebuilt from the source's handle ratios rather than by moving
points, so the tension survives any change in height or width.
"""

from __future__ import annotations

import geom as G
from checks import tension
from ctx import pmap


def _tension(con, idx):
    """(tension at the first, at the last on-curve point) of one cubic."""
    t = tension([con[i] for i in idx])
    assert t, "P bowl: no measurable tension"
    return t


def p_params(c):
    """The P bowl of this master: thicknesses, the curve length per unit of
    half-height, and the curve tensions (handle over distance to where the
    tangents meet) at the bar ends and at the side extremes."""
    key = "p_params"
    if key in c._cache:
        return c._cache[key]
    con = c.g("P")[0]
    assert len(con) == 20, "P topology changed"
    pt = [(p[0], p[1]) for p in con]
    stem_in = pt[1][0]
    ov = pt[10][0]
    # outer: bar end (18) -> extreme (15) -> bar end (12); inner: 3 -> 6 -> 9
    o_top, o_ext, o_bot = pt[18], pt[15], pt[12]
    i_top, i_ext, i_bot = pt[3], pt[6], pt[9]
    Y1, Y0 = o_top[1], o_bot[1]
    ob, oe = _tension(con, (12, 13, 14, 15))
    oe2, ob2 = _tension(con, (15, 16, 17, 18))
    ib, ie = _tension(con, (3, 4, 5, 6))
    ie2, ib2 = _tension(con, (6, 7, 8, 9))
    r = dict(
        stem0=pt[0][0], stem1=stem_in, ov=stem_in - ov, Y0=Y0, Y1=Y1,
        t_top=Y1 - i_top[1], t_bot=i_bot[1] - Y0, side=o_ext[0] - i_ext[0], xr=o_ext[0],
        ko=(o_ext[0] - (o_top[0] + o_bot[0]) / 2) / ((Y1 - Y0) / 2),
        ki=(i_ext[0] - (i_top[0] + i_bot[0]) / 2) / ((i_top[1] - i_bot[1]) / 2),
        # tensions (at the bar end, at the extreme), averaged over top and bottom
        ro=((ob + ob2) / 2, (oe + oe2) / 2),
        ri=((ib + ib2) / 2, (ie + ie2) / 2),
    )
    c._cache[key] = r
    return r


def p_bowl(c, x_ov, xr, Y0, Y1, t_top, t_bot, side, k_scale=1.0, mirrored=False):
    """A P-type bowl as one contour, open to the left where it overlaps a
    stem at x_ov: bars of t_top and t_bot, a right side of `side`, outer box
    x_ov..xr by Y0..Y1, with the tensions of this master's P. In the italic
    masters the side extremes lean back as Instrument's italic ones do, so
    they are vertical once the outline is slanted (pass mirrored=True for a
    bowl that will be mirrored to face left)."""
    p = p_params(c)
    ho = (Y1 - Y0) / 2
    yi0, yi1 = Y0 + t_bot, Y1 - t_top
    hi = (yi1 - yi0) / 2
    xo_m, yo_m = xr, Y0 + ho
    xi_m, yi_m = xr - side, yi0 + hi
    xo_a = max(xo_m - p["ko"] * k_scale * ho, x_ov + 2)
    xi_a = max(xi_m - p["ki"] * k_scale * hi, x_ov + 2)
    sl = (G.SLANT if c.italic else 0.0) * (-1 if mirrored else 1)

    def quarter(bx, by, ex, ey, tb, te):
        """Handles from the bar end (bx, by) to the extreme (ex, ey): the
        tangents meet at I, on the bar's line."""
        ix, iy = ex - sl * (by - ey), by
        return (G.P(bx + tb * (ix - bx), by, None), G.P(ex + te * (ix - ex), ey + te * (iy - ey), None))

    P = G.P
    it = quarter(xi_a, yi1, xi_m, yi_m, *p["ri"])
    ib = quarter(xi_a, yi0, xi_m, yi_m, *p["ri"])
    ob = quarter(xo_a, Y0, xo_m, yo_m, *p["ro"])
    ot = quarter(xo_a, Y1, xo_m, yo_m, *p["ro"])
    # the inner part runs clockwise, the outer counter-clockwise: one C shape
    return [
        P(x_ov, yi1), P(xi_a, yi1, "line", True), it[0], it[1], P(xi_m, yi_m, "curve", True),
        ib[1], ib[0], P(xi_a, yi0, "curve", True), P(x_ov, yi0),
        P(x_ov, Y0), P(xo_a, Y0, "line", True), ob[0], ob[1], P(xo_m, yo_m, "curve", True),
        ot[1], ot[0], P(xo_a, Y1, "curve", True), P(x_ov, Y1),
    ]


def bowl_for_counter(c, stem_in, counter, Y0, Y1, t_top, t_bot, side):
    """p_bowl hung on a stem whose inner edge is stem_in, with a counter of
    `counter` units at its widest."""
    p = p_params(c)
    x_ov = stem_in - p["ov"]
    return p_bowl(c, x_ov, stem_in + counter + side, Y0, Y1, t_top, t_bot, side)


def j_tail(c, x_left, V, y_top, reach, depth=1.0, base=None):
    """A vertical stem from y_top down whose foot is J's tail turned to the
    left: `reach` is how far the terminal reaches left of the stem; `depth`
    scales the height of the curve. With `base`, the overshoot is clipped to
    the baseline and the terminal is kept under y=base (Д's leg running into
    its base bar)."""
    J = c.g("J")[0]
    assert len(J) == 16, "J topology changed"
    pts = [(p[0], p[1]) for p in J]
    s_in, s_out = pts[5][0], pts[4][0]  # J's stem: inner (left) and outer edge
    term = pts[12][0]
    xf = pmap([(term, x_left - reach), (s_in, x_left), (s_out, x_left + V)])
    y_bot, y_in = pts[0][1], pts[9][1]  # overshoot bottom, top of the bottom stroke
    y_curve = max(pts[3][1], pts[6][1])
    ypairs = [(y_bot, y_bot), (y_in, y_in), (y_curve, y_in + (y_curve - y_in) * depth), (pts[4][1], y_top)]
    if base is not None:
        t_top = pts[12][1]
        ypairs = [(y_bot, 0), (y_in, base - (t_top - y_in) - 2), (t_top, base - 2),
                  (y_curve, t_top + (y_curve - t_top) * depth), (pts[4][1], y_top)]
    yf = pmap(ypairs)
    return [[xf(p[0]), yf(p[1]), p[2], p[3]] for p in J]
