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


def remap(con, on):
    """Move a contour's on-curve points to new positions (`on`: index ->
    (x, y)); each handle keeps its place relative to the box of its own
    segment, so every curve keeps its tangent directions and tension
    however much the segments are stretched, independently of each other."""
    n = len(con)
    out = [p[:] for p in con]
    for i, (x, y) in on.items():
        out[i][0], out[i][1] = x, y
    idx = [i for i, p in enumerate(con) if G.is_on(p)]
    for k, i in enumerate(idx):
        j = idx[(k + 1) % len(idx)]
        span = (j - i) % n
        if span < 2:
            continue
        a, b = con[i], con[j]
        A, B = out[i], out[j]
        for h in range(1, span):
            hi = (i + h) % n
            q = con[hi]
            u = (q[0] - a[0]) / (b[0] - a[0]) if abs(b[0] - a[0]) > 1e-6 else 0.0
            v = (q[1] - a[1]) / (b[1] - a[1]) if abs(b[1] - a[1]) > 1e-6 else 0.0
            out[hi][0] = A[0] + u * (B[0] - A[0]) if abs(b[0] - a[0]) > 1e-6 else A[0] + (q[0] - a[0])
            out[hi][1] = A[1] + v * (B[1] - A[1]) if abs(b[1] - a[1]) > 1e-6 else A[1] + (q[1] - a[1])
    return out


def foot_leg(c, x_left, V, y_top, reach):
    """A vertical stem from y_top down whose foot turns left as J's tail
    does, drawn at a smaller size: the terminal (J's vertical cut) ends
    `reach` left of the stem. J's proportions set the rest: the bottom
    stroke's thickness relative to the stem, the outer turn's height to
    width, the rise of the terminal, and each curve's handles (see remap),
    so the foot is a short J tail with J's modulation, not a squeezed one."""
    J = c.g("J")[0]
    assert len(J) == 16, "J topology changed"
    q = [(p[0], p[1]) for p in J]
    s_out, s_in = q[4][0], q[5][0]
    Vj = s_out - s_in
    y_b, y_i = q[0][1], q[9][1]  # outer bottom (overshoot), inner bottom
    tb = (y_i - y_b) * V / Vj  # bottom stroke thickness, as J's relative to its stem
    tb = min(tb, y_i - y_b + 0.5 * (V - Vj))
    # the inner turn takes 0.45 of the reach, the rest is a short run to the cut
    xi = x_left - 0.45 * reach
    term = x_left - reach
    k_out = (q[3][1] - y_b) / (s_out - q[0][0])  # outer turn: height / width
    k_in = (q[6][1] - y_i) / (s_in - q[9][0])
    rise = (q[12][1] - y_i) / (q[9][0] - q[12][0])  # the tail's lift towards the cut
    lift = (q[13][1] - y_b) / (q[9][0] - q[12][0])
    x_r = x_left + V
    on = {
        0: (xi, y_b),
        3: (x_r, y_b + k_out * (x_r - xi)),
        4: (x_r, y_top),
        5: (x_left, y_top),
        6: (x_left, y_b + tb + max(k_in * (x_left - xi), 0.35 * tb)),
        9: (xi, y_b + tb),
        12: (term, y_b + tb + rise * (xi - term)),
        13: (term, y_b + lift * (xi - term)),
    }
    return remap(J, on)
