"""Small outline-construction kit for drawing Greek in Instrument's vocabulary.

Contours are lists of points [x, y, type, smooth] in UFO point order
(type None = off-curve, 'line' / 'curve' = on-curve). Everything is built in
upright space; italic masters are unskewed on read and reskewed on write.
"""
from __future__ import annotations

import math
from fontTools.misc.bezierTools import splitCubicAtT, solveCubic

TAN = math.tan(math.radians(13))
SKEW_Y = 255


def skew(x, y, sign=1):
    return x + sign * (y - SKEW_Y) * TAN, y


# ------------------------------------------------------------ contour utils

def P(x, y, t="line", smooth=False):
    return [float(x), float(y), t, smooth]


def area(contour):
    """Signed area of the polygon through all points (on and off)."""
    a = 0.0
    n = len(contour)
    for i in range(n):
        x0, y0 = contour[i][0], contour[i][1]
        x1, y1 = contour[(i + 1) % n][0], contour[(i + 1) % n][1]
        a += x0 * y1 - x1 * y0
    return a / 2


def segments(contour):
    """Split a closed UFO contour into segments: (kind, [p0, ..., pn])."""
    n = len(contour)
    # rotate so we start at an on-curve point
    start = next(i for i, p in enumerate(contour) if p[2] is not None)
    pts = contour[start:] + contour[:start]
    segs = []
    cur = [pts[0]]
    for p in pts[1:] + [pts[0]]:
        cur.append(p)
        if p[2] is not None:
            segs.append((p[2], cur))
            cur = [p]
    return segs


def from_segments(segs):
    """Inverse of segments(): list of (kind, [p0..pn]) -> contour."""
    out = []
    for kind, pts in segs:
        for p in pts[1:]:
            out.append(list(p))
    # last point equals first: move it to the front for UFO order
    out = [out[-1]] + out[:-1]
    return out


def reverse(contour):
    """Reverse direction, keeping a valid UFO point order."""
    out = []
    for kind, pts in reversed(segments(contour)):
        rp = [list(p) for p in reversed(pts)]
        for p in rp[1:-1]:
            p[2] = None
        rp[-1][2] = kind
        out.extend(rp[1:])
    return [out[-1]] + out[:-1]


def ccw(contour):
    return contour if area(contour) > 0 else reverse(contour)


def cw(contour):
    return contour if area(contour) < 0 else reverse(contour)


def transform(contour, fn):
    out = []
    for x, y, t, s in contour:
        nx, ny = fn(x, y)
        out.append([nx, ny, t, s])
    return out


def translate(contour, dx, dy=0):
    return transform(contour, lambda x, y: (x + dx, y + dy))


def mirror_x(contour, axis):
    return reverse(transform(contour, lambda x, y: (2 * axis - x, y)))


def rect(x0, y0, x1, y1):
    return ccw([P(x0, y0), P(x1, y0), P(x1, y1), P(x0, y1)])


def poly(*pts):
    return ccw([P(x, y) for x, y in pts])


def rounded(contour):
    return [[round(x), round(y), t, s] for x, y, t, s in contour]


# ------------------------------------------------------------ curves


def cubic_t_at(p0, p1, p2, p3, axis, value):
    """Parameter t in [0,1] where the cubic's coordinate `axis` equals value."""
    a0, a1, a2, a3 = p0[axis], p1[axis], p2[axis], p3[axis]
    a = -a0 + 3 * a1 - 3 * a2 + a3
    b = 3 * a0 - 6 * a1 + 3 * a2
    c = -3 * a0 + 3 * a1
    d = a0 - value
    roots = [r for r in solveCubic(a, b, c, d) if -1e-6 <= r <= 1 + 1e-6]
    if not roots:
        raise ValueError(f"no t for {axis}={value} on {p0} {p1} {p2} {p3}")
    return min(max(roots[0], 0.0), 1.0)


def split(p0, p1, p2, p3, t):
    a, b = splitCubicAtT(tuple(p0[:2]), tuple(p1[:2]), tuple(p2[:2]), tuple(p3[:2]), t)
    return a, b


def handle(p, d, length):
    return (p[0] + d[0] * length, p[1] + d[1] * length)


def unit(dx, dy):
    l = math.hypot(dx, dy)
    return (dx / l, dy / l)


