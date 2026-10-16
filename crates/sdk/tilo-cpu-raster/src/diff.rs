//! Where two decoded scenes differ, and which part of the difference is the
//! old picture moved.
//!
//! Draws are compared in order. Everything before the first difference and
//! after the last one is the same draw in both scenes; a pixel can only change
//! under a draw in between, from either scene. A layer whose push or pop is in
//! between counts with its clip bounds.
//!
//! When every differing draw is its old self translated by one whole-pixel
//! offset (a pan or a scroll), the pixels those draws cover opaquely can be
//! moved instead of drawn again, wherever nothing that stayed put is drawn
//! over them, at either end of the move. [`Motion`] describes that.

use std::sync::Arc;

use vello_cpu::ImageSource;
use vello_cpu::kurbo::{Affine, BezPath, PathEl, Point, Rect, Shape};

use crate::decode::{Brush, DisplayList, LayerMode, Op, Style};

/// More differing draws than this are reported as a change everywhere: the
/// bounds would cover most of the scene, and listing them costs more than
/// rendering it.
const MAX_CHANGED: usize = 512;

/// Tolerance for comparing translated coordinates.
const EPSILON: f64 = 1e-3;

/// Part of a difference that is the old picture translated by whole pixels.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Motion {
    pub dx: i32,
    pub dy: i32,
    /// Where the moved draws paint opaquely in the new scene, already cut to
    /// the clips around them.
    pub opaque: Vec<Rect>,
    /// Bounds of draws that did not move but are drawn over moved ones. A
    /// pixel under one of them, or one whose source is, must be drawn.
    pub obstacles: Vec<Rect>,
}

/// What differs between two decoded scenes.
pub(crate) struct Changes {
    pub bounds: Vec<Rect>,
    /// The motion, or why the difference is not one.
    pub motion: Result<Motion, String>,
}

/// Device-space bounds of everything that differs between `old` and `new`,
/// or `None` when the difference is too large to list.
pub(crate) fn changes(old: &DisplayList, new: &DisplayList) -> Option<Changes> {
    let (a, b) = (&old.ops, &new.ops);
    let mut prefix = 0;
    while prefix < a.len() && prefix < b.len() && op_same(old, &a[prefix], new, &b[prefix]) {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < a.len() - prefix
        && suffix < b.len() - prefix
        && op_same(old, &a[a.len() - 1 - suffix], new, &b[b.len() - 1 - suffix])
    {
        suffix += 1;
    }
    let old_range = prefix..a.len() - suffix;
    let new_range = prefix..b.len() - suffix;
    let motion = motion(old, new, old_range.clone(), new_range.clone());
    let mut bounds = Vec::new();
    if motion.is_ok() {
        collect(old, old_range, &mut bounds);
        collect(new, new_range, &mut bounds);
        return Some(Changes { bounds, motion });
    }
    // Draws inserted or removed in between shift the rest by their count;
    // matching the two ranges keeps a hover that adds one draw from
    // repainting everything drawn after it.
    let (old_only, new_only) = unmatched(old, new, old_range, new_range)?;
    if old_only.len() + new_only.len() > MAX_CHANGED {
        return None;
    }
    collect(old, old_only.into_iter(), &mut bounds);
    collect(new, new_only.into_iter(), &mut bounds);
    Some(Changes { bounds, motion })
}

/// Most edits [`unmatched`] looks through before giving up.
const MAX_EDITS: usize = 384;

/// Ops of `old_range` and `new_range` outside a longest common subsequence
/// of equal ops (Myers' diff), or `None` past [`MAX_EDITS`] edits.
fn unmatched(
    old: &DisplayList,
    new: &DisplayList,
    old_range: std::ops::Range<usize>,
    new_range: std::ops::Range<usize>,
) -> Option<(Vec<usize>, Vec<usize>)> {
    let (n, m) = (old_range.len(), new_range.len());
    let hash_a: Vec<u64> = old.ops[old_range.clone()]
        .iter()
        .map(|op| op_hash(old, op))
        .collect();
    let hash_b: Vec<u64> = new.ops[new_range.clone()]
        .iter()
        .map(|op| op_hash(new, op))
        .collect();
    let eq = |x: usize, y: usize| {
        hash_a[x] == hash_b[y]
            && op_eq(
                old,
                &old.ops[old_range.start + x],
                new,
                &new.ops[new_range.start + y],
            )
    };
    let max = (n + m).min(MAX_EDITS);
    let offset = max as isize + 1;
    let mut v = vec![0isize; 2 * max + 3];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let at = |k: isize| (offset + k) as usize;
    let (n_i, m_i) = (n as isize, m as isize);
    let mut found = None;
    'search: for d in 0..=max as isize {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let mut x = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) {
                v[at(k + 1)]
            } else {
                v[at(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n_i && y < m_i && eq(x as usize, y as usize) {
                x += 1;
                y += 1;
            }
            v[at(k)] = x;
            if x >= n_i && y >= m_i {
                found = Some(d);
                break 'search;
            }
            k += 2;
        }
    }
    let edits = found?;
    let (mut old_only, mut new_only) = (Vec::new(), Vec::new());
    let (mut x, mut y) = (n_i, m_i);
    for d in (0..=edits).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = v[at(prev_k)];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            if x == prev_x {
                new_only.push(new_range.start + prev_y as usize);
            } else {
                old_only.push(old_range.start + prev_x as usize);
            }
        }
        x = prev_x;
        y = prev_y;
    }
    Some((old_only, new_only))
}

