//! Bezier outlines as a person edits them: anchors with the handles that
//! pull the curve in and out of each, laid down by the pen tool and dragged
//! when a path's points are edited.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo's float geometry, and anchor indices bounded by the anchors they index"
)]

use ui::vello::kurbo::{BezPath, CubicBez, ParamCurve as _, ParamCurveNearest as _, PathEl, Point};

use crate::model::Shape;

/// `v` scaled to length one, when it has a direction.
fn unit(v: ui::vello::kurbo::Vec2) -> Option<ui::vello::kurbo::Vec2> {
    let length = v.hypot();
    (length > 1e-12).then(|| v / length)
}

/// A point the outline passes through, with the handles of the curves on
/// either side. A handle that is absent makes that side a straight line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    pub at: Point,
    /// The control point of the curve arriving here.
    pub before: Option<Point>,
    /// The control point of the curve leaving here.
    pub after: Option<Point>,
}

impl Anchor {
    pub fn corner(at: Point) -> Self {
        Self {
            at,
            before: None,
            after: None,
        }
    }

    /// Whether the two handles pull along one line, so moving one turns the
    /// other with it.
    pub fn smooth(&self) -> bool {
        match (self.before, self.after) {
            (Some(before), Some(after)) => {
                let (a, b) = (before - self.at, after - self.at);
                a.hypot() > 1e-6
                    && b.hypot() > 1e-6
                    && a.cross(b).abs() <= 1e-3 * a.hypot() * b.hypot()
                    && a.dot(b) < 0.0
            }
            _ => false,
        }
    }
}

/// One connected run of anchors.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Contour {
    pub anchors: Vec<Anchor>,
    pub closed: bool,
}

/// Which part of an anchor a point is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Anchor,
    Before,
    After,
}

/// A point of an outline: an anchor or one of its handles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointRef {
    pub contour: usize,
    pub anchor: usize,
    pub part: Part,
}

/// A whole outline: its contours.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outline {
    pub contours: Vec<Contour>,
}

fn push_segment(path: &mut BezPath, from: &Anchor, to: &Anchor) {
    if from.after.is_none() && to.before.is_none() {
        path.line_to(to.at);
    } else {
        path.curve_to(
            from.after.unwrap_or(from.at),
            to.before.unwrap_or(to.at),
            to.at,
        );
    }
}

impl Contour {
    pub fn to_path(&self, path: &mut BezPath) {
        let Some(first) = self.anchors.first() else {
            return;
        };
        path.move_to(first.at);
        for pair in self.anchors.windows(2) {
            if let [from, to] = pair {
                push_segment(path, from, to);
            }
        }
        if self.closed {
            if let Some(last) = self.anchors.last()
                && (last.after.is_some() || first.before.is_some())
            {
                push_segment(path, last, first);
            }
            path.close_path();
        }
    }

    /// The segments as cubics: `(index of the anchor it leaves, curve)`.
    fn segments(&self) -> Vec<(usize, CubicBez)> {
        let count = self.anchors.len();
        let ends = if self.closed {
            count
        } else {
            count.saturating_sub(1)
        };
        (0..ends)
            .filter_map(|index| {
                let from = self.anchors.get(index)?;
                let to = self.anchors.get((index + 1) % count)?;
                Some((
                    index,
                    CubicBez::new(
                        from.at,
                        from.after.unwrap_or(from.at),
                        to.before.unwrap_or(to.at),
                        to.at,
                    ),
                ))
            })
            .collect()
    }
}