def tangent_vec(spec):
    if spec == "h":
        return (1.0, 0.0)
    if spec == "v":
        return (0.0, 1.0)
    a = math.radians(spec)
    return (math.cos(a), math.sin(a))


class Stroke:
    """A skeleton through extreme points, outlined with an elliptical pen.

    nodes: list of dicts {x, y, t: 'h'|'v'|angle, w: width scale, seg: 'curve'|'line'}
    The tangent sign follows the travel direction automatically.
    Thickness across a stroke whose normal is n: sqrt((V*nx)^2 + (H*ny)^2),
    V = vertical-stroke weight, H = horizontal-stroke weight.
    """

    def __init__(self, V, H, kh=0.6, kv=0.58, slant=0.0):
        self.V, self.H, self.kh, self.kv = V, H, kh, kv
        # Italic masters are drawn upright and sheared on write. A shear thickens
        # strokes running like "\" (an o's top-right and bottom-left) and thins
        # those running like "/" by about 11% at 13 degrees; Instrument's italic
        # corrects this by hand. `slant` (tan of the italic angle) pre-compensates
        # each node's width so the sheared stroke keeps the upright's modulation.
        self.slant = slant
        self.nodes = []

    def add(self, x, y, t, seg="curve", w=1.0, wl=None, wr=None, k=None):
        self.nodes.append(dict(x=x, y=y, t=t, seg=seg, w=w, wl=wl, wr=wr, k=k))
        return self

    def _dirs(self):
        n = self.nodes
        dirs = []
        for i, node in enumerate(n):
            tv = tangent_vec(node["t"])
            if i + 1 < len(n):
                dx, dy = n[i + 1]["x"] - node["x"], n[i + 1]["y"] - node["y"]
            else:
                dx, dy = node["x"] - n[i - 1]["x"], node["y"] - n[i - 1]["y"]
            if tv[0] * dx + tv[1] * dy < 0:
                tv = (-tv[0], -tv[1])
            dirs.append(tv)
        return dirs

    def sides(self):
        dirs = self._dirs()
        left, right = [], []
        for node, d in zip(self.nodes, dirs):
            nx, ny = -d[1], d[0]
            w = math.sqrt((self.V * nx) ** 2 + (self.H * ny) ** 2)
            f = 1.0
            if self.slant:
                T = self.slant
                cos = 1 / math.sqrt(1 + T * T)
                # wanted thickness after the shear (verticals keep Instrument's
                # italic stem, horizontals are unchanged), times the factor by
                # which the shear divides a band with normal (nx, ny)
                want = math.sqrt((self.V * cos * nx) ** 2 + (self.H * ny) ** 2)
                f = want * math.sqrt(nx * nx + (ny - nx * T) ** 2) / w
            wl = (node["wl"] if node["wl"] is not None else w * node["w"]) * f / 2
            wr = (node["wr"] if node["wr"] is not None else w * node["w"]) * f / 2
            left.append((node["x"] + nx * wl, node["y"] + ny * wl))
            right.append((node["x"] - nx * wr, node["y"] - ny * wr))
        return dirs, left, right

    def _k(self, d, node):
        if node["k"] is not None:
            return node["k"]
        # blend between horizontal and vertical handle ratios
        return self.kh * abs(d[0]) + self.kv * abs(d[1])

    def _side_segments(self, pts, dirs):
        segs = []
        for i in range(len(pts) - 1):
            a, b = pts[i], pts[i + 1]
            if self.nodes[i + 1]["seg"] == "line":
                segs.append(("line", [a, b]))
            else:
                d0, d1 = dirs[i], dirs[i + 1]
                dx, dy = b[0] - a[0], b[1] - a[1]
                l0 = self._k(d0, self.nodes[i]) * abs(dx * d0[0] + dy * d0[1])
                l1 = self._k(d1, self.nodes[i + 1]) * abs(dx * d1[0] + dy * d1[1])
                segs.append(("curve", [a, handle(a, d0, l0), handle(b, d1, -l1), b]))
        return segs

    def outline(self, start_cut=None, end_cut=None):
        """Closed contour. start_cut / end_cut: ('y', value) or ('x', value)
        trims the first / last segment of both sides at that line, giving a
        cut terminal (Instrument cuts its terminals horizontally)."""
        dirs, left, right = self.sides()
        ls = self._side_segments(left, dirs)
        rs = self._side_segments(right, dirs)
        if start_cut:
            ls[0] = trim_segment(ls[0], start_cut, keep="end")
            rs[0] = trim_segment(rs[0], start_cut, keep="end")
        if end_cut:
            ls[-1] = trim_segment(ls[-1], end_cut, keep="start")
            rs[-1] = trim_segment(rs[-1], end_cut, keep="start")
        # contour: right side forward, cap, left side backward, cap
        pts = []

        def emit(segs, reverse_dir):
            seq = segs
            if reverse_dir:
                seq = [(k, list(reversed(p))) for k, p in reversed(segs)]
            for k, p in seq:
                for q in p[1:-1]:
                    pts.append([q[0], q[1], None, False])
                pts.append([p[-1][0], p[-1][1], k, False])

        start = rs[0][1][0]
        pts.append([start[0], start[1], "line", False])
        emit(rs, False)
        end_left = ls[-1][1][-1]
        pts.append([end_left[0], end_left[1], "line", False])
        emit(ls, True)
        # last emitted point is the left start; the closing line returns to start
        pts = pts[:-1] + [pts[-1]]
        # mark smoothness at interior nodes
        _mark_smooth(pts)
        if pts[-1][0] == pts[0][0] and pts[-1][1] == pts[0][1]:
            pts = pts[:-1]
        return ccw(pts)


