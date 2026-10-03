# /// script
# requires-python = ">=3.11"
# dependencies = ["fonttools>=4.60", "ufoLib2", "skia-pathops", "shapely>=2"]
# ///
"""Draw the Vietnamese marks, stacked tones and horns in all eight masters.

    uv run vendor/fonts/tilo-sans/scripts/vietnamese.py

Everything here is derived from Instrument's own marks and letters
(acutecomb, gravecomb, circumflexcomb, brevecomb, tildecomb, O o U u d đ),
which the script never changes, plus the design values in DESIGN below. It
rewrites, in every master:

* the capital marks: circumflexcomb.case, brevecomb.case, acutecomb.case,
  gravecomb.case, tildecomb.case, hookabovecomb.case, and the narrow tildes
  of ĩ Ĩ (tildecomb.narrow, tildecomb.narrow.case);
* the hook above (hookabovecomb), drawn like the top of the question mark;
* the stacked-mark ligatures, lowercase and .case: circumflex + acute, grave
  or hook beside it on the right; circumflex + tilde, breve + any tone on top;
* the horned letters Ơ ơ Ư ư and horncomb;
* the positions of the marks in every new composite, and y's dot below;
* the đồng sign ₫ and the level of the round dot below (ss01);
* OS/2 usWinAscent/usWinDescent, from the tallest and deepest ink.

Edit DESIGN (or the drawing code), run this, then build.py. The script is
idempotent: running it twice gives the same sources. It does not touch
kerning; scripts/kerning.py sets the Vietnamese kerning from the compiled
fonts.
"""

from __future__ import annotations

import math
import sys
from pathlib import Path

import ufoLib2
from fontTools.misc.bezierTools import solveCubic, splitCubicAtT
from fontTools.misc.transform import Transform
from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.recordingPen import DecomposingRecordingPen
from ufoLib2.objects import Anchor, Component, Contour, Point

sys.path.insert(0, str(Path(__file__).resolve().parent))
from inkgeom import geometry  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
SOURCES = ROOT / "sources"
UPRIGHTS = ["Regular", "Bold", "CondensedRegular", "CondensedBold"]
ITALIC_OF = {"Regular": "Italic", "Bold": "BoldItalic", "CondensedRegular": "CondensedItalic",
             "CondensedBold": "CondensedBoldItalic"}
UPRIGHT_OF = {v: k for k, v in ITALIC_OF.items()}
MASTERS = UPRIGHTS + list(ITALIC_OF.values())
SLANT = math.tan(math.radians(13))
SKEW_Y = 255  # the italic outlines lean about half the x-height

CAP = 720
XHEIGHT = 510
# Marks attach with _top at the x-height; on a capital they move up by
# CAP - XHEIGHT. The .case marks start at CASE_BOTTOM, 42 units above the cap
# height once placed (Instrument's own capital accents start 73 above it).
CASE_BOTTOM = 552

# ------------------------------------------------------------------ design values
# Per upright master; each italic master uses its upright's values and is
# drawn from its own (slanted) marks. Heights are ink heights in font units.
#
# circ_case     capital circumflex: width scale and height; the stroke is
#               restored to the lowercase circumflex's after scaling.
# breve_case    capital breve: height and the thickness of the bowl's bottom;
#               the arms keep their width, the counter stays deep.
# tone_case     acute and grave over a breve (both cases) and the capital
#               tones: height, cut from the bottom so the angle is the master's.
# tilde_case    capital tilde: height and stroke (fraction of the tilde's).
# tilde_narrow  tilde of ĩ, and tilde_narrow_case of Ĩ: width factor, height.
# hook          hook above: W/H box, s stem, r right side, tt top stroke,
#               wt terminal width, yt terminal cut (fraction of H), sx stem
#               position (fraction of W), neck (stem height before it bends).
# side          tones beside the circumflex: height (lowercase, capital),
#               foot `drop` below the circumflex's top, `gap` from it.
# gap_stack     a tone or tilde on top of a breve or circumflex.
# hook_nest     how far off centre a hook over a breve may move to sit lower.
DESIGN = {
    "Regular": dict(
        circ_case=(0.90, 124), breve_case=(114, 54), tone_case=96, tilde_case=(86, 1.0),
        tilde_narrow=(0.76, 116), tilde_narrow_case=(0.66, 116),
        hook=dict(W=184, H=182, s=62, r=62, tt=56, wt=58, yt=0.48, sx=0.42, neck=0.18),
        hook_case=dict(W=164, H=134, s=56, r=56, tt=50, wt=52, yt=0.46, sx=0.43, neck=0.16),
        side=(112, 92), drop=36, gap_side=34, gap_stack=34, hook_nest=40,
    ),
    "Bold": dict(
        circ_case=(0.86, 126), breve_case=(118, 64), tone_case=100, tilde_case=(106, 0.80),
        tilde_narrow=(0.74, 136), tilde_narrow_case=(0.60, 136),
        hook=dict(W=262, H=200, s=94, r=92, tt=74, wt=80, yt=0.40, sx=0.45, neck=0.16),
        hook_case=dict(W=214, H=132, s=78, r=76, tt=58, wt=66, yt=0.38, sx=0.46, neck=0.14),
        side=(110, 92), drop=36, gap_side=34, gap_stack=32, hook_nest=40,
    ),
    "CondensedRegular": dict(
        circ_case=(0.90, 124), breve_case=(114, 54), tone_case=96, tilde_case=(86, 1.0),
        tilde_narrow=(0.80, 116), tilde_narrow_case=(0.70, 116),
        hook=dict(W=162, H=182, s=54, r=54, tt=50, wt=52, yt=0.48, sx=0.42, neck=0.18),
        hook_case=dict(W=146, H=134, s=48, r=48, tt=44, wt=46, yt=0.46, sx=0.43, neck=0.16),
        side=(108, 90), drop=36, gap_side=34, gap_stack=34, hook_nest=40,
    ),
    "CondensedBold": dict(
        circ_case=(0.86, 126), breve_case=(118, 64), tone_case=100, tilde_case=(106, 0.80),
        tilde_narrow=(0.78, 136), tilde_narrow_case=(0.64, 136),
        hook=dict(W=232, H=200, s=86, r=84, tt=70, wt=74, yt=0.40, sx=0.45, neck=0.16),
        hook_case=dict(W=196, H=132, s=72, r=70, tt=54, wt=60, yt=0.38, sx=0.46, neck=0.14),
        side=(106, 88), drop=36, gap_side=34, gap_stack=32, hook_nest=40,
    ),
}