impl Outline {
    /// The outline of path data: quadratic curves become cubic ones.
    pub fn from_path(path: &BezPath) -> Self {
        let mut contours: Vec<Contour> = Vec::new();
        let mut last = Point::ZERO;
        for element in path.elements() {
            match *element {
                PathEl::MoveTo(at) => {
                    contours.push(Contour {
                        anchors: vec![Anchor::corner(at)],
                        closed: false,
                    });
                    last = at;
                }
                PathEl::LineTo(at) => {
                    Self::open(&mut contours, last)
                        .anchors
                        .push(Anchor::corner(at));
                    last = at;
                }
                PathEl::QuadTo(control, at) => {
                    let after = last + (control - last) * (2.0 / 3.0);
                    let before = at + (control - at) * (2.0 / 3.0);
                    Self::curve(&mut contours, last, (after, before, at));
                    last = at;
                }
                PathEl::CurveTo(after, before, at) => {
                    Self::curve(&mut contours, last, (after, before, at));
                    last = at;
                }
                PathEl::ClosePath => {
                    if let Some(contour) = contours.last_mut() {
                        contour.closed = true;
                        // A last anchor on the first is the same point.
                        if contour.anchors.len() > 1
                            && let (Some(first), Some(end)) =
                                (contour.anchors.first(), contour.anchors.last())
                            && first.at.distance(end.at) < 1e-6
                        {
                            let before = end.before;
                            contour.anchors.pop();
                            if let Some(first) = contour.anchors.first_mut() {
                                first.before = before;
                            }
                        }
                        last = contour.anchors.first().map_or(last, |first| first.at);
                    }
                }
            }
        }
        Self { contours }
    }

