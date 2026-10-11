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
use ui::num::f64_of_usize;
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

    /// How many separate shapes it is made of.
    pub fn parts(&self) -> usize {
        self.shapes.len()
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

/// A grid of coverage, row by row: how much of each cell an area covers,
/// `0..=1`, as the magic wand finds it.
pub struct Coverage<'a> {
    pub width: usize,
    pub height: usize,
    pub cells: &'a [f32],
}

/// Where coverage is traced: half of a cell covered is on the edge.
const HALF: f32 = 0.5;
/// The smallest speck or hole a traced coverage keeps, in square cells.
const SPECK: f64 = 1.5;

impl Coverage<'_> {
    fn at(&self, x: usize, y: usize) -> f32 {
        // Cells are read one cell in, so the grid has an empty border and
        // every edge closes.
        if x == 0 || y == 0 || x > self.width || y > self.height {
            return 0.0;
        }
        self.cells
            .get((y - 1) * self.width + (x - 1))
            .copied()
            .unwrap_or(0.0)
    }

    /// The area as a region in units: `to_units` maps a cell's corner to the
    /// artwork. The edge runs where coverage is half, followed between the
    /// cells' centres, so a partly covered cell puts it partway across.
    pub fn traced(&self, to_units: Affine) -> Region {
        let grid = self.width + 2;
        let inside = |x: usize, y: usize| self.at(x, y) >= HALF;
        // An edge between grid points: (y * grid + x) * 2, plus 1 when it
        // runs down rather than across.
        let across = |x: usize, y: usize| (y * grid + x) * 2;
        let down = |x: usize, y: usize| (y * grid + x) * 2 + 1;
        let point = |key: usize| {
            let (base, vertical) = (key / 2, key % 2 == 1);
            let (column, row) = (base % grid, base / grid);
            let (to_column, to_row) = if vertical {
                (column, row + 1)
            } else {
                (column + 1, row)
            };
            let (from, to) = (self.at(column, row), self.at(to_column, to_row));
            let along = if (to - from).abs() > f32::EPSILON {
                f64::from(((HALF - from) / (to - from)).clamp(0.0, 1.0))
            } else {
                0.5
            };
            // Grid point (column, row) is the centre of cell (column - 1, row - 1).
            let (x, y) = (f64_of_usize(column) - 0.5, f64_of_usize(row) - 0.5);
            if vertical {
                [x, y + along]
            } else {
                [x + along, y]
            }
        };
        let mut next: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
        let mut starts: Vec<usize> = Vec::new();
        for y in 0..=self.height {
            for x in 0..=self.width {
                // The square's corners and sides, clockwise from the top left.
                let corners = [(x, y), (x + 1, y), (x + 1, y + 1), (x, y + 1)];
                let sides = [across(x, y), down(x + 1, y), across(x, y + 1), down(x, y)];
                let ins = corners.map(|(x, y)| inside(x, y));
                if ins.iter().all(|inside| *inside) || ins.iter().all(|inside| !*inside) {
                    continue;
                }
                // Where the edge crosses the sides, and whether it enters the
                // area there, going clockwise.
                let crossings: Vec<(usize, bool)> = (0..4)
                    .filter_map(|side| {
                        let (from, to) = (ins.get(side)?, ins.get((side + 1) % 4)?);
                        (from != to).then_some((*sides.get(side)?, !*from))
                    })
                    .collect();
                let count = crossings.len();
                // Two inside corners across from each other join through the
                // middle when it is covered too.
                let joined = count == 4
                    && corners.iter().map(|(x, y)| self.at(*x, *y)).sum::<f32>() / 4.0 >= HALF;
                for (index, (side, enters)) in crossings.iter().enumerate() {
                    if !enters {
                        continue;
                    }
                    let partner = if joined { index + count - 1 } else { index + 1 } % count;
                    if let Some((leaves, _)) = crossings.get(partner) {
                        next.insert(*side, *leaves);
                        starts.push(*side);
                    }
                }
            }
        }
        let mut contours: Vec<Contour> = Vec::new();
        for start in starts {
            let mut contour = Vec::new();
            let mut key = start;
            while let Some(after) = next.remove(&key) {
                contour.push(point(key));
                key = after;
            }
            if contour.len() >= 3 && signed_area(&contour).abs() >= SPECK {
                contours.push(douglas_peucker(&softened(&contour), 0.2));
            }
        }
        Region::of_contours(contours).transformed(to_units)
    }
}