# Horns: reach right of the stem/bowl edge to the horn's outer edge, flat top
# cut, where the outer and inner edges leave the letter below its top (all in
# stroke widths), and the top's rise above the cap height / x-height.
HORN = {
    "Regular": dict(reach=1.7, cut=1.0, outer=1.45, inner=0.5, reach_o=1.2, outer_o=1.5, inner_o=0.55, rise=40),
    "Bold": dict(reach=1.2, cut=0.95, outer=1.2, inner=0.38, reach_o=0.95, outer_o=1.25, inner_o=0.42, rise=40),
    "CondensedRegular": dict(reach=1.7, cut=1.0, outer=1.45, inner=0.5, reach_o=1.2, outer_o=1.5, inner_o=0.55, rise=40),
    "CondensedBold": dict(reach=1.15, cut=0.95, outer=1.2, inner=0.38, reach_o=0.9, outer_o=1.25, inner_o=0.42, rise=40),
}
# Clearance between a horn and the next letter (see build_horns).
HORN_CLEAR = 24

# y's dot below: as far from the descender as the other letters' dots are from
# their bowls and stems.
YDOT_GAP = {"Regular": 46, "Bold": 46, "CondensedRegular": 44, "CondensedBold": 44}


# ------------------------------------------------------------------ point lists
# A contour is a list of [x, y, type, smooth]; type None is an off-curve point.
def pts_of(glyph):
    assert len(glyph.contours) == 1, glyph.name
    return [[p.x, p.y, p.type, p.smooth] for p in glyph.contours[0].points]


def make_contour(pts):
    return Contour(points=[Point(round(x), round(y), t, s) for x, y, t, s in pts])


def translate(pts, dx, dy):
    return [[x + dx, y + dy, t, s] for x, y, t, s in pts]


def skew(pts, k=SLANT, y0=SKEW_Y):
    return [[x + k * (y - y0), y, t, s] for x, y, t, s in pts]


def unslant(pts):
    return skew(pts, -SLANT)


def bounds(pts):
    xs = [p[0] for p in pts]
    ys = [p[1] for p in pts]
    return min(xs), min(ys), max(xs), max(ys)


def upright(fn, pts, italic):
    """Apply fn to an italic mark in unslanted space, so scaling it
    vertically does not change its slant."""
    return skew(fn(unslant(pts))) if italic else fn(pts)


def slide(pts, dy, italic):
    """Move a mark up or down; an italic mark moves along the slant."""
    return translate(pts, SLANT * dy if italic else 0, dy)


# ------------------------------------------------------------------ marks
def truncate_bottom(pts, cut):
    """Line-only shape (acute, grave): raise every bottom vertex along its
    other edge to ymin + cut, so the stroke keeps its angle and width."""
    ymin = min(p[1] for p in pts)
    n = len(pts)
    out = [list(p) for p in pts]
    for i, p in enumerate(pts):
        if abs(p[1] - ymin) > 0.5:
            continue
        for j in ((i - 1) % n, (i + 1) % n):
            q = pts[j]
            if abs(q[1] - ymin) > 0.5:
                t = cut / (q[1] - p[1])
                out[i][0] = p[0] + (q[0] - p[0]) * t
                out[i][1] = ymin + cut
                break
    return out


def tone(pts, height):
    """An acute or grave `height` tall, at the master's own angle."""
    ymin, ymax = min(p[1] for p in pts), max(p[1] for p in pts)
    return truncate_bottom(pts, (ymax - ymin) - height) if height < ymax - ymin else pts


def stroke_axis_angle(pts):
    """Angle of a tone's stroke, from the middle of its bottom edge to the
    middle of its top edge (90 = vertical, < 90 leans right)."""
    ymin, ymax = min(p[1] for p in pts), max(p[1] for p in pts)
    bot = [p[0] for p in pts if abs(p[1] - ymin) < 0.5]
    top = [p[0] for p in pts if abs(p[1] - ymax) < 0.5]
    return math.degrees(math.atan2(ymax - ymin, sum(top) / len(top) - sum(bot) / len(bot)))