/// A hash of what `op` draws, equal for ops [`op_eq`] calls equal.
fn op_hash(list: &DisplayList, op: &Op) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let rect = |h: &mut std::collections::hash_map::DefaultHasher, r: &Rect| {
        for v in [r.x0, r.y0, r.x1, r.y1] {
            v.to_bits().hash(h);
        }
    };
    let path = |h: &mut std::collections::hash_map::DefaultHasher, index: usize| {
        let p = list.path(index);
        p.elements().len().hash(h);
        if let Some(PathEl::MoveTo(q)) = p.elements().first() {
            q.x.to_bits().hash(h);
            q.y.to_bits().hash(h);
        }
    };
    std::mem::discriminant(op).hash(&mut h);
    match op {
        Op::Draw { path: p, bbox, .. } | Op::Blur { path: p, bbox, .. } => {
            rect(&mut h, bbox);
            path(&mut h, *p);
        }
        Op::Glyphs { glyphs, bbox, .. } => {
            rect(&mut h, bbox);
            glyphs.len().hash(&mut h);
        }
        Op::Begin { bbox, .. } => rect(&mut h, bbox),
        Op::End => {}
    }
    h.finish()
}

/// The translation of every differing draw, if there is one, with where it
/// can be moved rather than drawn.
fn motion(
    old: &DisplayList,
    new: &DisplayList,
    old_range: std::ops::Range<usize>,
    new_range: std::ops::Range<usize>,
) -> Result<Motion, String> {
    if old_range.is_empty() {
        return Err("nothing-moved".into());
    }
    if old_range.len() != new_range.len() {
        return Err(format!(
            "draw-count {}->{}",
            old_range.len(),
            new_range.len()
        ));
    }
    let pairs = || {
        old_range
            .clone()
            .zip(new_range.clone())
            .map(|(i, j)| (&old.ops[i], &new.ops[j]))
    };
    // The offset most differing draws moved by, in whole pixels.
    let whole = |v: f64| (v - v.round()).abs() < EPSILON && v.abs() < 65536.0;
    let mut offsets: Vec<((i64, i64), usize)> = Vec::new();
    for (a, b) in pairs() {
        if op_same(old, a, new, b) {
            continue;
        }
        let (Some(ba), Some(bb)) = (op_bbox(a), op_bbox(b)) else {
            continue;
        };
        let (dx, dy) = (bb.x0 - ba.x0, bb.y0 - ba.y0);
        if !whole(dx) || !whole(dy) || (dx.abs() < 0.5 && dy.abs() < 0.5) {
            continue;
        }
        let key = (dx.round() as i64, dy.round() as i64);
        match offsets.iter_mut().find(|(offset, _)| *offset == key) {
            Some((_, count)) => *count += 1,
            None => offsets.push((key, 1)),
        }
    }
    let ((dx, dy), _) = offsets
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .ok_or("no-whole-pixel-offset")?;
    let (dx, dy) = (dx as f64, dy as f64);
    let d = (dx.round(), dy.round());

    let mut moved = vec![false; new_range.len()];
    let mut changed = vec![false; new_range.len()];
    let mut obstacles = Vec::new();
    for (k, (a, b)) in pairs().enumerate() {
        if op_same(old, a, new, b) {
            if let Some(bbox) = drawn_bbox(b) {
                obstacles.push(bbox);
            } else if let Op::Begin {
                path,
                transform,
                mode,
                bbox,
                ..
            } = b
            {
                // A layer that stayed put while its content moved: a pixel
                // within the move's reach of its edge can be inside it at one
                // end and outside at the other.
                let shape = path.map(|path| {
                    let path = new.path(path);
                    (
                        device_rect(path, *transform),
                        opaque_interior(path, *transform),
                    )
                });
                match (mode, shape) {
                    (LayerMode::Clip, Some((Some(rect), _))) => {
                        obstacles.extend(edge_bands(rect, d));
                    }
                    // A rounded clip: exact inside its interior, so only its
                    // corners and the interior's edges are uncertain.
                    (LayerMode::Clip, Some((None, Some(inner)))) => {
                        obstacles.extend(edge_bands(inner, d));
                        obstacles.extend(ring(*bbox, inner));
                    }
                    _ => obstacles.push(*bbox),
                }
            }
        } else if op_shift_eq(old, a, new, b, d) {
            moved[k] = true;
        } else {
            // Changed in place: drawn again at both ends of the move.
            changed[k] = true;
            obstacles.extend(op_bbox(a));
            obstacles.extend(op_bbox(b));
        }
    }
    if !moved.contains(&true) {
        return Err(format!("offset {dx},{dy} moved nothing"));
    }
    // Layers around the range clip both ends of the move. Only plain
    // rectangular clips keep a moved pixel equal to its source.
    let mut clip = Rect::new(f64::MIN, f64::MIN, f64::MAX, f64::MAX);
    for op in &new.ops[..new_range.start] {
        if let Op::Begin {
            path,
            transform,
            mode,
            end,
            ..
        } = op
            && *end >= new_range.end
        {
            if !matches!(mode, LayerMode::Clip) {
                return Err("blended-layer".into());
            }
            let rect = path
                .and_then(|path| clip_interior(new.path(path), *transform))
                .ok_or("shaped-clip")?;
            clip = clip.intersect(rect);
        }
    }
    let clip = clip.intersect(clip + vello_cpu::kurbo::Vec2::new(d.0, d.1));
    // Everything after the range is drawn over it.
    for op in &new.ops[new_range.end..] {
        if let Some(bbox) = drawn_bbox(op) {
            obstacles.push(bbox);
        }
    }

    // Opaque coverage of the moved draws, inside the clips they sit in.
    let mut opaque = Vec::new();
    let mut stack: Vec<Option<Rect>> = Vec::new();
    for (k, op) in new.ops[new_range.clone()].iter().enumerate() {
        let inside = stack.last().copied().unwrap_or(Some(clip));
        match op {
            Op::Begin {
                path,
                transform,
                mode,
                ..
            } => {
                // A clip that moved with its content clips both ends alike; one
                // that stayed must contain both ends of the move.
                let rect = match mode {
                    _ if changed[k] => None,
                    LayerMode::Clip => path
                        .and_then(|path| clip_interior(new.path(path), *transform))
                        .map(|rect| {
                            if moved[k] {
                                rect
                            } else {
                                rect.intersect(rect + vello_cpu::kurbo::Vec2::new(d.0, d.1))
                            }
                        }),
                    _ => None,
                };
                stack.push(inside.zip(rect).map(|(outer, rect)| outer.intersect(rect)));
            }
            Op::End => {
                stack.pop();
            }
            Op::Draw {
                path,
                transform,
                style: Style::Fill(_),
                brush,
                ..
            } if moved[k] && brush_opaque(brush) => {
                if let (Some(inside), Some(interior)) =
                    (inside, opaque_interior(new.path(*path), *transform))
                {
                    let rect = inside.intersect(interior);
                    if rect.width() > 0.0 && rect.height() > 0.0 {
                        opaque.push(rect);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(Motion {
        dx: d.0 as i32,
        dy: d.1 as i32,
        opaque,
        obstacles,
    })
}

/// Strips along the edges of `rect`, as wide as the move on either side.
fn edge_bands(rect: Rect, d: (f64, f64)) -> [Rect; 4] {
    let (w, h) = (d.0.abs(), d.1.abs());
    [
        Rect::new(rect.x0 - w, rect.y0 - h, rect.x0 + w, rect.y1 + h),
        Rect::new(rect.x1 - w, rect.y0 - h, rect.x1 + w, rect.y1 + h),
        Rect::new(rect.x0 - w, rect.y0 - h, rect.x1 + w, rect.y0 + h),
        Rect::new(rect.x0 - w, rect.y1 - h, rect.x1 + w, rect.y1 + h),
    ]
}

/// A rectangle inside which a clip path passes everything: the path itself
/// when it is a rectangle, else the interior of a rounded one.
fn clip_interior(path: &BezPath, transform: Affine) -> Option<Rect> {
    device_rect(path, transform).or_else(|| opaque_interior(path, transform))
}

/// `outer` without `inner`, as up to four bands.
fn ring(outer: Rect, inner: Rect) -> Vec<Rect> {
    let inner = outer.intersect(inner);
    if inner.width() <= 0.0 || inner.height() <= 0.0 {
        return vec![outer];
    }
    [
        Rect::new(outer.x0, outer.y0, outer.x1, inner.y0),
        Rect::new(outer.x0, inner.y1, outer.x1, outer.y1),
        Rect::new(outer.x0, inner.y0, inner.x0, inner.y1),
        Rect::new(inner.x1, inner.y0, outer.x1, inner.y1),
    ]
    .into_iter()
    .filter(|band| band.width() > 0.0 && band.height() > 0.0)
    .collect()
}

/// A brush that leaves no pixel it covers translucent: an opaque colour,
/// or an image with no transparent pixel drawn at full alpha (every extend
/// mode repeats opaque pixels).
fn brush_opaque(brush: &Brush) -> bool {
    match brush {
        Brush::Solid(color) => color.components[3] >= 1.0,
        Brush::Image(image, _) => {
            image.sampler.alpha >= 1.0
                && matches!(&image.image, ImageSource::Pixmap(pixmap) if !pixmap.may_have_transparency())
        }
        Brush::Gradient(..) => false,
    }
}

/// Bounds of what an op paints itself; a layer's push paints nothing.
fn drawn_bbox(op: &Op) -> Option<Rect> {
    match op {
        Op::Draw { bbox, .. } | Op::Glyphs { bbox, .. } | Op::Blur { bbox, .. } => Some(*bbox),
        Op::Begin { .. } | Op::End => None,
    }
}

fn op_bbox(op: &Op) -> Option<Rect> {
    match op {
        Op::Draw { bbox, .. }
        | Op::Glyphs { bbox, .. }
        | Op::Blur { bbox, .. }
        | Op::Begin { bbox, .. } => Some(*bbox),
        Op::End => None,
    }
}

/// The device rectangle a path fills, when it is an axis-aligned rectangle
/// under a transform without rotation or shear.
fn device_rect(path: &BezPath, transform: Affine) -> Option<Rect> {
    let [_, b, c, _, _, _] = transform.as_coeffs();
    if b != 0.0 || c != 0.0 {
        return None;
    }
    let mut points = Vec::with_capacity(5);
    for el in path.elements() {
        match el {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => points.push(*p),
            PathEl::ClosePath => {}
            _ => return None,
        }
    }
    if points.len() > 1 && points.first() == points.last() {
        points.pop();
    }
    if points.len() != 4 {
        return None;
    }
    let bounds = path.bounding_box();
    let on_corner = |p: &Point| {
        (p.x == bounds.x0 || p.x == bounds.x1) && (p.y == bounds.y0 || p.y == bounds.y1)
    };
    let axis_aligned = points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .all(|(p, q)| p.x == q.x || p.y == q.y);
    (points.iter().all(on_corner) && axis_aligned).then(|| transform.transform_rect_bbox(bounds))
}

/// A device rectangle inside what a single closed path fills: an
/// axis-aligned rectangle, or a rounded one, whose corners curve within
/// the radius of its bounds.
fn opaque_interior(path: &BezPath, transform: Affine) -> Option<Rect> {
    let [_, b, c, _, _, _] = transform.as_coeffs();
    if b != 0.0 || c != 0.0 {
        return None;
    }
    let bounds = path.bounding_box();
    let mut moves = 0;
    let mut current = Point::ZERO;
    let mut radius: f64 = 0.0;
    let reach = |p: Point| {
        let x = (p.x - bounds.x0).min(bounds.x1 - p.x);
        let y = (p.y - bounds.y0).min(bounds.y1 - p.y);
        x.max(y)
    };
    for el in path.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                moves += 1;
                current = p;
            }
            PathEl::LineTo(p) => {
                if p.x != current.x && p.y != current.y {
                    return None;
                }
                current = p;
            }
            PathEl::QuadTo(c1, p) => {
                radius = radius.max(reach(current)).max(reach(c1)).max(reach(p));
                current = p;
            }
            PathEl::CurveTo(c1, c2, p) => {
                radius = radius
                    .max(reach(current))
                    .max(reach(c1))
                    .max(reach(c2))
                    .max(reach(p));
                current = p;
            }
            PathEl::ClosePath => {}
        }
    }
    if moves != 1 {
        return None;
    }
    let inner = bounds.inset(-radius);
    (inner.width() > 0.0 && inner.height() > 0.0).then(|| transform.transform_rect_bbox(inner))
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < EPSILON
}

/// `b` is `a` moved by device offset `d`.
fn affine_shifted(a: Affine, b: Affine, d: (f64, f64)) -> bool {
    let (ca, cb) = (a.as_coeffs(), b.as_coeffs());
    ca[..4] == cb[..4] && near(cb[4] - ca[4], d.0) && near(cb[5] - ca[5], d.1)
}

fn rect_shifted(a: Rect, b: Rect, d: (f64, f64)) -> bool {
    near(b.x0 - a.x0, d.0)
        && near(b.x1 - a.x1, d.0)
        && near(b.y0 - a.y0, d.1)
        && near(b.y1 - a.y1, d.1)
}

/// Path `pb` under `tb` is path `pa` under `ta` moved by `d` in device space:
/// the transform moved, the path's own coordinates did, or both, as when a
/// parent draws content laid out elsewhere through an extra translation.
fn geometry_shifted(pa: &BezPath, ta: Affine, pb: &BezPath, tb: Affine, d: (f64, f64)) -> bool {
    if pa == pb {
        return affine_shifted(ta, tb, d);
    }
    let (ca, cb) = (ta.as_coeffs(), tb.as_coeffs());
    if ca[..4] != cb[..4] {
        return false;
    }
    let moved = |p: Point, q: Point| {
        let (p, q) = (ta * p, tb * q);
        near(q.x - p.x, d.0) && near(q.y - p.y, d.1)
    };
    pa.elements().len() == pb.elements().len()
        && pa
            .elements()
            .iter()
            .zip(pb.elements())
            .all(|pair| match pair {
                (PathEl::MoveTo(p), PathEl::MoveTo(q)) | (PathEl::LineTo(p), PathEl::LineTo(q)) => {
                    moved(*p, *q)
                }
                (PathEl::QuadTo(p1, p2), PathEl::QuadTo(q1, q2)) => {
                    moved(*p1, *q1) && moved(*p2, *q2)
                }
                (PathEl::CurveTo(p1, p2, p3), PathEl::CurveTo(q1, q2, q3)) => {
                    moved(*p1, *q1) && moved(*p2, *q2) && moved(*p3, *q3)
                }
                (PathEl::ClosePath, PathEl::ClosePath) => true,
                _ => false,
            })
}

fn brush_shifted(a: &Brush, b: &Brush, d: (f64, f64)) -> bool {
    match (a, b) {
        (Brush::Solid(a), Brush::Solid(b)) => a == b,
        (Brush::Gradient(a, at), Brush::Gradient(b, bt)) => a == b && affine_shifted(*at, *bt, d),
        (Brush::Image(a, at), Brush::Image(b, bt)) => {
            let same_pixels = match (&a.image, &b.image) {
                (ImageSource::Pixmap(a), ImageSource::Pixmap(b)) => Arc::ptr_eq(a, b),
                _ => false,
            };
            same_pixels && a.sampler == b.sampler && affine_shifted(*at, *bt, d)
        }
        _ => false,
    }
}

/// `b` draws what `a` drew, possibly encoded differently: the same draw, or
/// one whose path and transform moved against each other.
fn op_same(la: &DisplayList, a: &Op, lb: &DisplayList, b: &Op) -> bool {
    op_eq(la, a, lb, b) || op_shift_eq(la, a, lb, b, (0.0, 0.0))
}

/// `b` is `a` drawn again `d` device pixels away.
fn op_shift_eq(la: &DisplayList, a: &Op, lb: &DisplayList, b: &Op, d: (f64, f64)) -> bool {
    match (a, b) {
        (
            Op::Draw {
                path: pa,
                transform: ta,
                style: sa,
                brush: ba,
                bbox: xa,
            },
            Op::Draw {
                path: pb,
                transform: tb,
                style: sb,
                brush: bb,
                bbox: xb,
            },
        ) => {
            rect_shifted(*xa, *xb, d)
                && style_eq(sa, sb)
                && brush_shifted(ba, bb, d)
                && geometry_shifted(la.path(*pa), *ta, lb.path(*pb), *tb, d)
        }
        (
            Op::Glyphs {
                glyphs: ga,
                style: sa,
                brush: ba,
                bbox: xa,
            },
            Op::Glyphs {
                glyphs: gb,
                style: sb,
                brush: bb,
                bbox: xb,
            },
        ) => {
            rect_shifted(*xa, *xb, d)
                && style_eq(sa, sb)
                && brush_shifted(ba, bb, d)
                && ga.len() == gb.len()
                && la.glyphs[ga.clone()]
                    .iter()
                    .zip(&lb.glyphs[gb.clone()])
                    .all(|(a, b)| {
                        Arc::ptr_eq(&a.outline, &b.outline)
                            && affine_shifted(a.transform, b.transform, d)
                    })
        }
        (
            Op::Blur {
                path: pa,
                transform: ta,
                brush_transform: bta,
                params: qa,
                color: ca,
                bbox: xa,
            },
            Op::Blur {
                path: pb,
                transform: tb,
                brush_transform: btb,
                params: qb,
                color: cb,
                bbox: xb,
            },
        ) => {
            rect_shifted(*xa, *xb, d)
                && ca == cb
                && qa.width == qb.width
                && qa.height == qb.height
                && qa.radius == qb.radius
                && qa.std_dev == qb.std_dev
                && affine_shifted(*bta, *btb, d)
                && geometry_shifted(la.path(*pa), *ta, lb.path(*pb), *tb, d)
        }
        (
            Op::Begin {
                path: pa,
                transform: ta,
                fill: fa,
                mode: ma,
                bbox: xa,
                ..
            },
            Op::Begin {
                path: pb,
                transform: tb,
                fill: fb,
                mode: mb,
                bbox: xb,
                ..
            },
        ) => {
            rect_shifted(*xa, *xb, d)
                && fa == fb
                && ma == mb
                && match (pa, pb) {
                    (Some(pa), Some(pb)) => {
                        geometry_shifted(la.path(*pa), *ta, lb.path(*pb), *tb, d)
                    }
                    (None, None) => true,
                    _ => false,
                }
        }
        (Op::End, Op::End) => true,
        _ => false,
    }
}

fn collect(list: &DisplayList, range: impl Iterator<Item = usize>, out: &mut Vec<Rect>) {
    for index in range {
        match &list.ops[index] {
            Op::Draw { bbox, .. } | Op::Glyphs { bbox, .. } | Op::Blur { bbox, .. } => {
                out.push(*bbox);
            }
            Op::Begin { bbox, .. } => out.push(*bbox),
            Op::End => {
                // The layer this pops: its push is the nearest unmatched one.
                if let Some(Op::Begin { bbox, .. }) = list.ops[..index]
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, op)| matches!(op, Op::Begin { end, .. } if *end == index))
                    .map(|(_, op)| op)
                {
                    out.push(*bbox);
                }
            }
        }
    }
}

