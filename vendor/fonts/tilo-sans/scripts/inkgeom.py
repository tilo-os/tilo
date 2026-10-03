"""Glyph ink as shapely geometry, for clearance and collision checks.

Outlines are first resolved with skia-pathops (nonzero winding, so overlapping
strokes and the self-crossing open corners of the sources count as one shape),
then flattened into polygons: outer contours minus counters.
"""

from __future__ import annotations

import pathops
import shapely
from fontTools.pens.basePen import BasePen
from fontTools.pens.transformPen import TransformPen

STEPS = 10


class _FlattenPen(BasePen):
    def __init__(self):
        super().__init__(None)
        self.contours: list[list[tuple[float, float]]] = []
        self._cur: list[tuple[float, float]] = []

    def _moveTo(self, pt):
        self._cur = [pt]

    def _lineTo(self, pt):
        self._cur.append(pt)

    def _curveToOne(self, p1, p2, p3):
        p0 = self._getCurrentPoint()
        for i in range(1, STEPS + 1):
            t = i / STEPS
            mt = 1 - t
            x = mt**3 * p0[0] + 3 * mt * mt * t * p1[0] + 3 * mt * t * t * p2[0] + t**3 * p3[0]
            y = mt**3 * p0[1] + 3 * mt * mt * t * p1[1] + 3 * mt * t * t * p2[1] + t**3 * p3[1]
            self._cur.append((x, y))

    def _qCurveToOne(self, p1, p2):
        p0 = self._getCurrentPoint()
        for i in range(1, STEPS + 1):
            t = i / STEPS
            mt = 1 - t
            self._cur.append((mt * mt * p0[0] + 2 * mt * t * p1[0] + t * t * p2[0],
                              mt * mt * p0[1] + 2 * mt * t * p1[1] + t * t * p2[1]))

    def _closePath(self):
        if len(self._cur) >= 3:
            self.contours.append(self._cur)
        self._cur = []

    _endPath = _closePath


def _area(pts):
    return 0.5 * sum(x0 * y1 - x1 * y0 for (x0, y0), (x1, y1) in zip(pts, pts[1:] + pts[:1]))


def geometry(draw, transform=None):
    """draw(pen) draws an outline; returns its ink as a shapely geometry (or None)."""
    path = pathops.Path()
    pen = path.getPen()
    draw(TransformPen(pen, transform) if transform else pen)
    try:
        path.simplify(fix_winding=True)
    except pathops.PathOpsError:
        pass
    flat = _FlattenPen()
    path.draw(flat)
    if not flat.contours:
        return None
    signed = [(_area(c), c) for c in flat.contours]
    outer_sign = 1 if max(signed, key=lambda s: abs(s[0]))[0] > 0 else -1
    fills = [shapely.Polygon(c) for a, c in signed if a * outer_sign > 0]
    holes = [shapely.Polygon(c) for a, c in signed if a * outer_sign < 0]
    ink = shapely.union_all([shapely.make_valid(p) for p in fills])
    if holes:
        ink = ink.difference(shapely.union_all([shapely.make_valid(p) for p in holes]))
    return ink


def gap(a, b) -> tuple[float, float]:
    """(distance, overlap area) between two inks; distance 0 when they touch or overlap."""
    if a is None or b is None:
        return float("inf"), 0.0
    d = a.distance(b)
    overlap = a.intersection(b).area if d == 0 else 0.0
    return d, overlap
