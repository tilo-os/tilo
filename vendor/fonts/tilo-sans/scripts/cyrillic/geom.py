"""Outline geometry for constructing glyphs from existing drawings.

A contour is a list of points [x, y, segmentType, smooth] (UFO point
semantics: segmentType is None for off-curve points). Contours keep their
start point and direction through every operation, so a construction run
over compatible masters yields compatible results.
"""

from __future__ import annotations

import math
from copy import deepcopy

SLANT = math.tan(math.radians(13))
SLANT_Y0 = 251.0  # the italic masters are the upright skewed about this height


def P(x, y, t="line", smooth=False):
    return [float(x), float(y), t, smooth]


def copy(contours):
    return deepcopy(contours)


def is_on(p):
    return p[2] is not None


def reverse_contour(c):
    """Reverse a closed contour, keeping the first on-curve point first."""
    n = len(c)
    # rotate so the contour starts on an on-curve point
    first_on = next(i for i, p in enumerate(c) if is_on(p))
    c = c[first_on:] + c[:first_on]
    types = [p[2] for p in c]
    pts = [p[:] for p in c]
    # segment types attach to the end point of a segment. Reversed, a segment
    # ending at on-curve B (type t) starting at on-curve A becomes a segment
    # ending at A with type t.
    on_idx = [i for i, p in enumerate(pts) if is_on(p)]
    new_types = {}
    for k, i in enumerate(on_idx):
        prev = on_idx[k - 1]  # segment from prev to i has type types[i]
        new_types[prev] = types[i]
    out = [pts[0][:]]
    for p in reversed(pts[1:]):
        out.append(p[:])
    # out[0] is pts[0]; then pts[n-1], ..., pts[1]
    index_map = [0] + list(range(n - 1, 0, -1))
    for j, i in enumerate(index_map):
        if is_on(pts[i]):
            out[j][2] = new_types[i]
    return out


def transform(contours, a=1, b=0, c=0, d=1, e=0, f=0):
    """x' = a x + c y + e ; y' = b x + d y + f. Mirrors reverse direction back."""
    out = []
    for con in contours:
        nc = [[a * p[0] + c * p[1] + e, b * p[0] + d * p[1] + f, p[2], p[3]] for p in con]
        if a * d - b * c < 0:
            nc = reverse_contour(nc)
        out.append(nc)
    return out


def shift(contours, dx=0, dy=0):
    return transform(contours, e=dx, f=dy)


def scale(contours, sx, sy=None, ox=0, oy=0):
    sy = sx if sy is None else sy
    return transform(contours, sx, 0, 0, sy, ox - sx * ox, oy - sy * oy)


def mirror_x(contours, axis):
    """Mirror about the vertical line x = axis."""
    return transform(contours, -1, 0, 0, 1, 2 * axis, 0)


def mirror_y(contours, axis):
    return transform(contours, 1, 0, 0, -1, 0, 2 * axis)


def rotate180(contours, cx, cy):
    return transform(contours, -1, 0, 0, -1, 2 * cx, 2 * cy)


def unslant(contours):
    return transform(contours, 1, 0, -SLANT, 1, SLANT * SLANT_Y0, 0)


def reslant(contours):
    return transform(contours, 1, 0, SLANT, 1, -SLANT * SLANT_Y0, 0)


def _attach(con):
    """For each point, the index of the on-curve point it moves with."""
    n = len(con)
    owner = [None] * n
    for i, p in enumerate(con):
        if is_on(p):
            owner[i] = i
    for i, p in enumerate(con):
        if is_on(p):
            continue
        prev_on = is_on(con[(i - 1) % n])
        next_on = is_on(con[(i + 1) % n])
        if prev_on:
            owner[i] = (i - 1) % n
        elif next_on:
            owner[i] = (i + 1) % n
        else:  # middle handle of a run (qcurve): nearest on-curve forward
            j = i
            while not is_on(con[j % n]):
                j += 1
            owner[i] = j % n
    return owner


def stretch(contours, axis, at, delta, below=False):
    """Move every on-curve point beyond `at` (x or y) by delta; handles move
    with their on-curve point, so straight strokes keep their thickness and
    curves keep their tangents."""
    k = 0 if axis == "x" else 1
    out = []
    for con in contours:
        owner = _attach(con)
        nc = [p[:] for p in con]
        for i, p in enumerate(con):
            o = con[owner[i]]
            hit = o[k] < at if below else o[k] > at
            if hit:
                nc[i][k] += delta
        out.append(nc)
    return out