def offset_lines(pts, delta):
    """Move every edge of a line-only contour `delta` units outwards and
    rejoin the edges where they meet."""
    n = len(pts)
    area = 0.5 * sum(pts[i][0] * pts[(i + 1) % n][1] - pts[(i + 1) % n][0] * pts[i][1] for i in range(n))
    sgn = 1 if area > 0 else -1
    lines = []
    for i in range(n):
        x0, y0 = pts[i][:2]
        x1, y1 = pts[(i + 1) % n][:2]
        dx, dy = x1 - x0, y1 - y0
        ln = math.hypot(dx, dy)
        lines.append(((x0 + sgn * dy / ln * delta, y0 - sgn * dx / ln * delta), (dx, dy)))
    out = []
    for i in range(n):
        (p1, d1), (p2, d2) = lines[i - 1], lines[i]
        den = d1[0] * d2[1] - d1[1] * d2[0]
        if abs(den) < 1e-9:
            x, y = p2
        else:
            t = ((p2[0] - p1[0]) * d2[1] - (p2[1] - p1[1]) * d2[0]) / den
            x, y = p1[0] + d1[0] * t, p1[1] + d1[1] * t
        out.append([x, y, pts[i][2], pts[i][3]])
    return out


def leg_thickness(pts):
    """Perpendicular stroke of a circumflex's left leg (points 0 and 1 at the
    foot, 7 on its outer edge at the top)."""
    (x0, y0), (x1, _), (x7, y7) = pts[0][:2], pts[1][:2], pts[7][:2]
    return (x1 - x0) * math.sin(math.atan2(y7 - y0, x7 - x0))


APEX_MIN = 0.8  # ... and at least this share of its leg stroke
APEX_MAX = 0.5  # vertical stroke at a capital circumflex's apex, at most this share of its height


def circumflex_case(pts, cx, width, height):
    """Capital circumflex: `width` times as wide as Instrument's and `height`
    tall, drawn the way Instrument draws it (two legs with open corners
    under a flat top) with the lowercase circumflex's leg stroke. The legs
    get flatter; the flat top keeps its share of the width, narrowing only
    as far as the apex needs to stay as thick as the legs read."""
    (x0, b), (x1, _), _, _, _, (x5, _), (x6, top), (x7, _) = [q[:2] for q in pts]
    stroke = leg_thickness(pts)
    h = height
    hw = width * (x5 - x0) / 2
    tf = width * (x6 - x7) / 2
    for _ in range(200):
        th = math.atan2(h, hw - tf)
        # vertical stroke at the apex, where the inner edges cross
        apex = stroke / math.cos(th) - tf * math.tan(th)
        if apex < APEX_MIN * stroke:
            tf -= 0.5
        elif apex > APEX_MAX * h:
            tf += 0.5
        else:
            break
    wl = stroke / math.sin(th)          # horizontal leg width at the foot
    # open corners: the inner edges run on past the point where they cross,
    # but not so far that they poke out of the other leg
    yc = min(b + h - 0.3 * apex, b + h - apex / 2 + tf * math.tan(th) / 2 - 4)
    run = (yc - b) / math.tan(th)
    return [
        [cx - hw, b, "line", False], [cx - hw + wl, b, "line", False],
        [cx - hw + wl + run, yc, "line", False], [cx + hw - wl - run, yc, "line", False],
        [cx + hw - wl, b, "line", False], [cx + hw, b, "line", False],
        [cx + tf, b + h, "line", False], [cx - tf, b + h, "line", False],
    ]


BREVE_OUTER = (0, 1, 2, 3, 11, 12, 13)


def breve_case(pts, height, bottom):
    """Capital breve (Instrument's 14-point breve: tips flat at the top, bowl
    at the bottom): `height` tall, its bowl's bottom `bottom` thick. The arms
    keep their width and the counter its share of the height, so the mark
    stays a cup rather than a solid half disc."""
    ymin = min(p[1] for p in pts)
    ytop = max(p[1] for p in pts)
    k = height / (ytop - ymin)
    top2 = ymin + height
    inner_bottom = pts[7][1]
    c = (top2 - (ymin + bottom)) / (ytop - inner_bottom)
    out = []
    for i, (x, y, t, s) in enumerate(pts):
        if i in BREVE_OUTER:
            out.append([x, ymin + k * (y - ymin), t, s])
        else:
            out.append([x, top2 - (ytop - y) * c, t, s])
    return out


TILDE_UPPER = set(range(4, 14))


def tilde_case(pts, height, thick_scale=1.0):
    """Tilde with a lower wave (Instrument's 20-point tilde); the stroke keeps
    its thickness, or `thick_scale` of it."""
    ys = [p[1] for p in pts]
    lo, hi = min(ys), max(ys)
    mid = (lo + hi) / 2
    thick = pts[10][1] - pts[17][1]  # vertical stroke at the left hump
    t2 = thick * thick_scale
    k = (height - t2) / (hi - lo - thick)
    d = (t2 - k * thick) / 2
    out = []
    for i, (x, y, t, s) in enumerate(pts):
        upper = (i in TILDE_UPPER or i == 3) and i != 13
        out.append([x, mid + k * (y - mid) + (d if upper else -d), t, s])
    return out


