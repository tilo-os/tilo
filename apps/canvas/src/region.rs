//! Areas of the artwork: what a selection covers, what a shape covers once
//! shapes are combined. A region is polygons, outer edges one way round and
//! holes the other, so it fills the same under either fill rule; it becomes
//! SVG path data where it is stored or drawn.
//!
//! Combining (union, subtract, intersect, exclude) is exact on the polygons;
//! curves are followed to a small fraction of a unit and fitted again when a
//! combined shape becomes a path.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo's geometry operators (Affine, Point, Vec2) are float geometry, not overflowing integer arithmetic"
)]

use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::simplify::SimplifyShape;
use i_overlay::float::single::SingleFloatOverlay;
use i_overlay::mesh::float::outline::offset::OutlineOffset;
use i_overlay::mesh::float::style::{LineJoin, OutlineStyle};
use ui::vello::kurbo::{
    Affine, BezPath, ParamCurve as _, ParamCurveNearest as _, PathEl, PathSeg, Point, Rect,
    Shape as _,
};

/// How closely curves are followed when they become polygons, in units.
const TOLERANCE: f64 = 0.05; // geometric accuracy in artwork units, not spacing

/// The most corners a region keeps; a finer one is simplified until it fits.
pub const MAX_CORNERS: usize = 24_000;

type Contour = Vec<[f64; 2]>;

/// An area: shapes, each an outer edge and its holes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Region {
    shapes: Vec<Vec<Contour>>,
}

/// How two regions are combined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// Everything either covers.
    Union,
    /// The first, less the second.
    Subtract,
    /// Only what both cover.
    Intersect,
    /// What one or the other covers, not both.
    Exclude,
}

impl Op {
    /// The operation a word names: `union`/`add`, `subtract`, `intersect`,
    /// `exclude`.
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "union" | "add" => Some(Self::Union),
            "subtract" => Some(Self::Subtract),
            "intersect" => Some(Self::Intersect),
            "exclude" => Some(Self::Exclude),
            _ => None,
        }
    }

    fn rule(self) -> OverlayRule {
        match self {
            Self::Union => OverlayRule::Union,
            Self::Subtract => OverlayRule::Difference,
            Self::Intersect => OverlayRule::Intersect,
            Self::Exclude => OverlayRule::Xor,
        }
    }
}

fn finite(contour: &[[f64; 2]]) -> bool {
    contour
        .iter()
        .all(|[x, y]| x.is_finite() && y.is_finite() && x.abs() < 1e9 && y.abs() < 1e9)
}

impl Region {
    /// The area `path` fills (non-zero), its curves followed closely. An
    /// open subpath is closed back to its start.
    pub fn of_path(path: &BezPath) -> Self {
        let mut contours: Vec<Contour> = Vec::new();
        ui::vello::kurbo::flatten(path.iter(), TOLERANCE, |element| match element {
            PathEl::MoveTo(at) => contours.push(vec![[at.x, at.y]]),
            PathEl::LineTo(at) => {
                if let Some(contour) = contours.last_mut() {
                    contour.push([at.x, at.y]);
                }
            }
            _ => {}
        });
        Self::of_contours(contours)
    }

    /// The area of polygons that may cross themselves and each other,
    /// filled non-zero.
    fn of_contours(mut contours: Vec<Contour>) -> Self {
        contours.retain(|contour| contour.len() >= 3 && finite(contour));
        if contours.is_empty() {
            return Self::default();
        }
        Self {
            shapes: contours.simplify_shape(FillRule::NonZero),
        }
    }

    pub fn of_rect(rect: Rect) -> Self {
        Self::of_path(&rect.to_path(0.1))
    }

    pub fn is_empty(&self) -> bool {
        self.shapes.iter().all(Vec::is_empty)
    }

    /// `self` combined with `other`.
    #[must_use]
    pub fn combine(&self, other: &Self, op: Op) -> Self {
        if other.is_empty() {
            return match op {
                Op::Union | Op::Subtract | Op::Exclude => self.clone(),
                Op::Intersect => Self::default(),
            };
        }
        if self.is_empty() {
            return match op {
                Op::Union | Op::Exclude => other.clone(),
                Op::Subtract | Op::Intersect => Self::default(),
            };
        }
        Self {
            shapes: self
                .shapes
                .overlay(&other.shapes, op.rule(), FillRule::NonZero),
        }
    }

    /// The region grown outward by `by` units, its corners rounded; a
    /// negative `by` shrinks it.
    #[must_use]
    pub fn grown(&self, by: f64) -> Self {
        if by == 0.0 || self.is_empty() || !by.is_finite() {
            return self.clone();
        }
        let style = OutlineStyle::new(by).line_join(LineJoin::Round(0.2));
        Self {
            shapes: self.shapes.outline(&style),
        }
    }

