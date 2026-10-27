//! A mark's points as a person edits them: a path's bezier outline, or a
//! brush stroke's path. A hand-laid stroke has a point every few pixels,
//! too many to drag one by one, so its editor shows the few that shape it
//! and bends the stretch around the one being dragged, the pressure of
//! every point kept.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "float geometry, and point indices bounded by the stroke they index"
)]

use ui::vello::kurbo::{BezPath, Point};

use super::outline::{Anchor, Outline, Part, PointRef};
use crate::model::{Mark, Shape, StrokePoint};

/// The most points a stroke's editor shows.
const MOST_SHOWN: usize = 40;

/// A mark's editable points.
#[derive(Clone, Debug, PartialEq)]
pub enum Editable {
    Outline(Outline),
    Stroke(StrokeEdit),
}

impl Editable {
    /// The points of a path, polygon, line or brush stroke; a stroke's
    /// path is shown as its brush follows it, after the mark's smoothing.
    pub fn of_mark(mark: &Mark) -> Option<Self> {
        match &mark.shape {
            Shape::Stroke { points, size, .. } => Some(Self::Stroke(StrokeEdit::new(
                points.clone(),
                *size,
                crate::brushes::Dynamics::of_mark(mark).smoothing,
            ))),
            shape => Outline::of_shape(shape).map(Self::Outline),
        }
    }

    pub fn to_shape(&self, like: &Shape) -> Shape {
        match self {
            Self::Outline(outline) => outline.to_shape(like),
            Self::Stroke(stroke) => stroke.to_shape(like),
        }
    }

    pub fn to_path(&self) -> BezPath {
        match self {
            Self::Outline(outline) => outline.to_path(),
            Self::Stroke(stroke) => stroke.to_path(),
        }
    }

    pub fn get(&self, point: PointRef) -> Option<Point> {
        match self {
            Self::Outline(outline) => outline.get(point),
            Self::Stroke(stroke) => stroke.anchors.get(point.anchor).map(|anchor| anchor.at),
        }
    }

    /// Every anchor shown, in order.
    pub fn anchors(&self) -> Vec<(PointRef, Anchor)> {
        match self {
            Self::Outline(outline) => outline
                .anchors()
                .map(|(point, anchor)| (point, *anchor))
                .collect(),
            Self::Stroke(stroke) => stroke
                .anchors
                .iter()
                .enumerate()
                .map(|(anchor, at)| (StrokeEdit::point(anchor), *at))
                .collect(),
        }
    }

    pub fn hit(&self, at: Point, reach: f64, active: Option<PointRef>) -> Option<PointRef> {
        match self {
            Self::Outline(outline) => outline.hit(at, reach, active),
            Self::Stroke(stroke) => stroke.hit(at, reach),
        }
    }

    pub fn set(&mut self, point: PointRef, to: Point) {
        match self {
            Self::Outline(outline) => outline.set(point, to),
            Self::Stroke(stroke) => stroke.set(point.anchor, to),
        }
    }

    pub fn remove(&mut self, point: PointRef) {
        match self {
            Self::Outline(outline) => outline.remove(point),
            Self::Stroke(stroke) => stroke.remove(point.anchor),
        }
    }

    pub fn insert_near(&mut self, at: Point, reach: f64) -> Option<PointRef> {
        match self {
            Self::Outline(outline) => outline.insert_near(at, reach),
            Self::Stroke(stroke) => stroke.insert_near(at, reach),
        }
    }

    /// Turn an anchor between a corner and a smooth point; a stroke's
    /// points have no handles to turn.
    pub fn toggle_smooth(&mut self, contour: usize, anchor: usize) {
        if let Self::Outline(outline) = self {
            outline.toggle_smooth(contour, anchor);
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::Outline(outline) => outline.is_empty(),
            Self::Stroke(stroke) => stroke.points.is_empty(),
        }
    }

    /// Which point of a stroke an anchor is, to cut the stroke there.
    pub fn stroke_point(&self, point: PointRef) -> Option<Point> {
        match self {
            Self::Stroke(stroke) => stroke.anchors.get(point.anchor).map(|anchor| anchor.at),
            Self::Outline(_) => None,
        }
    }
}

/// A brush stroke's path being edited.
#[derive(Clone, Debug, PartialEq)]
pub struct StrokeEdit {
    points: Vec<StrokePoint>,
    /// The indices of the points shown, first to last.
    keys: Vec<usize>,
    anchors: Vec<Anchor>,
    /// The brush's size, and how much it smooths the path it follows.
    size: f64,
    smoothing: f64,
}

fn at(point: &[f64]) -> Point {
    Point::new(
        point.first().copied().unwrap_or(0.0),
        point.get(1).copied().unwrap_or(0.0),
    )
}

impl StrokeEdit {
    /// The stroke's points, showing those that shape it: where it turns,
    /// its ends, and no more than `MOST_SHOWN`.
    pub fn new(points: Vec<StrokePoint>, size: f64, smoothing: f64) -> Self {
        let mut tolerance = (size * 0.25).max(1.0);
        let mut keys = crate::brushes::kept(&points, tolerance);
        while keys.len() > MOST_SHOWN && tolerance < crate::model::MAX_SIDE {
            tolerance *= 1.6;
            keys = crate::brushes::kept(&points, tolerance);
        }
        let mut edit = Self {
            points,
            keys,
            anchors: Vec::new(),
            size,
            smoothing,
        };
        edit.refresh();
        edit
    }

    fn point(anchor: usize) -> PointRef {
        PointRef {
            contour: 0,
            anchor,
            part: Part::Anchor,
        }
    }