    fn open(contours: &mut Vec<Contour>, at: Point) -> &mut Contour {
        if contours.last().is_none_or(|contour| contour.closed) {
            contours.push(Contour {
                anchors: vec![Anchor::corner(at)],
                closed: false,
            });
        }
        #[allow(
            clippy::expect_used,
            reason = "a contour was pushed just above when there was none"
        )]
        contours.last_mut().expect("an open contour")
    }

    fn curve(contours: &mut Vec<Contour>, last: Point, (after, before, at): (Point, Point, Point)) {
        let contour = Self::open(contours, last);
        if let Some(from) = contour.anchors.last_mut() {
            from.after = Some(after);
        }
        contour.anchors.push(Anchor {
            at,
            before: Some(before),
            after: None,
        });
    }

    /// The outline a shape's points can be edited as: paths, polygons and
    /// lines.
    pub fn of_shape(shape: &Shape) -> Option<Self> {
        match shape {
            Shape::Path { .. } | Shape::Polygon { .. } | Shape::Line { .. } => {
                Some(Self::from_path(&shape.path()))
            }
            _ => None,
        }
    }

    /// The outline as a shape of the kind `like` is: a polygon or line stays
    /// one while its sides are straight.
    pub fn to_shape(&self, like: &Shape) -> Shape {
        let straight = self.contours.len() == 1
            && self.contours.iter().all(|contour| {
                contour
                    .anchors
                    .iter()
                    .all(|anchor| anchor.before.is_none() && anchor.after.is_none())
            });
        let points = || -> Vec<[f64; 2]> {
            self.contours
                .iter()
                .flat_map(|contour| {
                    contour
                        .anchors
                        .iter()
                        .map(|anchor| [anchor.at.x, anchor.at.y])
                })
                .collect()
        };
        match like {
            Shape::Polygon { .. } if straight && self.contours.iter().all(|c| c.closed) => {
                Shape::Polygon { points: points() }
            }
            Shape::Line { .. } if straight && self.contours.iter().all(|c| !c.closed) => {
                Shape::Line { points: points() }
            }
            _ => Shape::Path {
                d: crate::model::path_data(&self.to_path()),
            },
        }
    }

    pub fn to_path(&self) -> BezPath {
        let mut path = BezPath::new();
        for contour in &self.contours {
            contour.to_path(&mut path);
        }
        path
    }

    fn anchor(&self, contour: usize, anchor: usize) -> Option<&Anchor> {
        self.contours.get(contour)?.anchors.get(anchor)
    }

    fn anchor_mut(&mut self, contour: usize, anchor: usize) -> Option<&mut Anchor> {
        self.contours.get_mut(contour)?.anchors.get_mut(anchor)
    }

    /// Where `point` is.
    pub fn get(&self, point: PointRef) -> Option<Point> {
        let anchor = self.anchor(point.contour, point.anchor)?;
        match point.part {
            Part::Anchor => Some(anchor.at),
            Part::Before => anchor.before,
            Part::After => anchor.after,
        }
    }

    /// Every anchor, in order.
    pub fn anchors(&self) -> impl Iterator<Item = (PointRef, &Anchor)> {
        self.contours.iter().enumerate().flat_map(|(contour, c)| {
            c.anchors.iter().enumerate().map(move |(anchor, a)| {
                (
                    PointRef {
                        contour,
                        anchor,
                        part: Part::Anchor,
                    },
                    a,
                )
            })
        })
    }

    /// The point within `reach` of `at`, nearest first; the handles of
    /// `active` (the anchor being worked on) count, others' do not.
    pub fn hit(&self, at: Point, reach: f64, active: Option<PointRef>) -> Option<PointRef> {
        let mut best: Option<(f64, PointRef)> = None;
        let mut consider = |point: PointRef, position: Option<Point>| {
            if let Some(position) = position {
                let distance = position.distance(at);
                if distance <= reach && best.is_none_or(|(nearest, _)| distance < nearest) {
                    best = Some((distance, point));
                }
            }
        };
        if let Some(active) = active
            && let Some(anchor) = self.anchor(active.contour, active.anchor)
        {
            for (part, position) in [(Part::Before, anchor.before), (Part::After, anchor.after)] {
                consider(PointRef { part, ..active }, position);
            }
        }
        for (point, anchor) in self.anchors() {
            consider(point, Some(anchor.at));
        }
        best.map(|(_, point)| point)
    }

    /// Put `point` at `to`. An anchor carries its handles along; a handle of
    /// a smooth anchor turns the other handle with it, keeping its length.
    pub fn set(&mut self, point: PointRef, to: Point) {
        let Some(anchor) = self.anchor_mut(point.contour, point.anchor) else {
            return;
        };
        match point.part {
            Part::Anchor => {
                let shift = to - anchor.at;
                anchor.at = to;
                anchor.before = anchor.before.map(|handle| handle + shift);
                anchor.after = anchor.after.map(|handle| handle + shift);
            }
            Part::Before | Part::After => {
                let smooth = anchor.smooth();
                let centre = anchor.at;
                let (moved, other) = if point.part == Part::Before {
                    (&mut anchor.before, &mut anchor.after)
                } else {
                    (&mut anchor.after, &mut anchor.before)
                };
                *moved = Some(to);
                if smooth && let Some(opposite) = other {
                    let length = (*opposite - centre).hypot();
                    if let Some(direction) = unit(centre - to) {
                        *other = Some(centre + direction * length);
                    }
                }
            }
        }
    }

    /// Take `point` out: an anchor leaves the outline (a contour too small
    /// to keep goes with it); a handle straightens its side.
    pub fn remove(&mut self, point: PointRef) {
        match point.part {
            Part::Before | Part::After => {
                if let Some(anchor) = self.anchor_mut(point.contour, point.anchor) {
                    if point.part == Part::Before {
                        anchor.before = None;
                    } else {
                        anchor.after = None;
                    }
                }
            }
            Part::Anchor => {
                if let Some(contour) = self.contours.get_mut(point.contour) {
                    if point.anchor < contour.anchors.len() {
                        contour.anchors.remove(point.anchor);
                    }
                    if contour.anchors.len() < 2 {
                        self.contours.remove(point.contour);
                    }
                }
            }
        }
    }

    /// Whether the outline still draws something.
    pub fn is_empty(&self) -> bool {
        self.contours.is_empty()
    }

    /// Add an anchor on the outline nearest `at`, within `reach`, splitting
    /// that segment so the curve keeps its shape.
    pub fn insert_near(&mut self, at: Point, reach: f64) -> Option<PointRef> {
        let mut best: Option<(f64, usize, usize, f64)> = None;
        for (index, contour) in self.contours.iter().enumerate() {
            for (from, curve) in contour.segments() {
                let nearest = curve.nearest(at, 1e-6);
                if nearest.distance_sq <= reach * reach
                    && best.is_none_or(|(distance, ..)| nearest.distance_sq < distance)
                {
                    best = Some((nearest.distance_sq, index, from, nearest.t));
                }
            }
        }
        let (_, index, from, t) = best?;
        let contour = self.contours.get_mut(index)?;
        let count = contour.anchors.len();
        let to = (from + 1) % count;
        let (start, end) = (*contour.anchors.get(from)?, *contour.anchors.get(to)?);
        let straight = start.after.is_none() && end.before.is_none();
        let curve = CubicBez::new(
            start.at,
            start.after.unwrap_or(start.at),
            end.before.unwrap_or(end.at),
            end.at,
        );
        let middle = if straight {
            Anchor::corner(curve.eval(t))
        } else {
            let (left, right) = (curve.subsegment(0.0..t), curve.subsegment(t..1.0));
            if let Some(start) = contour.anchors.get_mut(from) {
                start.after = Some(left.p1);
            }
            if let Some(end) = contour.anchors.get_mut(to) {
                end.before = Some(right.p2);
            }
            Anchor {
                at: left.p3,
                before: Some(left.p2),
                after: Some(right.p1),
            }
        };
        let place = from + 1;
        contour.anchors.insert(place, middle);
        Some(PointRef {
            contour: index,
            anchor: place,
            part: Part::Anchor,
        })
    }

    /// Turn an anchor between a corner and a smooth point: a smooth point
    /// gets handles along its neighbours' direction, a third of the way to
    /// each.
    pub fn toggle_smooth(&mut self, contour: usize, anchor: usize) {
        let Some(c) = self.contours.get(contour) else {
            return;
        };
        let count = c.anchors.len();
        let Some(here) = c.anchors.get(anchor).copied() else {
            return;
        };
        if here.before.is_some() || here.after.is_some() {
            if let Some(point) = self.anchor_mut(contour, anchor) {
                point.before = None;
                point.after = None;
            }
            return;
        }
        let neighbour = |offset: isize| -> Option<Point> {
            let index = anchor
                .checked_add_signed(offset)
                .filter(|index| *index < count)
                .or_else(|| {
                    c.closed
                        .then(|| anchor.wrapping_add_signed(offset).wrapping_add(count) % count)
                })?;
            c.anchors.get(index).map(|neighbour| neighbour.at)
        };
        let (previous, next) = (neighbour(-1), neighbour(1));
        let direction = match (previous, next) {
            (Some(previous), Some(next)) => unit(next - previous),
            (Some(previous), None) => unit(here.at - previous),
            (None, Some(next)) => unit(next - here.at),
            (None, None) => None,
        };
        let Some(direction) = direction else {
            return;
        };
        let reach =
            |other: Option<Point>| other.map_or(20.0, |other| other.distance(here.at) / 3.0);
        if let Some(point) = self.anchor_mut(contour, anchor) {
            point.before = Some(here.at - direction * reach(previous));
            point.after = Some(here.at + direction * reach(next));
        }
    }
}