    /// Everything within `bounds` the region does not cover.
    #[must_use]
    pub fn inverted(&self, bounds: Rect) -> Self {
        Self::of_rect(bounds).combine(self, Op::Subtract)
    }

    /// The box around the region.
    pub fn bounds(&self) -> Option<Rect> {
        let mut points = self.shapes.iter().flatten().flatten();
        let first = points.next()?;
        let start = Rect::from_points((first[0], first[1]), (first[0], first[1]));
        Some(points.fold(start, |rect, [x, y]| rect.union_pt((*x, *y))))
    }

    /// How much it covers, in square units.
    pub fn area(&self) -> f64 {
        self.shapes
            .iter()
            .flatten()
            .map(|contour| signed_area(contour))
            .sum::<f64>()
            .abs()
    }

    /// How many corners its polygons have.
    pub fn corners(&self) -> usize {
        self.shapes.iter().flatten().map(Vec::len).sum()
    }

    /// The polygons as a path: straight edges, every contour closed.
    pub fn to_path(&self) -> BezPath {
        let mut path = BezPath::new();
        for contour in self.shapes.iter().flatten() {
            let mut points = contour.iter();
            let Some([x, y]) = points.next() else {
                continue;
            };
            path.move_to((*x, *y));
            for [x, y] in points {
                path.line_to((*x, *y));
            }
            path.close_path();
        }
        path
    }

    /// The region as a path that follows `sources`, the outlines it was
    /// made from, wherever its edges lie on them: a stretch along a curve is
    /// that stretch of the curve again, exactly, and a stretch along a line
    /// is one line.
    pub fn to_curves(&self, sources: &[BezPath]) -> BezPath {
        let segments: Vec<(PathSeg, Rect)> = sources
            .iter()
            .flat_map(BezPath::segments)
            .map(|segment| (segment, segment.bounding_box().inflate(NEAR, NEAR)))
            .collect();
        let mut path = BezPath::new();
        for contour in self.shapes.iter().flatten() {
            follow(contour, &segments, &mut path);
        }
        path
    }

    /// Fewer corners: each contour's points within `tolerance` units of a
    /// straight run are dropped.
    #[must_use]
    pub fn simplified(&self, tolerance: f64) -> Self {
        let contours = self
            .shapes
            .iter()
            .flatten()
            .map(|contour| douglas_peucker(contour, tolerance))
            .collect();
        Self::of_contours(contours)
    }

    /// At most [`MAX_CORNERS`] corners, simplified as little as that takes.
    #[must_use]
    pub fn bounded(self, start: f64) -> Self {
        let mut region = self;
        let mut tolerance = start;
        while region.corners() > MAX_CORNERS && tolerance < 64.0 {
            region = region.simplified(tolerance);
            tolerance *= 2.0;
        }
        region
    }

    /// The region under `affine`.
    #[must_use]
    pub fn transformed(&self, affine: Affine) -> Self {
        let map = |[x, y]: [f64; 2]| {
            let at = affine * Point::new(x, y);
            [at.x, at.y]
        };
        Self {
            shapes: self
                .shapes
                .iter()
                .map(|shape| {
                    shape
                        .iter()
                        .map(|contour| contour.iter().copied().map(map).collect())
                        .collect()
                })
                .collect(),
        }
    }

    /// Whether `point` is inside.
    pub fn contains(&self, point: Point) -> bool {
        self.to_path().contains(point)
    }
}

/// How near an edge's ends and middle must lie to a source segment to be
/// part of it, in units: the polygons follow curves to [`TOLERANCE`].
const NEAR: f64 = 0.15; // geometric accuracy in artwork units, not spacing

/// Where `point` lies on `segment`, if it lies on it.
fn on(segment: &PathSeg, point: Point) -> Option<f64> {
    let nearest = segment.nearest(point, 1e-7);
    (nearest.distance_sq <= NEAR * NEAR).then_some(nearest.t)
}

/// A stretch of a source segment an edge lies along: the segment, and
/// where the edge starts and ends on it.
type Along = Option<(usize, f64, f64)>;

