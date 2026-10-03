"""Construction checks: curve tension, handles past their segment, and
counters that close up in some master.

construct.py runs these on every glyph it builds and refuses to write the
sources when one fails, so a construction that looks right in Regular cannot
quietly collapse in the corners of the design space.
"""

from __future__ import annotations

import math

import geom as G
from ctx import flatten, xcuts, ycuts

# Instrument's own Latin tops out around 0.77 (the 99th percentile is about
# 0.70); anything past this reads as a squared-off corner.
MAX_TENSION = 0.80
# A handle may lie this far outside the box of its segment's end points.
HANDLE_SLACK = 2.0


def segments(con):
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
        yield seg


def _intersect(p0, d0, p1, d1):
    det = d0[0] * d1[1] - d0[1] * d1[0]
    if abs(det) < 1e-9:
        return None
    t = ((p1[0] - p0[0]) * d1[1] - (p1[1] - p0[1]) * d1[0]) / det
    s = ((p1[0] - p0[0]) * d0[1] - (p1[1] - p0[1]) * d0[0]) / det
    return t, s


def tension(seg):
    """Handle length over the distance to where the two tangents meet, for
    each handle; None for S-curves and nearly straight segments."""
    if len(seg) != 4:
        return None
    p0, h1, h2, p3 = [(p[0], p[1]) for p in seg]
    d0 = (h1[0] - p0[0], h1[1] - p0[1])
    d1 = (h2[0] - p3[0], h2[1] - p3[1])
    l0, l1 = math.hypot(*d0), math.hypot(*d1)
    if l0 < 1 or l1 < 1:
        return None
    r = _intersect(p0, (d0[0] / l0, d0[1] / l0), p3, (d1[0] / l1, d1[1] / l1))
    if r is None:
        return None
    t, s = r
    if t <= 1 or s <= 1:
        return None
    # skip shallow curves: tangents within 30 degrees of each other
    cosang = -(d0[0] * d1[0] + d0[1] * d1[1]) / (l0 * l1)
    if cosang > math.cos(math.radians(30)):
        return None
    return l0 / t, l1 / s


def curve_problems(name, contours, max_tension=MAX_TENSION, check_x=True, slack=HANDLE_SLACK):
    """Handles outside their segment's box (in x only if check_x: italic
    side extremes are slanted) and over-tight curves."""
    out = []
    for ci, con in enumerate(contours):
        for seg in segments(con):
            if len(seg) != 4:
                continue
            p0, p3 = seg[0], seg[3]
            x0, x1 = sorted((p0[0], p3[0]))
            y0, y1 = sorted((p0[1], p3[1]))
            for h in seg[1:3]:
                in_x = x0 - slack <= h[0] <= x1 + slack or not check_x
                if not (in_x and y0 - slack <= h[1] <= y1 + slack):
                    out.append(f"{name}: contour {ci}: handle ({h[0]:.0f},{h[1]:.0f}) outside its segment "
                               f"({p0[0]:.0f},{p0[1]:.0f})-({p3[0]:.0f},{p3[1]:.0f})")
            t = tension(seg)
            if t and max(t) > max_tension:
                out.append(f"{name}: contour {ci}: tension {max(t):.2f} at ({p0[0]:.0f},{p0[1]:.0f})-({p3[0]:.0f},{p3[1]:.0f})")
    return out


def stem_top_problems(name, contours, height, near=60, min_len=150):
    """Vertical straight edges at least min_len long whose top ends within
    `near` of `height` must end exactly on it: a stem that stops short of
    the x-height (or cap height) is a construction slip, not a design."""
    out = []
    for ci, con in enumerate(contours):
        n = len(con)
        for i, p in enumerate(con):
            q = con[(i + 1) % n]
            if not (G.is_on(p) and q[2] == "line") or abs(p[0] - q[0]) > 0.5 or abs(p[1] - q[1]) < min_len:
                continue
            top = max(p[1], q[1])
            # a stem's top is a flat cut: the next or previous edge runs level
            # for a good part of a stem (M's notch edges do not)
            j = (i + 1) % n if q[1] > p[1] else i
            k = (j + 1) % n if q[1] > p[1] else (i - 1) % n
            a, b = con[j], con[k]
            flat = G.is_on(a) and G.is_on(b) and abs(a[1] - b[1]) < 0.5 and abs(a[0] - b[0]) >= 30
            if flat and abs(top - height) <= near and abs(top - height) > 0.5:
                out.append(f"{name}: contour {ci}: stem edge at x={p[0]:.0f} ends at y={top:.0f}, not {height}")
    return out