def _line_cut(seg, cut):
    """A cut along an arbitrary line: ('line', (px, py), angle in degrees).
    Rotates the segment so the line is horizontal and cuts at y=0."""
    _, (px, py), ang = cut
    a = math.radians(ang)
    c, s = math.cos(a), math.sin(a)
    # rotate by -a about p: the line becomes the x-axis
    rot = [((x - px) * c + (y - py) * s, -(x - px) * s + (y - py) * c) for x, y in (q[:2] for q in seg[1])]
    return rot, (c, s, px, py)


def trim_segment(seg, cut, keep):
    kind, p = seg
    if cut[0] == "line":
        rot, (c, s, px, py) = _line_cut(seg, cut)
        if kind == "line":
            a, b = rot
            t = a[1] / (a[1] - b[1])
        else:
            t = cubic_t_at(*rot, 1, 0.0)
        if kind == "line":
            a, b = p
            m = (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t)
            return ("line", [m, b]) if keep == "end" else ("line", [a, m])
        s0, s1 = split(*p, t)
        return ("curve", list(s1)) if keep == "end" else ("curve", list(s0))
    axis = 0 if cut[0] == "x" else 1
    if kind == "line":
        a, b = p
        if abs(b[axis] - a[axis]) < 1e-9:
            return seg
        t = (cut[1] - a[axis]) / (b[axis] - a[axis])
        m = (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t)
        return ("line", [m, b]) if keep == "end" else ("line", [a, m])
    t = cubic_t_at(*p, axis, cut[1])
    s0, s1 = split(*p, t)
    return ("curve", list(s1)) if keep == "end" else ("curve", list(s0))


def _mark_smooth(pts):
    n = len(pts)
    for i, p in enumerate(pts):
        if p[2] is None:
            continue
        prev = pts[i - 1]
        nxt = pts[(i + 1) % n]
        v0 = (p[0] - prev[0], p[1] - prev[1])
        v1 = (nxt[0] - p[0], nxt[1] - p[1])
        l0, l1 = math.hypot(*v0), math.hypot(*v1)
        if l0 < 1e-6 or l1 < 1e-6:
            continue
        cross = (v0[0] * v1[1] - v0[1] * v1[0]) / (l0 * l1)
        dot = (v0[0] * v1[0] + v0[1] * v1[1]) / (l0 * l1)
        if abs(cross) < 1e-3 and dot > 0 and (prev[2] is None or nxt[2] is None):
            p[3] = True


def cubic_segment(a, da, b, db, ka, kb):
    """Cubic from a (tangent da) to b (tangent db) with handle ratios."""
    dx, dy = b[0] - a[0], b[1] - a[1]
    la = ka * abs(dx * da[0] + dy * da[1])
    lb = kb * abs(dx * db[0] + dy * db[1])
    return [a, handle(a, da, la), handle(b, db, -lb), b]