def tilde_narrow(pts, k, height):
    """Narrower tilde for i and I: the wave is squeezed, the stroke keeps its
    thickness (terminals and the crossing are widened back)."""
    pts = tilde_case(pts, height)
    xs = [p[0] for p in pts]
    cx = (min(xs) + max(xs)) / 2
    w = pts[3][0] - pts[4][0]  # right terminal width
    d = (1 - k) * w / 2
    return [[cx + k * (x - cx) + (-d if i in TILDE_UPPER else d), y, t, s] for i, (x, y, t, s) in enumerate(pts)]


def hook(p, bottom=583, x0=60):
    """Hook above, built like the top of Instrument's question mark: a short
    stem cut flat at the foot, a round shoulder on the right, and the stroke
    coming down on the left to a flat cut, so the curl stays an open
    crescent."""
    W, H, s, r, tt, wt = p["W"], p["H"], p["s"], p["r"], p["tt"], p["wt"]
    top = bottom + H
    xr = x0 + W
    yt = bottom + p["yt"] * H          # terminal cut
    xs = x0 + p["sx"] * W              # stem's left edge at the foot
    ym = bottom + 0.60 * H             # right extreme
    xc = x0 + 0.50 * W                 # top extreme
    xri = xr - r
    yci = top - tt
    ymi = ym + 0.02 * H
    xci = xc + 0.04 * W
    k = 0.55
    neck = p["neck"]
    return [
        [xs, bottom, "curve", False],
        [xs + s, bottom, "line", False],
        [xs + s, bottom + neck * H, None, False],
        [xr, ym - 0.30 * H, None, False],
        [xr, ym, "curve", True],
        [xr, ym + (top - ym) * k, None, False],
        [xc + (xr - xc) * k, top, None, False],
        [xc, top, "curve", True],
        [xc - (xc - x0) * k, top, None, False],
        [x0, yt + (top - yt) * k, None, False],
        [x0, yt, "curve", False],
        [x0 + wt, yt, "line", False],
        [x0 + wt, yt + (yci - yt) * 0.45, None, False],
        [xci - (xci - x0 - wt) * 0.5, yci, None, False],
        [xci, yci, "curve", True],
        [xci + (xri - xci) * k, yci, None, False],
        [xri, ymi + (yci - ymi) * k, None, False],
        [xri, ymi, "curve", True],
        [xri, ymi - 0.30 * H, None, False],
        [xs, bottom + (neck + 0.10) * H, None, False],
    ]


def hook_stem_x(p, x0=60):
    return x0 + p["sx"] * p["W"] + p["s"] / 2


# ------------------------------------------------------------------ geometry
def ink_of(font, name_or_pts, dx=0, dy=0):
    def draw(pen):
        if isinstance(name_or_pts, str):
            rp = DecomposingRecordingPen(font)
            font[name_or_pts].draw(rp)
            rp.replay(pen)
        else:
            make_contour(name_or_pts).draw(pen)
    return geometry(draw, (1, 0, 0, 1, dx, dy))


def solve(fixed, moving, lo, hi, gap):
    """moving(v) -> geometry, its distance from `fixed` growing with v; the
    v in [lo, hi] where the distance is `gap`."""
    for _ in range(40):
        mid = (lo + hi) / 2
        if fixed.distance(moving(mid)) < gap:
            lo = mid
        else:
            hi = mid
    return (lo + hi) / 2


def set_glyph(font, name, contours=(), components=(), anchors=(), width=0, unicode=None):
    g = font[name] if name in font else font.newGlyph(name)
    g.clearContours()
    g.clearComponents()
    g.clearAnchors()
    for c in contours:
        g.contours.append(make_contour(c))
    for base, dx, dy in components:
        g.components.append(Component(base, Transform(1, 0, 0, 1, round(dx), round(dy))))
    for nm, x, y in anchors:
        g.anchors.append(Anchor(round(x), round(y), nm))
    g.width = width
    if unicode is not None:
        g.unicodes = [unicode]
    return g


def axis_x(anchor_x, y, italic):
    """x of a mark's axis at height y, given its _top anchor's x at the x-height."""
    return anchor_x + (SLANT * (y - XHEIGHT) if italic else 0)