def twisted_area(con):
    """Area where a contour winds against its own direction: where its
    inner side has crossed its outer one. Instrument's own outlines overlap
    themselves on purpose (a diagonal running on into a stem: winding 2);
    a twist (winding -1 inside a counter-clockwise contour) never is.
    With A the signed area, U the nonzero and E the even-odd area of the
    contour, the twisted area is (2U - E - |A|) / 2."""
    import pathops
    from fontTools.pens.areaPen import AreaPen

    def draw(pen):
        pts = [((p[0], p[1]), p[2]) for p in con]
        start = next(i for i, (_, t) in enumerate(pts) if t is not None)
        pts = pts[start + 1:] + pts[:start + 1]
        pen.moveTo(pts[-1][0])
        buf = []
        for xy, t in pts:
            if t is None:
                buf.append(xy)
            elif t == "line":
                pen.lineTo(xy)
            else:
                pen.curveTo(*buf, xy)
                buf = []
        pen.closePath()

    ap = AreaPen()
    draw(ap)
    path = pathops.Path()
    draw(path.getPen())
    U = pathops.simplify(path, fix_winding=False).area
    path.fillType = pathops.FillType.EVEN_ODD
    E = pathops.simplify(path, fix_winding=False).area
    return (2 * U - E - abs(ap.value)) / 2


def self_intersection_problems(name, contours, limit=40.0):
    """Contours whose inner and outer sides cross (see twisted_area)."""
    out = []
    for ci, con in enumerate(contours):
        t = twisted_area(con)
        if t > limit:
            out.append(f"{name}: contour {ci} crosses itself ({t:.0f} units² wound backwards)")
    return out


def backward_extreme_problems(name, contours, slant, tol=0.05):
    """Italic only, on outlines slanted back upright: the tangent at every
    point on a curve's side (a smooth on-curve point between two handles,
    tangent within about 27 degrees of vertical) must be vertical there (the
    slanted design leans with the italic) or lean forward (Instrument's
    italic draws its extremes vertical in the slanted design, which leans
    them back by the slant here); a side leaning the other way belongs to a
    mirrored italic curve whose stress runs backwards."""
    out = []
    for ci, con in enumerate(contours):
        n = len(con)
        for i, p in enumerate(con):
            a, b = con[i - 1], con[(i + 1) % n]
            if not G.is_on(p) or G.is_on(a) or G.is_on(b):
                continue
            dy = b[1] - a[1]
            if abs(dy) < 1:
                continue
            k = (b[0] - a[0]) / dy
            if abs(k) > 0.5:  # not a side: a top, bottom or diagonal curve
                continue
            if k > tol:
                out.append(f"{name}: contour {ci}: side extreme at ({p[0]:.0f},{p[1]:.0f}) leans {k:+.2f} "
                           f"(upright 0, italic {-slant:+.2f})")
    return out


def ink_runs(contours, y=None, x=None):
    """Filled intervals along a scanline (nonzero winding)."""
    hits = []
    for con in contours:
        pts = flatten(con)
        for (ax, ay), (bx, by) in zip(pts, pts[1:] + pts[:1]):
            if y is not None:
                if (ay <= y < by) or (by <= y < ay):
                    hits.append((ax + (y - ay) * (bx - ax) / (by - ay), 1 if by > ay else -1))
            else:
                if (ax <= x < bx) or (bx <= x < ax):
                    hits.append((ay + (x - ax) * (by - ay) / (bx - ax), 1 if bx > ax else -1))
    hits.sort()
    runs, wind, start = [], 0, None
    for v, d in hits:
        was = wind != 0
        wind += d
        if not was and wind != 0:
            start = v
        elif was and wind == 0:
            runs.append((start, v))
    return runs


def gaps(contours, y=None, x=None):
    runs = ink_runs(contours, y=y, x=x)
    return [b[0] - a[1] for a, b in zip(runs, runs[1:])]


def counter_problems(name, contours, probes, refs):
    """probes: (axis, at, count, ref, fraction): along the scanline there must
    be at least `count` interior gaps of at least fraction * refs[ref]."""
    out = []
    for axis, at, count, ref, frac in probes:
        gs = gaps(contours, y=at) if axis == "y" else gaps(contours, x=at)
        need = frac * refs[ref]
        ok = [g for g in gs if g >= need - 0.5]
        if len(ok) < count:
            out.append(f"{name}: at {axis}={at:.0f} gaps {[round(g) for g in gs]} — need {count} of >= {need:.0f} "
                       f"({frac:.2f} x {ref} {refs[ref]:.0f})")
    return out


def has_ink(contours, x0, y0, x1, y1, step=4):
    for y in range(int(y0), int(y1), step):
        for a, b in ink_runs(contours, y=y + 0.5):
            if b > x0 and a < x1:
                return True
    return False


__all__ = ["curve_problems", "counter_problems", "self_intersection_problems", "backward_extreme_problems", "stem_top_problems", "gaps", "has_ink", "ink_runs", "tension", "xcuts", "ycuts"]