/// Append `contour` to `path`, each run of edges along one source segment
/// as that segment's stretch.
fn follow(contour: &[[f64; 2]], segments: &[(PathSeg, Rect)], path: &mut BezPath) {
    let count = contour.len();
    if count < 3 {
        return;
    }
    let point = |index: usize| {
        let [x, y] = contour.get(index % count).copied().unwrap_or_default();
        Point::new(x, y)
    };
    let along: Vec<Along> = (0..count)
        .map(|index| {
            let (a, b) = (point(index), point(index + 1));
            segments
                .iter()
                .enumerate()
                .filter(|(_, (_, reach))| reach.contains(a) && reach.contains(b))
                .find_map(|(which, (segment, _))| {
                    let start = on(segment, a)?;
                    let end = on(segment, b)?;
                    on(segment, a.midpoint(b))?;
                    ((end - start).abs() > 1e-9).then_some((which, start, end))
                })
        })
        .collect();
    let edge = |index: usize| along.get(index % count).copied().flatten();
    let forward = |start: f64, end: f64| end > start;
    // Whether edge `index` carries on along the stretch the one before it is.
    let continues = |index: usize| match (edge(index + count - 1), edge(index)) {
        (Some((a, a_start, a_end)), Some((b, b_start, b_end))) => {
            a == b
                && (a_end - b_start).abs() < 1e-3
                && forward(a_start, a_end) == forward(b_start, b_end)
        }
        _ => false,
    };
    let first = (0..count).find(|index| !continues(*index)).unwrap_or(0);
    path.move_to(point(first));
    let mut index = first;
    let end = first + count;
    while index < end {
        let Some((which, start, mut stop)) = edge(index) else {
            index += 1;
            path.line_to(point(index));
            continue;
        };
        let mut next = index + 1;
        while next < end && continues(next) {
            if let Some((_, _, further)) = edge(next) {
                stop = further;
            }
            next += 1;
        }
        let to = point(next);
        match segments
            .get(which)
            .map(|(segment, _)| segment.subsegment(start..stop))
        {
            Some(PathSeg::Quad(quad)) => path.quad_to(quad.p1, to),
            Some(PathSeg::Cubic(cubic)) => path.curve_to(cubic.p1, cubic.p2, to),
            Some(PathSeg::Line(_)) | None => path.line_to(to),
        }
        index = next;
    }
    path.close_path();
}

fn signed_area(contour: &[[f64; 2]]) -> f64 {
    let mut sum = 0.0;
    let mut previous = contour.last().copied().unwrap_or_default();
    for &[x, y] in contour {
        sum += previous[0] * y - x * previous[1];
        previous = [x, y];
    }
    sum / 2.0
}

/// A closed polygon with the points that stray less than `tolerance` from
/// the straight run between their neighbours dropped.
fn douglas_peucker(contour: &[[f64; 2]], tolerance: f64) -> Contour {
    if contour.len() < 4 {
        return contour.to_vec();
    }
    // Split the ring at its first point and the point farthest from it, and
    // simplify the two halves as open runs.
    let first = contour.first().copied().unwrap_or_default();
    let far = contour
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| distance(first, **a).total_cmp(&distance(first, **b)))
        .map_or(0, |(index, _)| index);
    let mut keep = vec![false; contour.len()];
    if let Some(slot) = keep.first_mut() {
        *slot = true;
    }
    if let Some(slot) = keep.get_mut(far) {
        *slot = true;
    }
    let mut ring: Vec<[f64; 2]> = contour.to_vec();
    ring.push(first);
    let mut spans = vec![(0, far), (far, contour.len())];
    while let Some((from, to)) = spans.pop() {
        if to <= from.saturating_add(1) {
            continue;
        }
        let (Some(a), Some(b)) = (ring.get(from).copied(), ring.get(to).copied()) else {
            continue;
        };
        let mut worst = (0.0, from);
        for index in from.saturating_add(1)..to {
            if let Some(point) = ring.get(index).copied() {
                let away = off_line(point, a, b);
                if away > worst.0 {
                    worst = (away, index);
                }
            }
        }
        if worst.0 > tolerance {
            if let Some(slot) = keep.get_mut(worst.1) {
                *slot = true;
            }
            spans.push((from, worst.1));
            spans.push((worst.1, to));
        }
    }
    contour
        .iter()
        .zip(keep)
        .filter_map(|(point, kept)| kept.then_some(*point))
        .collect()
}