    fn refresh(&mut self) {
        self.anchors = self
            .keys
            .iter()
            .filter_map(|index| self.points.get(*index))
            .map(|point| Anchor::corner(at(point)))
            .collect();
    }

    fn to_shape(&self, like: &Shape) -> Shape {
        let mut shape = like.clone();
        if let Shape::Stroke { points, .. } = &mut shape {
            points.clone_from(&self.points);
        }
        shape
    }

    /// The curve the brush lays the stroke along, not the polyline
    /// through its points.
    fn to_path(&self) -> BezPath {
        crate::brushes::centreline(&self.points, self.size, self.smoothing)
    }

    fn hit(&self, to: Point, reach: f64) -> Option<PointRef> {
        self.anchors
            .iter()
            .enumerate()
            .map(|(index, anchor)| (anchor.at.distance(to), index))
            .filter(|(distance, _)| *distance <= reach)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, index)| Self::point(index))
    }

    /// Drag shown point `anchor` to `to`: the points between its shown
    /// neighbours follow, less the further they are from it, so the stroke
    /// bends rather than kinks.
    fn set(&mut self, anchor: usize, to: Point) {
        let Some(&key) = self.keys.get(anchor) else {
            return;
        };
        let Some(now) = self.points.get(key).map(|point| at(point)) else {
            return;
        };
        let shift = to - now;
        let before = anchor
            .checked_sub(1)
            .and_then(|k| self.keys.get(k))
            .copied();
        let after = self.keys.get(anchor + 1).copied();
        let last = self.points.len().saturating_sub(1);
        // An end's stretch runs to the stroke's end; a middle point's to
        // its neighbours, which stay where they are.
        let from = before.map_or(0, |before| before + 1);
        let until = after.map_or(last, |after| after.saturating_sub(1));
        for index in from..=until {
            let weight = match index.cmp(&key) {
                std::cmp::Ordering::Equal => 1.0,
                std::cmp::Ordering::Less => {
                    let start = before.unwrap_or(0);
                    falloff(index - start, key - start, before.is_none())
                }
                std::cmp::Ordering::Greater => {
                    let end = after.unwrap_or(last);
                    falloff(end - index, end - key, after.is_none())
                }
            };
            if let Some(point) = self.points.get_mut(index)
                && let [x, y, ..] = point.as_mut_slice()
            {
                *x += shift.x * weight;
                *y += shift.y * weight;
            }
        }
        self.refresh();
    }

    /// Take shown point `anchor` out: the stroke runs straight between its
    /// neighbours, or ends at the neighbour when it was an end.
    fn remove(&mut self, anchor: usize) {
        let Some(&key) = self.keys.get(anchor) else {
            return;
        };
        if self.keys.len() <= 2 {
            self.points.clear();
            self.keys.clear();
            self.refresh();
            return;
        }
        let before = anchor
            .checked_sub(1)
            .and_then(|k| self.keys.get(k))
            .copied();
        let after = self.keys.get(anchor + 1).copied();
        let (start, end) = match (before, after) {
            (Some(before), Some(after)) => (before + 1, after),
            (None, Some(after)) => (0, after),
            (Some(before), None) => (before + 1, self.points.len()),
            (None, None) => return,
        };
        let removed = end.saturating_sub(start);
        if start < end && end <= self.points.len() {
            self.points.drain(start..end);
        }
        self.keys.remove(anchor);
        for later in self.keys.iter_mut().filter(|index| **index > key) {
            *later = later.saturating_sub(removed);
        }
        self.refresh();
    }

    /// Show the stroke's point nearest `to`, within `reach`, to drag it.
    fn insert_near(&mut self, to: Point, reach: f64) -> Option<PointRef> {
        let (distance, index) = self
            .points
            .iter()
            .enumerate()
            .map(|(index, point)| (at(point).distance(to), index))
            .min_by(|a, b| a.0.total_cmp(&b.0))?;
        if distance > reach || self.keys.contains(&index) {
            return None;
        }
        let place = self.keys.partition_point(|key| *key < index);
        self.keys.insert(place, index);
        self.refresh();
        Some(Self::point(place))
    }
}

/// How much a point `steps` from the far end of a stretch `span` long
/// follows the dragged point at its near end: smoothly from none to all.
/// A stretch running off the stroke's end moves whole.
fn falloff(steps: usize, span: usize, open: bool) -> f64 {
    if open || span == 0 {
        return 1.0;
    }
    let t = f64::from(u32::try_from(steps).unwrap_or(u32::MAX))
        / f64::from(u32::try_from(span).unwrap_or(u32::MAX));
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke() -> Vec<StrokePoint> {
        (0..=100)
            .map(|step| vec![f64::from(step), 0.0, 0.5])
            .collect()
    }

    #[test]
    fn a_dense_stroke_shows_a_few_points_and_bends_when_one_is_dragged() {
        let mut edit = StrokeEdit::new(stroke(), 4.0, 0.0);
        assert_eq!(edit.anchors.len(), 2, "a straight stroke shows its ends");
        let middle = edit.insert_near(Point::new(50.0, 1.0), 3.0).unwrap();
        assert_eq!(edit.anchors.len(), 3);
        edit.set(middle.anchor, Point::new(50.0, 20.0));
        let y = |index: usize| edit.points[index][1];
        assert!((y(50) - 20.0).abs() < 1e-9);
        assert!(y(25) > 1.0 && y(25) < 19.0, "{}", y(25));
        assert!(y(0).abs() < 1e-9 && y(100).abs() < 1e-9);
        assert!(
            edit.points
                .iter()
                .all(|point| (point[2] - 0.5).abs() < 1e-12)
        );
        edit.remove(middle.anchor);
        assert_eq!(edit.anchors.len(), 2);
        assert_eq!(edit.points.len(), 2);
    }
}