/// A path being laid down with the pen: a click places a corner, a drag
/// pulls a smooth point's handles, a click on the first anchor closes it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PenDraft {
    pub contour: Contour,
    /// The pointer is down on the newest anchor, pulling its handles.
    pub pulling: bool,
}

/// What a press of the pen did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PenStep {
    /// Placed an anchor; the path goes on.
    Placed,
    /// Closed the path on its first anchor; it is done once let go.
    Closed,
}

impl PenDraft {
    /// A press at `at`; `reach` is how near the first anchor closes the
    /// path.
    pub fn press(&mut self, at: Point, reach: f64) -> PenStep {
        let anchors = &self.contour.anchors;
        if anchors.len() >= 2
            && anchors
                .first()
                .is_some_and(|first| first.at.distance(at) <= reach)
        {
            self.contour.closed = true;
            self.pulling = true;
            return PenStep::Closed;
        }
        self.contour.anchors.push(Anchor::corner(at));
        self.pulling = true;
        PenStep::Placed
    }

    /// The pointer dragged to `to` while down: pull the handles of the
    /// anchor just placed (or the first, when closing) symmetrically.
    pub fn pull(&mut self, to: Point) {
        if !self.pulling {
            return;
        }
        let closing = self.contour.closed;
        let anchor = if closing {
            self.contour.anchors.first_mut()
        } else {
            self.contour.anchors.last_mut()
        };
        let Some(anchor) = anchor else {
            return;
        };
        if to.distance(anchor.at) < 1e-3 {
            anchor.before = None;
            anchor.after = None;
            return;
        }
        let mirrored = anchor.at + (anchor.at - to);
        anchor.after = Some(to);
        anchor.before = Some(mirrored);
    }