def rect(x0, y0, x1, y1):
    x0, x1 = min(x0, x1), max(x0, x1)
    y0, y1 = min(y0, y1), max(y0, y1)
    return [P(x0, y0), P(x1, y0), P(x1, y1), P(x0, y1)]


def poly(*pts):
    """Closed polygon; give points counter-clockwise (outer)."""
    con = [P(x, y) for x, y in pts]
    if area([con]) < 0:
        con = reverse_contour(con)
    return con


def quad_stroke(x0, y0, x1, y1, w0, w1=None):
    """A straight stroke between two centre points with horizontal widths w0/w1
    (horizontal cuts at both ends, as Instrument's diagonals are drawn)."""
    w1 = w0 if w1 is None else w1
    return poly((x0 - w0 / 2, y0), (x0 + w0 / 2, y0), (x1 + w1 / 2, y1), (x1 - w1 / 2, y1))


def area(contours):
    """Signed area (approximate with control polygon); positive = CCW."""
    total = 0
    for con in contours:
        n = len(con)
        for i in range(n):
            x0, y0 = con[i][0], con[i][1]
            x1, y1 = con[(i + 1) % n][0], con[(i + 1) % n][1]
            total += x0 * y1 - x1 * y0
    return total / 2


def bounds(contours):
    """Bounds of the actual curves (on-curve extremes plus cubic extrema)."""
    xs, ys = [], []
    for con in contours:
        n = len(con)
        on = [i for i, p in enumerate(con) if is_on(p)]
        for i in on:
            xs.append(con[i][0]); ys.append(con[i][1])
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
                for t in [s / 16 for s in range(1, 16)]:
                    x, y = _cubic(seg, t)
                    xs.append(x); ys.append(y)
    return min(xs), min(ys), max(xs), max(ys)


def _cubic(seg, t):
    (x0, y0), (x1, y1), (x2, y2), (x3, y3) = [(p[0], p[1]) for p in seg]
    mt = 1 - t
    x = mt**3 * x0 + 3 * mt * mt * t * x1 + 3 * mt * t * t * x2 + t**3 * x3
    y = mt**3 * y0 + 3 * mt * mt * t * y1 + 3 * mt * t * t * y2 + t**3 * y3
    return x, y


def interpolate(a, b, tx, ty=None):
    """Blend two compatible outlines, separately in x and y."""
    ty = tx if ty is None else ty
    out = []
    for ca, cb in zip(a, b):
        assert len(ca) == len(cb), "incompatible contours"
        out.append([[pa[0] + (pb[0] - pa[0]) * tx, pa[1] + (pb[1] - pa[1]) * ty, pa[2], pa[3]] for pa, pb in zip(ca, cb)])
    return out


def round_contours(contours):
    return [[[round(p[0]), round(p[1]), p[2], p[3]] for p in con] for con in contours]


def select(contours, *idx):
    return [deepcopy(contours[i]) for i in idx]


def move_points(contours, pred, dx=0, dy=0):
    """Move on-curve points matching pred(x, y) (and their handles)."""
    out = []
    for con in contours:
        owner = _attach(con)
        nc = [p[:] for p in con]
        for i in range(len(con)):
            o = con[owner[i]]
            if pred(o[0], o[1]):
                nc[i][0] += dx
                nc[i][1] += dy
        out.append(nc)
    return out


def _line_meet(p, d, q, e):
    det = d[0] * e[1] - d[1] * e[0]
    if abs(det) < 1e-9:
        return None
    t = ((q[0] - p[0]) * e[1] - (q[1] - p[1]) * e[0]) / det
    return p[0] + t * d[0], p[1] + t * d[1]