/// A traced outline with each corner drawn a little toward its neighbours,
/// so the grid it was traced on does not show.
fn softened(contour: &[[f64; 2]]) -> Contour {
    let count = contour.len();
    let point = |index: usize| contour.get(index % count).copied().unwrap_or_default();
    (0..count)
        .map(|index| {
            let (before, here, after) = (point(index + count - 1), point(index), point(index + 1));
            [
                0.5 * here[0] + 0.25 * (before[0] + after[0]),
                0.5 * here[1] + 0.25 * (before[1] + after[1]),
            ]
        })
        .collect()
}

impl Region {
    /// Which cells of a `width` by `height` grid have their centre inside:
    /// `to_cells` maps units to the grid, a cell one unit square.
    pub fn cells(&self, to_cells: Affine, width: usize, height: usize) -> Vec<bool> {
        let mut rows: Vec<Vec<(f64, i32)>> = vec![Vec::new(); height];
        for contour in self.shapes.iter().flatten() {
            let points: Vec<Point> = contour
                .iter()
                .map(|[x, y]| to_cells * Point::new(*x, *y))
                .collect();
            for (index, a) in points.iter().enumerate() {
                let Some(b) = points.get((index + 1) % points.len()) else {
                    continue;
                };
                if (a.y - b.y).abs() < 1e-12 {
                    continue;
                }
                let (low, high, winding) = if a.y < b.y { (a, b, 1) } else { (b, a, -1) };
                // The rows whose centre y + 0.5 lies in [low.y, high.y).
                let first = index_of((low.y - 0.5).ceil());
                let last = index_of((high.y - 0.5).ceil()).min(height);
                for row in first..last {
                    let y = f64_of_usize(row) + 0.5;
                    let x = low.x + (y - low.y) * (high.x - low.x) / (high.y - low.y);
                    if let Some(crossings) = rows.get_mut(row) {
                        crossings.push((x, winding));
                    }
                }
            }
        }
        let mut cells = vec![false; width * height];
        for (row, crossings) in rows.iter_mut().enumerate() {
            crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut winding = 0;
            for pair in crossings.windows(2) {
                let [(from, turn), (to, _)] = pair else {
                    continue;
                };
                winding += turn;
                if winding == 0 {
                    continue;
                }
                // The cells whose centre x + 0.5 lies in [from, to).
                let first = index_of((from - 0.5).ceil());
                let last = index_of((to - 0.5).ceil()).min(width);
                for column in first..last {
                    if let Some(slot) = cells.get_mut(row * width + column) {
                        *slot = true;
                    }
                }
            }
        }
        cells
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped at zero, and the callers bound it by the grid"
)]
fn index_of(value: f64) -> usize {
    if value.is_finite() {
        value.clamp(0.0, 1e9) as usize
    } else {
        0
    }
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
    fn coverage_traces_to_its_outline() {
        // A disc of radius 20 cells, its edge cells partly covered.
        let (width, height) = (60, 60);
        let cells: Vec<f32> = (0..width * height)
            .map(|index| {
                let (x, y) = (
                    f64_of_usize(index % width) + 0.5,
                    f64_of_usize(index / width) + 0.5,
                );
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "a coverage of 0 to 1 fits an f32"
                )]
                let cover = (20.5 - (x - 30.0).hypot(y - 30.0)).clamp(0.0, 1.0) as f32;
                cover
            })
            .collect();
        let coverage = Coverage {
            width,
            height,
            cells: &cells,
        };
        let region = coverage.traced(Affine::scale(2.0));
        let area = region.area();
        let disc = std::f64::consts::PI * 40.0 * 40.0;
        assert!((area - disc).abs() / disc < 0.01, "{area} against {disc}");
        assert!(region.corners() < 200, "{} corners", region.corners());
        assert!(region.contains(Point::new(60.0, 60.0)));
    }

    #[test]
    fn a_region_marks_the_cells_inside_it() {
        let ring = square(0.0, 0.0, 10.0).combine(&square(3.0, 3.0, 4.0), Op::Subtract);
        let cells = ring.cells(Affine::IDENTITY, 12, 12);
        assert!(cells[0] && cells[9 * 12 + 9]);
        assert!(!cells[5 * 12 + 5], "the hole");
        assert!(!cells[11], "outside");
        assert_eq!(cells.iter().filter(|cell| **cell).count(), 100 - 16);
    }
}