fn distance(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// How far `point` is from the line through `a` and `b`.
fn off_line(point: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let length = distance(a, b);
    if length < 1e-12 {
        return distance(point, a);
    }
    ((b[0] - a[0]) * (a[1] - point[1]) - (a[0] - point[0]) * (b[1] - a[1])).abs() / length
}

/// A grid of yes/no cells, row by row: a region as pixels.
pub struct Mask {
    pub width: usize,
    pub height: usize,
    pub cells: Vec<bool>,
}

impl Mask {
    fn at(&self, x: usize, y: usize) -> bool {
        y.checked_mul(self.width)
            .and_then(|row| row.checked_add(x))
            .and_then(|index| self.cells.get(index))
            .copied()
            .unwrap_or(false)
    }

    /// The cells that are set, as a region in units: `to_units` maps a
    /// cell's corner to the artwork. Its staircase edges are smoothed to
    /// lines through the middle of each step.
    pub fn traced(&self, to_units: Affine) -> Region {
        let mut runs: Vec<Contour> = Vec::new();
        for y in 0..self.height {
            let mut x = 0;
            while x < self.width {
                if !self.at(x, y) {
                    x = x.saturating_add(1);
                    continue;
                }
                let start = x;
                while x < self.width && self.at(x, y) {
                    x = x.saturating_add(1);
                }
                let (x0, x1, y0) = (cell(start), cell(x), cell(y));
                runs.push(vec![[x0, y0], [x1, y0], [x1, y0 + 1.0], [x0, y0 + 1.0]]);
            }
        }
        let steps = Region::of_contours(runs);
        let smooth = steps
            .shapes
            .iter()
            .flatten()
            .map(|contour| douglas_peucker(&smooth_steps(contour), 0.35))
            .collect();
        Region::of_contours(smooth).transformed(to_units)
    }
}

/// A traced outline with its staircases read as the lines through their
/// steps' middles. A corner between two long edges stays a corner.
fn smooth_steps(contour: &[[f64; 2]]) -> Contour {
    let count = contour.len();
    let point = |index: usize| contour.get(index % count).copied().unwrap_or_default();
    // A step is an edge one cell long.
    let step = |index: usize| distance(point(index), point(index + 1)) <= 1.0 + 1e-9;
    let mut smooth = Vec::with_capacity(count);
    for index in 0..count {
        if step(index) {
            let (a, b) = (point(index), point(index + 1));
            smooth.push([f64::midpoint(a[0], b[0]), f64::midpoint(a[1], b[1])]);
        } else if !step(index + count - 1) {
            smooth.push(point(index));
        }
    }
    smooth
}

#[allow(
    clippy::cast_precision_loss,
    reason = "cell indices are at most a few thousand"
)]
fn cell(index: usize) -> f64 {
    index as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, side: f64) -> Region {
        Region::of_rect(Rect::new(x, y, x + side, y + side))
    }

    #[test]
    fn regions_combine_exactly() {
        let a = square(0.0, 0.0, 10.0);
        let b = square(5.0, 5.0, 10.0);
        let close = |region: Region, area: f64| {
            assert!((region.area() - area).abs() < 1e-6, "{}", region.area());
        };
        close(a.combine(&b, Op::Union), 175.0);
        close(a.combine(&b, Op::Subtract), 75.0);
        close(a.combine(&b, Op::Intersect), 25.0);
        close(a.combine(&b, Op::Exclude), 150.0);
        close(a.inverted(Rect::new(0.0, 0.0, 20.0, 20.0)), 300.0);
        let hole = square(0.0, 0.0, 30.0).combine(&square(10.0, 10.0, 10.0), Op::Subtract);
        assert!(!hole.contains(Point::new(15.0, 15.0)));
        assert!(hole.contains(Point::new(5.0, 5.0)));
    }

    #[test]
    fn a_region_grows_and_shrinks() {
        let a = square(0.0, 0.0, 10.0);
        let grown = a.grown(2.0).bounds().unwrap_or_default();
        assert!((grown.x0 + 2.0).abs() < 0.05 && (grown.x1 - 12.0).abs() < 0.05);
        let shrunk = a.grown(-2.0);
        assert!((shrunk.area() - 36.0).abs() < 0.1, "{}", shrunk.area());
    }

    #[test]
    fn a_mask_traces_to_its_outline() {
        // A filled disc of radius 20 cells.
        let (width, height) = (60, 60);
        let cells = (0..width * height)
            .map(|index| {
                let (x, y) = (cell(index % width) + 0.5, cell(index / width) + 0.5);
                (x - 30.0).hypot(y - 30.0) <= 20.0
            })
            .collect();
        let mask = Mask {
            width,
            height,
            cells,
        };
        let region = mask.traced(Affine::scale(2.0));
        let area = region.area();
        let disc = std::f64::consts::PI * 40.0 * 40.0;
        assert!((area - disc).abs() / disc < 0.03, "{area} against {disc}");
        assert!(region.corners() < 120, "{} corners", region.corners());
        assert!(region.contains(Point::new(60.0, 60.0)));
    }
}