def retilt(contours, slope_from, slope_to, tol=0.12):
    """Turn the tangents at side extremes from dx/dy = slope_from to
    slope_to, keeping each curve's tension (handle length over the distance
    to where its tangents meet). Mirroring an italic outline flips the lean
    of its side extremes; retilt(mirrored, s, -s) puts it back."""
    k = slope_to - slope_from
    out = []
    for con in contours:
        n = len(con)
        # on-curve points whose tangent (through both neighbours) leans slope_from
        turn = set()
        for i, p in enumerate(con):
            if not is_on(p):
                continue
            a, b = con[i - 1], con[(i + 1) % n]
            if is_on(a) and is_on(b):
                continue
            dx, dy = b[0] - a[0], b[1] - a[1]
            if is_on(a):
                dx, dy = b[0] - p[0], b[1] - p[1]
            elif is_on(b):
                dx, dy = p[0] - a[0], p[1] - a[1]
            if abs(dy) > 1e-6 and abs(dx / dy - slope_from) < tol:
                turn.add(i)
        nc = [p[:] for p in con]
        on = [i for i, p in enumerate(con) if is_on(p)]
        for j, i in enumerate(on):
            prev = on[j - 1]
            if (i - prev) % n != 3 or not ({prev, i} & turn):
                continue
            h1, h2 = (prev + 1) % n, (prev + 2) % n
            P0, P3 = con[prev], con[i]
            d0 = (con[h1][0] - P0[0], con[h1][1] - P0[1])
            d1 = (con[h2][0] - P3[0], con[h2][1] - P3[1])
            n0 = (d0[0] + k * d0[1], d0[1]) if prev in turn else d0
            n1 = (d1[0] + k * d1[1], d1[1]) if i in turn else d1
            I0 = _line_meet(P0, d0, P3, d1)
            I1 = _line_meet(P0, n0, P3, n1)
            ok = I0 is not None and I1 is not None
            if ok:
                t0 = math.hypot(*d0) / max(math.hypot(I0[0] - P0[0], I0[1] - P0[1]), 1e-9)
                t1 = math.hypot(*d1) / max(math.hypot(I0[0] - P3[0], I0[1] - P3[1]), 1e-9)
                e0 = (I1[0] - P0[0], I1[1] - P0[1])
                e1 = (I1[0] - P3[0], I1[1] - P3[1])
                # the new meeting point must lie ahead of both handles
                ok = e0[0] * n0[0] + e0[1] * n0[1] > 0 and e1[0] * n1[0] + e1[1] * n1[1] > 0 and t0 < 1.5 and t1 < 1.5
            if ok:
                nc[h1][0], nc[h1][1] = P0[0] + t0 * e0[0], P0[1] + t0 * e0[1]
                nc[h2][0], nc[h2][1] = P3[0] + t1 * e1[0], P3[1] + t1 * e1[1]
            else:  # S-curve or parallel tangents: turn the handles, keep their lengths
                for hi, Pt, d, dn in ((h1, P0, d0, n0), (h2, P3, d1, n1)):
                    L, Ln = math.hypot(*d), math.hypot(*dn)
                    if Ln:
                        nc[hi][0], nc[hi][1] = Pt[0] + dn[0] / Ln * L, Pt[1] + dn[1] / Ln * L
        out.append(nc)
    return out


def cap_tension(contours, limit):
    """Shorten the handles of curves tighter than `limit` (zone maps can
    stretch a handle past its curve's proportions) to exactly `limit`."""
    out = []
    for con in contours:
        n = len(con)
        nc = [p[:] for p in con]
        on = [i for i, p in enumerate(con) if is_on(p)]
        for j, i in enumerate(on):
            prev = on[j - 1]
            if (i - prev) % n != 3:
                continue
            h1, h2 = (prev + 1) % n, (prev + 2) % n
            P0, P3 = con[prev], con[i]
            d0 = (con[h1][0] - P0[0], con[h1][1] - P0[1])
            d1 = (con[h2][0] - P3[0], con[h2][1] - P3[1])
            I = _line_meet(P0, d0, P3, d1)
            if I is None:
                continue
            for hi, Pt, d in ((h1, P0, d0), (h2, P3, d1)):
                e = (I[0] - Pt[0], I[1] - Pt[1])
                L, Le = math.hypot(*d), math.hypot(*e)
                if e[0] * d[0] + e[1] * d[1] <= 0 or not Le:
                    continue
                if L / Le > limit:
                    nc[hi][0], nc[hi][1] = Pt[0] + d[0] * limit * Le / L, Pt[1] + d[1] * limit * Le / L
        out.append(nc)
    return out


def mirror_x_italic(contours, axis, italic):
    """mirror_x for outlines in upright-equivalent space: in the italic the
    side extremes are turned back to Instrument's lean (vertical once
    slanted)."""
    out = mirror_x(contours, axis)
    return retilt(out, SLANT, -SLANT) if italic else out