    pub fn release(&mut self) {
        self.pulling = false;
    }

    /// Take back the newest anchor; `false` when there is none left.
    pub fn undo_anchor(&mut self) -> bool {
        self.contour.closed = false;
        self.contour.anchors.pop();
        !self.contour.anchors.is_empty()
    }

    /// The path so far, with a rubber band to `cursor` when one is given.
    pub fn preview(&self, cursor: Option<Point>) -> BezPath {
        let mut path = BezPath::new();
        self.contour.to_path(&mut path);
        if let (Some(cursor), Some(last), false) = (
            cursor,
            self.contour.anchors.last(),
            self.contour.closed || self.pulling,
        ) {
            match last.after {
                Some(after) => path.curve_to(after, cursor, cursor),
                None => path.line_to(cursor),
            }
        }
        path
    }

    /// The finished shape, when it has a segment to draw.
    pub fn shape(&self) -> Option<Shape> {
        (self.contour.anchors.len() >= 2).then(|| Shape::Path {
            d: crate::model::path_data(&self.preview(None)),
        })
    }
}

/// `to` moved so the line from `from` runs at a multiple of 45 degrees.
pub fn constrain_45(from: Point, to: Point) -> Point {
    let delta = to - from;
    let length = delta.hypot();
    if length < 1e-9 {
        return to;
    }
    let step = std::f64::consts::FRAC_PI_4;
    let angle = (delta.y.atan2(delta.x) / step).round() * step;
    // Keep the projection's length along the snapped direction.
    let direction = ui::vello::kurbo::Vec2::new(angle.cos(), angle.sin());
    from + direction * delta.dot(direction)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::vello::kurbo::Shape as _;

    #[test]
    fn path_data_round_trips_through_anchors() {
        let path =
            BezPath::from_svg("M10,10 C20,0 40,0 50,10 L50,50 Z M100,100 Q120,80 140,100").unwrap();
        let outline = Outline::from_path(&path);
        assert_eq!(outline.contours.len(), 2);
        let first = &outline.contours[0];
        assert!(first.closed);
        assert_eq!(first.anchors.len(), 3);
        assert_eq!(first.anchors[0].after, Some(Point::new(20.0, 0.0)));
        assert_eq!(first.anchors[1].before, Some(Point::new(40.0, 0.0)));
        // Redrawn, it covers the same ground.
        let again = outline.to_path();
        assert!((again.bounding_box().area() - path.bounding_box().area()).abs() < 1.0);
    }

    #[test]
    fn a_closing_curve_onto_the_first_anchor_merges_with_it() {
        let path = BezPath::from_svg("M0,0 L10,0 C10,10 0,10 0,0 Z").unwrap();
        let outline = Outline::from_path(&path);
        assert_eq!(outline.contours[0].anchors.len(), 2);
        assert_eq!(
            outline.contours[0].anchors[0].before,
            Some(Point::new(0.0, 10.0))
        );
    }

    #[test]
    fn moving_a_smooth_handle_turns_its_twin() {
        let mut outline = Outline::from_path(
            &BezPath::from_svg("M0,0 C0,-10 40,-10 50,0 C60,10 100,10 100,0").unwrap(),
        );
        let middle = PointRef {
            contour: 0,
            anchor: 1,
            part: Part::After,
        };
        assert!(outline.contours[0].anchors[1].smooth());
        outline.set(middle, Point::new(50.0, 20.0));
        let anchor = outline.contours[0].anchors[1];
        let before = anchor.before.unwrap();
        assert!(
            (before.x - 50.0).abs() < 1e-9 && before.y < 0.0,
            "{before:?}"
        );
        // Moving the anchor carries both handles.
        outline.set(
            PointRef {
                part: Part::Anchor,
                ..middle
            },
            Point::new(60.0, 0.0),
        );
        assert_eq!(
            outline.contours[0].anchors[1].after,
            Some(Point::new(60.0, 20.0))
        );
    }

    #[test]
    fn inserting_on_a_curve_keeps_its_shape() {
        let path = BezPath::from_svg("M0,0 C0,-40 100,-40 100,0").unwrap();
        let mut outline = Outline::from_path(&path);
        let before =
            CubicBez::new((0.0, 0.0), (0.0, -40.0), (100.0, -40.0), (100.0, 0.0)).eval(0.5);
        let inserted = outline.insert_near(before, 2.0).unwrap();
        assert_eq!(inserted.anchor, 1);
        let at = outline.get(inserted).unwrap();
        assert!(at.distance(before) < 1.0, "{at:?} {before:?}");
        let area = |path: &BezPath| path.bounding_box().area();
        assert!((area(&outline.to_path()) - area(&path)).abs() < 1.0);
    }

    #[test]
    fn the_pen_places_corners_pulls_curves_and_closes() {
        let mut pen = PenDraft::default();
        assert_eq!(pen.press(Point::new(0.0, 0.0), 5.0), PenStep::Placed);
        pen.release();
        assert_eq!(pen.press(Point::new(100.0, 0.0), 5.0), PenStep::Placed);
        pen.pull(Point::new(120.0, 20.0));
        pen.release();
        let smooth = pen.contour.anchors[1];
        assert_eq!(smooth.after, Some(Point::new(120.0, 20.0)));
        assert_eq!(smooth.before, Some(Point::new(80.0, -20.0)));
        pen.press(Point::new(50.0, 80.0), 5.0);
        pen.release();
        assert_eq!(pen.press(Point::new(2.0, 1.0), 5.0), PenStep::Closed);
        pen.release();
        let Some(Shape::Path { d }) = pen.shape() else {
            panic!("a path")
        };
        assert!(d.ends_with('Z'), "{d}");
        assert!(d.contains('C'), "{d}");
    }

    #[test]
    fn polygons_stay_polygons_while_straight() {
        let shape = Shape::Polygon {
            points: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]],
        };
        let mut outline = Outline::of_shape(&shape).unwrap();
        outline.set(
            PointRef {
                contour: 0,
                anchor: 1,
                part: Part::Anchor,
            },
            Point::new(20.0, 0.0),
        );
        assert_eq!(
            outline.to_shape(&shape),
            Shape::Polygon {
                points: vec![[0.0, 0.0], [20.0, 0.0], [10.0, 10.0]]
            }
        );
        outline.toggle_smooth(0, 1);
        assert_eq!(outline.to_shape(&shape).kind(), "path");
    }

    #[test]
    fn lines_snap_to_eighths_of_a_turn() {
        let snapped = constrain_45(Point::ZERO, Point::new(100.0, 8.0));
        assert!((snapped.y).abs() < 1e-9 && (snapped.x - 100.0).abs() < 1e-9);
        let diagonal = constrain_45(Point::ZERO, Point::new(100.0, 90.0));
        assert!((diagonal.x - diagonal.y).abs() < 1e-9);
    }
}