/// Path `a` of list `la` has the same elements as path `b` of list `lb`.
fn same_path(la: &DisplayList, a: usize, lb: &DisplayList, b: usize) -> bool {
    la.path(a) == lb.path(b)
}

fn style_eq(a: &Style, b: &Style) -> bool {
    match (a, b) {
        (Style::Fill(a), Style::Fill(b)) => a == b,
        (Style::Stroke(a), Style::Stroke(b)) => a == b,
        _ => false,
    }
}

fn brush_eq(a: &Brush, b: &Brush) -> bool {
    match (a, b) {
        (Brush::Solid(a), Brush::Solid(b)) => a == b,
        (Brush::Gradient(a, at), Brush::Gradient(b, bt)) => a == b && at == bt,
        (Brush::Image(a, at), Brush::Image(b, bt)) => {
            let same_pixels = match (&a.image, &b.image) {
                (ImageSource::Pixmap(a), ImageSource::Pixmap(b)) => Arc::ptr_eq(a, b),
                _ => false,
            };
            same_pixels && a.sampler == b.sampler && at == bt
        }
        _ => false,
    }
}

fn op_eq(la: &DisplayList, a: &Op, lb: &DisplayList, b: &Op) -> bool {
    match (a, b) {
        (
            Op::Draw {
                path: pa,
                transform: ta,
                style: sa,
                brush: ba,
                bbox: xa,
            },
            Op::Draw {
                path: pb,
                transform: tb,
                style: sb,
                brush: bb,
                bbox: xb,
            },
        ) => {
            xa == xb
                && ta == tb
                && style_eq(sa, sb)
                && brush_eq(ba, bb)
                && same_path(la, *pa, lb, *pb)
        }
        (
            Op::Glyphs {
                glyphs: ga,
                style: sa,
                brush: ba,
                bbox: xa,
            },
            Op::Glyphs {
                glyphs: gb,
                style: sb,
                brush: bb,
                bbox: xb,
            },
        ) => {
            xa == xb
                && style_eq(sa, sb)
                && brush_eq(ba, bb)
                && ga.len() == gb.len()
                && la.glyphs[ga.clone()]
                    .iter()
                    .zip(&lb.glyphs[gb.clone()])
                    .all(|(a, b)| Arc::ptr_eq(&a.outline, &b.outline) && a.transform == b.transform)
        }
        (
            Op::Blur {
                path: pa,
                transform: ta,
                brush_transform: bta,
                params: qa,
                color: ca,
                bbox: xa,
            },
            Op::Blur {
                path: pb,
                transform: tb,
                brush_transform: btb,
                params: qb,
                color: cb,
                bbox: xb,
            },
        ) => {
            xa == xb
                && ta == tb
                && bta == btb
                && ca == cb
                && qa.width == qb.width
                && qa.height == qb.height
                && qa.radius == qb.radius
                && qa.std_dev == qb.std_dev
                && same_path(la, *pa, lb, *pb)
        }
        (
            Op::Begin {
                path: pa,
                transform: ta,
                fill: fa,
                mode: ma,
                bbox: xa,
                ..
            },
            Op::Begin {
                path: pb,
                transform: tb,
                fill: fb,
                mode: mb,
                bbox: xb,
                ..
            },
        ) => {
            xa == xb
                && ta == tb
                && fa == fb
                && ma == mb
                && match (pa, pb) {
                    (Some(pa), Some(pb)) => same_path(la, *pa, lb, *pb),
                    (None, None) => true,
                    _ => false,
                }
        }
        (Op::End, Op::End) => true,
        _ => false,
    }
}