# ------------------------------------------------------------------ marks per master
def build_marks(m: str, fonts) -> None:
    f = fonts[m]
    italic = m in UPRIGHT_OF
    d = DESIGN[UPRIGHT_OF.get(m, m)]

    def anchor(g, n):
        return next(a for a in f[g].anchors if a.name == n)

    def mark_anchors(top_x, top_y):
        return [("_top", top_x, XHEIGHT), ("top", axis_x(top_x, top_y, italic), top_y)]

    def to_case_bottom(pts, overshoot=0):
        return slide(pts, CASE_BOTTOM - overshoot - bounds(pts)[1], italic)

    # single capital marks
    ct = anchor("circumflexcomb", "_top")
    circ = pts_of(f["circumflexcomb"])
    # the circumflex's axis in unslanted space
    circ_cx = ct.x - (SLANT * (XHEIGHT - SKEW_Y) if italic else 0)
    circ_c = to_case_bottom(upright(lambda p: circumflex_case(p, circ_cx, *d["circ_case"]), circ, italic))
    bt = anchor("brevecomb", "_top")
    breve = pts_of(f["brevecomb"])
    breve_c = to_case_bottom(upright(lambda p: breve_case(p, *d["breve_case"]), breve, italic), overshoot=4)
    acute_c = to_case_bottom(tone(pts_of(f["acutecomb"]), d["tone_case"]))
    grave_c = to_case_bottom(tone(pts_of(f["gravecomb"]), d["tone_case"]))
    tilde = pts_of(f["tildecomb"])
    tilde_c = to_case_bottom(upright(lambda p: tilde_case(p, *d["tilde_case"]), tilde, italic))
    tt = anchor("tildecomb", "_top")
    tilde_n = upright(lambda p: tilde_narrow(p, *d["tilde_narrow"]), tilde, italic)
    tilde_nc = upright(lambda p: tilde_narrow(p, *d["tilde_narrow_case"]), tilde, italic)

    hk = hook(d["hook"])
    hkc = hook(d["hook_case"], bottom=CASE_BOTTOM)
    hook_x = hook_stem_x(d["hook"])
    hook_case_x = hook_stem_x(d["hook_case"])
    if italic:
        hk, hkc = skew(hk), skew(hkc)
        hook_x += SLANT * (XHEIGHT - SKEW_Y)
        hook_case_x += SLANT * (XHEIGHT - SKEW_Y)

    set_glyph(f, "circumflexcomb.case", [circ_c], anchors=mark_anchors(ct.x, round(bounds(circ_c)[3])))
    set_glyph(f, "brevecomb.case", [breve_c], anchors=mark_anchors(bt.x, round(bounds(breve_c)[3])))
    set_glyph(f, "acutecomb.case", [acute_c], anchors=mark_anchors(anchor("acutecomb", "_top").x, round(bounds(acute_c)[3])))
    set_glyph(f, "gravecomb.case", [grave_c], anchors=mark_anchors(anchor("gravecomb", "_top").x, round(bounds(grave_c)[3])))
    set_glyph(f, "tildecomb.case", [tilde_c], anchors=mark_anchors(tt.x, round(bounds(tilde_c)[3])))
    tilde_top = anchor("tildecomb", "top")
    set_glyph(f, "tildecomb.narrow", [tilde_n], anchors=[("_top", tt.x, XHEIGHT), ("top", tilde_top.x, tilde_top.y)])
    set_glyph(f, "tildecomb.narrow.case", [tilde_nc], anchors=[("_top", tt.x, XHEIGHT), ("top", tilde_top.x, tilde_top.y)])
    set_glyph(f, "hookabovecomb", [hk], anchors=mark_anchors(hook_x, round(bounds(hk)[3])), unicode=0x0309)
    set_glyph(f, "hookabovecomb.case", [hkc], anchors=mark_anchors(hook_case_x, round(bounds(hkc)[3])))

    # stacked marks
    def geo(pts, dx=0, dy=0):
        return ink_of(f, pts, dx, dy)

    def beside(base_pts, tone_pts):
        """(dx, dy) putting the tone right of the circumflex: its foot `drop`
        below the circumflex's top, `gap_side` from it."""
        b, t = bounds(base_pts), bounds(tone_pts)
        dy = (b[3] - d["drop"]) - t[1]
        start = b[2] - t[0] - 200
        v = solve(geo(base_pts), lambda v: geo(tone_pts, start + v, dy), 0, 500, d["gap_side"])
        return start + v, dy

    def above(base_pts, mark_pts, centre_x, xbias=0.0):
        """(dx, dy) centring the mark on the base's axis, `gap_stack` above it."""
        b, mk = bounds(base_pts), bounds(mark_pts)

        def at(v):
            dy = b[3] - 250 + v - mk[1]
            mid_y = mk[1] + dy + (mk[3] - mk[1]) / 2
            return axis_x(centre_x, mid_y, italic) - (mk[0] + mk[2]) / 2 + xbias, dy
        v = solve(geo(base_pts), lambda v: geo(mark_pts, *at(v)), 0, 600, d["gap_stack"])
        return at(v)

    def above_nested(base_pts, mark_pts, centre_x, reach):
        """Like above(), but the mark may move up to `reach` units off centre
        when that lets it sit lower (a hook's stem dipping into the breve's
        cup); the lowest placement wins, the most central of equals."""
        best = None
        for k in range(-reach, reach + 1, 4):
            dx, dy = above(base_pts, mark_pts, centre_x, xbias=k)
            key = (round(dy / 2), abs(k))
            if best is None or key < best[0]:
                best = (key, dx, dy)
        return best[1], best[2]

    def stack_anchors(parts, base_anchor_x, shift):
        top = max(bounds(q)[3] for q in parts)
        ax = base_anchor_x + shift
        return [("_top", ax, XHEIGHT), ("top", axis_x(ax, round(top), italic), round(top))]

    for case in (False, True):
        sfx = ".case" if case else ""
        cpts = circ_c if case else circ
        cb = bounds(cpts)
        side_h = d["side"][1 if case else 0]
        # acute, grave and hook beside the circumflex, on its right: the grave
        # too, clear of the ascender of a th, kh, ph, đ or l before ầ ề ồ. The
        # pair's anchor moves right by half the tone's overhang, so the pair
        # sits centred over the letter.
        for name in ("acutecomb", "gravecomb"):
            t = tone(pts_of(f[name]), side_h)
            dx, dy = beside(cpts, t)
            placed = translate(t, dx, dy)
            shift = 0.5 * (bounds(placed)[2] - cb[2])
            set_glyph(f, f"circumflexcomb_{name}{sfx}", [placed], components=[(f"circumflexcomb{sfx}", 0, 0)],
                      anchors=stack_anchors([cpts, placed], ct.x, shift))
        hpts = hkc if case else hk
        dx, dy = beside(cpts, hpts)
        placed = translate(hpts, dx, dy)
        shift = 0.5 * (bounds(placed)[2] - cb[2])
        set_glyph(f, f"circumflexcomb_hookabovecomb{sfx}",
                  components=[(f"circumflexcomb{sfx}", 0, 0), (f"hookabovecomb{sfx}", dx, dy)],
                  anchors=stack_anchors([cpts, placed], ct.x, shift))
        # tilde on top of the circumflex
        tpts = tilde_c if case else tilde
        dx, dy = above(cpts, tpts, ct.x)
        set_glyph(f, f"circumflexcomb_tildecomb{sfx}",
                  components=[(f"circumflexcomb{sfx}", 0, 0), (f"tildecomb{sfx}", dx, dy)],
                  anchors=stack_anchors([cpts, translate(tpts, dx, dy)], ct.x, 0))
        # every tone on top of the breve; the acute, grave and hook are the
        # short capital forms in both cases, or the lowercase stack would tower
        # over the ascenders; the tilde keeps its size on lowercase
        bpts = breve_c if case else breve
        for name, comp, mpts in (("acutecomb", "acutecomb.case", acute_c),
                                 ("gravecomb", "gravecomb.case", grave_c),
                                 ("hookabovecomb", "hookabovecomb.case", hkc),
                                 ("tildecomb", "tildecomb" + sfx, tilde_c if case else tilde)):
            # a leaning tone is centred a little towards its foot
            w = bounds(mpts)[2] - bounds(mpts)[0]
            bias = {"acutecomb": -0.06 * w, "gravecomb": 0.06 * w}.get(name, 0.0)
            if name == "hookabovecomb":
                dx, dy = above_nested(bpts, mpts, bt.x, d["hook_nest"])
            else:
                dx, dy = above(bpts, mpts, bt.x, xbias=bias)
            dx, dy = round(dx), round(dy)
            set_glyph(f, f"brevecomb_{name}{sfx}", components=[(f"brevecomb{sfx}", 0, 0), (comp, dx, dy)],
                      anchors=stack_anchors([bpts, translate(mpts, dx, dy)], bt.x, 0))

    # round dot below (ss01): its top level with the square dot's
    sq_top = bounds(pts_of(f["dotbelowcomb"]))[3]
    rd = f["dotbelowcomb.ss01"]
    rpts = pts_of(rd)
    dy = sq_top - bounds(rpts)[3]
    if abs(dy) > 0.5:
        anchors = [(a.name, a.x + (SLANT * dy if italic and a.name == "bottom" else 0), a.y + (dy if a.name == "bottom" else 0))
                   for a in rd.anchors]
        set_glyph(f, "dotbelowcomb.ss01", [slide(rpts, dy, italic)], anchors=anchors)


def build_dong(m: str, fonts) -> None:
    """₫: đ over a bar below the baseline, as thick as đ's crossbar and
    spanning bowl to stem."""
    f = fonts[m]
    italic = m in UPRIGHT_OF
    dc = f["dcroat"]
    cross = [[p.x, p.y, p.type, p.smooth] for p in dc.contours[0].points]
    cb = bounds(cross)
    t = cb[3] - cb[1]
    d_pts = [[p.x, p.y, p.type, p.smooth] for c in f["d"].contours for p in c.points]
    if italic:
        d_pts = unslant(d_pts)
    db = bounds(d_pts)
    top = -90
    x0, x1 = db[0] + 0.11 * (db[2] - db[0]), db[2]
    bar = [[x0, top - t, "line", False], [x1, top - t, "line", False], [x1, top, "line", False], [x0, top, "line", False]]
    if italic:
        bar = skew(bar)
    set_glyph(f, "dong", [cross, bar], components=[("d", 0, 0)], width=dc.width, unicode=0x20AB)
    for side, like in (("public.kern1.", "dcroat"), ("public.kern2.", "d")):
        for gname, members in f.groups.items():
            if gname.startswith(side) and like in members and "dong" not in members:
                f.groups[gname] = list(members) + ["dong"]


# ------------------------------------------------------------------ horns
def horn_path(start, end, ytop, hx1, hx0):
    """Outer edge from `start` (on the letter) to the flat top, the top, and
    the inner edge back down to `end` (on the letter)."""
    (xo, yo), (xe, ye) = start, end
    return [
        [xo + 0.55 * (hx1 - xo), yo + 0.03 * (ytop - yo), None, False],
        [hx1, yo + 0.42 * (ytop - yo), None, False],
        [hx1, ytop, "curve", False],
        [hx0, ytop, "line", False],
        [hx0, ytop - 0.55 * (ytop - ye), None, False],
        [xe + 0.55 * (hx0 - xe), ye + 0.02 * (ytop - ye), None, False],
        [xe, ye, "curve", False],
    ]


def horn_from_stem(pts, top, xi, xo, hp):
    """The stem's right edge, rising to its top-right corner (xo, top), grows
    the horn between `outer` and `inner` stroke widths below the top."""
    w = xo - xi
    hx1 = xo + hp["reach"] * w
    hx0 = hx1 - hp["cut"] * w
    yo, yi = top - hp["outer"] * w, top - hp["inner"] * w
    i = next(k for k, q in enumerate(pts) if abs(q[0] - xo) < 1 and abs(q[1] - top) < 1)
    horn = [[xo, yo, "line", False]] + horn_path((xo, yo), (xo, yi), top + hp["rise"], hx1, hx0) + [[xo, top, "line", False]]
    return pts[:i] + horn + pts[i + 1:], hx1


def split_rising(seg, y):
    """Split a rising cubic at height y."""
    lo, hi = 0.0, 1.0
    for _ in range(60):
        mid = (lo + hi) / 2
        lo, hi = (mid, hi) if splitCubicAtT(*seg, mid)[0][3][1] < y else (lo, mid)
    return splitCubicAtT(*seg, (lo + hi) / 2)


def horn_from_bowl(pts, top, hp, w):
    """The bowl's right-to-top quadrant (points 3..6) grows the horn between
    its outer and inner joins."""
    p3, p4, p5, p6 = [tuple(q[:2]) for q in pts[3:7]]
    xr = p3[0]
    a, rest = split_rising((p3, p4, p5, p6), top - hp["outer_o"] * w)
    _, c = split_rising(rest, top - hp["inner_o"] * w)
    p_out, p_in = a[3], c[0]
    hx1 = xr + hp["reach_o"] * w
    hx0 = hx1 - hp["cut"] * w
    horn = ([[a[1][0], a[1][1], None, False], [a[2][0], a[2][1], None, False], [p_out[0], p_out[1], "curve", False]]
            + horn_path(p_out, p_in, top + hp["rise"], hx1, hx0)
            + [[c[1][0], c[1][1], None, False], [c[2][0], c[2][1], None, False]])
    return pts[:4] + horn + pts[6:], hx1, (p_out, p_in)


HORNED = {"Uhorn": ("U", CAP, 0x01AF), "uhorn": ("u", XHEIGHT, 0x01B0),
          "Ohorn": ("O", CAP, 0x01A0), "ohorn": ("o", XHEIGHT, 0x01A1)}


def build_horns(m: str, fonts):
    """Ơ ơ Ư ư grown from each master's O o U u (italics in unslanted space).
    Returns ơ's horn joins, for horncomb."""
    f = fonts[m]
    italic = m in UPRIGHT_OF
    up = UPRIGHT_OF.get(m, m)
    hp = HORN[up]
    joins = None
    for horned, (base, top, cp) in HORNED.items():
        g = f[base]
        contours = [[[p.x, p.y, p.type, p.smooth] for p in c.points] for c in g.contours]
        if italic:
            contours = [unslant(c) for c in contours]
        if base in ("U", "u"):
            ci, i = max(((ci, k) for ci, c in enumerate(contours) for k, q in enumerate(c) if abs(q[1] - top) < 1),
                        key=lambda ck: contours[ck[0]][ck[1]][0])
            c = contours[ci]
            xo, xi = c[i][0], c[i + 1][0]
            contours[ci], hx1 = horn_from_stem(c, top, xi, xo, hp)
        else:
            outer, inner = contours
            w = outer[3][0] - max(q[0] for q in inner if q[2])
            contours[0], hx1, j = horn_from_bowl(outer, top, hp, w)
            if horned == "ohorn":
                joins = j
        # The same rule in every master, the italics measured unslanted. A
        # horn that never overhangs leaves a hole before n or i (ơn, ưi); one
        # that overhangs as far as a flat-topped neighbour allows runs into
        # the crossbar of t and the arm of T (ợt, ỚT). The advance sits half
        # way between the two, and kerning opens t, T and the like by the rest.
        def lsb(name):
            pts = [[p.x, p.y, p.type, p.smooth] for c in f[name].contours for p in c.points]
            return bounds(unslant(pts) if italic else pts)[0]
        flat, reaching = ("n", "t") if top == XHEIGHT else ("H", "T")
        overhang = hx1 - (lsb(flat) - HORN_CLEAR)
        clear = hx1 + HORN_CLEAR - lsb(reaching)
        width = max(g.width, round((overhang + clear) / 2))
        if italic:
            contours = [skew(c) for c in contours]
        anchors = [(a.name, a.x, a.y) for a in g.anchors if a.name in ("top", "bottom")]
        set_glyph(f, horned, contours, anchors=anchors, width=width, unicode=cp)
    return joins


def build_horncomb(m: str, fonts, joins) -> None:
    """The combining horn: ơ's horn alone, closed inside the bowl's stroke and
    hung on o's topright anchor. ccmp composes o, O, u, U + U+031B into the
    drawn letters, so this only draws on other letters."""
    f = fonts[m]
    italic = m in UPRIGHT_OF
    p2, p1 = joins
    pts = [[p.x, p.y, p.type, p.smooth] for p in f["ohorn"].contours[0].points]
    if italic:
        pts = unslant(pts)
    i2 = next(k for k, q in enumerate(pts) if q[2] and abs(q[0] - p2[0]) < 1.5 and abs(q[1] - p2[1]) < 1.5)
    i1 = next(k for k, q in enumerate(pts) if q[2] and abs(q[0] - p1[0]) < 1.5 and abs(q[1] - p1[1]) < 1.5)
    horn = [list(q) for q in pts[i2:i1 + 1]]
    horn[0][2] = "line"
    inner = [[q.x, q.y, q.type, q.smooth] for q in f["o"].contours[1].points if q.type]
    if italic:
        inner = unslant(inner)
    cx = sum(q[0] for q in inner) / len(inner)
    cy = XHEIGHT / 2

    def inward(pt, frac):
        return [pt[0] + (cx - pt[0]) * frac, pt[1] + (cy - pt[1]) * frac, "line", False]

    contour = horn + [inward(p1, 0.10), inward(p2, 0.10)]
    if italic:
        contour = skew(contour)
    tr = next(a for a in f["o"].anchors if a.name == "topright")
    set_glyph(f, "horncomb", [contour], anchors=[("_topright", tr.x, tr.y)], unicode=0x031B)


# ------------------------------------------------------------------ composites
def place_composites(m: str, fonts) -> None:
    """Every new composite: its marks hang on the base's anchors."""
    f = fonts[m]
    upstream = set(f.lib["public.glyphOrder"])
    for name in sorted(f.keys()):
        if name in upstream:
            continue
        g = f[name]
        if len(g.components) < 2 or g.contours or g.width == 0:
            continue
        base_name = g.components[0].baseGlyph
        banc = {a.name: a for a in f[base_name].anchors}
        if base_name in ("i", "i.ss01"):
            banc["bottom"] = next(a for a in f["idotless"].anchors if a.name == "bottom")
        new = [g.components[0]]
        for comp in g.components[1:]:
            mark = comp.baseGlyph
            if mark.startswith("tildecomb") and name in ("itilde", "Itilde"):
                mark = "tildecomb.narrow.case" if name == "Itilde" else "tildecomb.narrow"
            manc = {a.name: a for a in f[mark].anchors}
            if "_top" in manc:
                ba, ma = banc["top"], manc["_top"]
            elif "_bottom" in manc:
                ba, ma = banc["bottom"], manc["_bottom"]
            else:
                new.append(comp)
                continue
            new.append(Component(mark, Transform(1, 0, 0, 1, round(ba.x - ma.x), round(ba.y - ma.y))))
        if base_name in ("y", "y.ss03") and any(c.baseGlyph.startswith("dotbelowcomb") for c in new):
            new = [new[0], ydot(m, f, base_name, new[1].baseGlyph)]
        g.components = new
        g.width = f[base_name].width  # an accented letter spaces like its letter


def ydot(m, f, base_name, dot):
    """y's dot below: beside the descender, as far from it as the other
    letters' dots are from their bowls and stems, at the same depth."""
    base = ink_of(f, base_name)
    banc = {a.name: a for a in f[base_name].anchors}
    manc = {a.name: a for a in f[dot].anchors}
    x0 = banc["bottom"].x - manc["_bottom"].x
    dx = solve(base, lambda v: ink_of(f, dot, x0 - 100 + v, 0), 0, 400, YDOT_GAP[UPRIGHT_OF.get(m, m)])
    return Component(dot, Transform(1, 0, 0, 1, round(x0 - 100 + dx), 0))


def ink_extremes(fonts) -> tuple[int, int]:
    """(highest, lowest) ink over every glyph of every master."""
    hi, lo = 0, 0
    for f in fonts.values():
        for g in f:
            bp = BoundsPen(f)
            g.draw(bp)
            if bp.bounds:
                lo, hi = min(lo, bp.bounds[1]), max(hi, bp.bounds[3])
    return math.ceil(hi), math.floor(lo)


def main() -> int:
    fonts = {m: ufoLib2.Font.open(SOURCES / f"TiloSans-{m}.ufo") for m in MASTERS}
    for m in MASTERS:
        build_marks(m, fonts)
        build_dong(m, fonts)
        build_horncomb(m, fonts, build_horns(m, fonts))
        # Upstream's i and i.ss01 carry a stray bottom anchor off the stem (as
        # far as 100 units right of it in the italic); without it they inherit
        # dotless i's, centred on the stem, so i + U+0323 draws ị.
        for name in ("i", "i.ss01"):
            fonts[m][name].anchors = [a for a in fonts[m][name].anchors if a.name != "bottom"]
    for m in MASTERS:
        place_composites(m, fonts)
    # Windows clips ink outside usWinAscent/usWinDescent; line spacing comes
    # from typo/hhea (USE_TYPO_METRICS), which stay upstream's.
    hi, lo = ink_extremes(fonts)
    for f in fonts.values():
        f.info.openTypeOS2WinAscent = max(986, hi)
        f.info.openTypeOS2WinDescent = max(350, -lo)
    for f in fonts.values():
        f.save()
    print(f"ok: ink from {lo} to {hi}; usWinAscent {max(986, hi)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
