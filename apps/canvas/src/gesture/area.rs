//! Selecting areas with the pointer: a rectangle or ellipse dragged out, a
//! lasso dragged freehand or clicked out point by point, the magic wand and
//! the fill bucket. Each finished gesture is one `select` (or, for the
//! bucket, one `draw`) whose area is plain geometry.
//!
//! Held keys say how a new area meets the selection there is: Shift adds,
//! Alt subtracts, both intersect.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo's geometry operators (Affine, Point, Vec2) are float geometry, not overflowing integer arithmetic"
)]

use ui::tilo_resource::Invoke;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, BezPath, Point, Rect};

use crate::actions::{Draw, NewMark};
use crate::model::{Artwork, Shape, path_data};
use crate::region::{Op, Region};
use crate::selection::Select;

use super::Ink;

/// How a new area meets the selection there is, from the held keys.
pub fn mode(shift: bool, alt: bool) -> Option<String> {
    match (shift, alt) {
        (true, true) => Some("intersect".into()),
        (true, false) => Some("add".into()),
        (false, true) => Some("subtract".into()),
        (false, false) => None,
    }
}

/// An area being dragged out.
#[derive(Clone, Debug, PartialEq)]
pub enum AreaDrag {
    /// A rectangle, or the ellipse in it, from `start` to `now`.
    Box {
        start: Point,
        now: Point,
        oval: bool,
    },
    /// A lasso dragged freehand.
    Lasso { points: Vec<Point> },
}

/// The area tools' state between events.
#[derive(Clone, Debug, Default)]
pub struct AreaGesture {
    pub drag: Option<AreaDrag>,
    /// A lasso being clicked out, point by point.
    pub polygon: Vec<Point>,
}

impl AreaGesture {
    /// Whether a gesture is under way.
    pub fn active(&self) -> bool {
        self.drag.is_some() || !self.polygon.is_empty()
    }

    /// A press of a box tool (`oval` for the ellipse) or the lasso.
    pub fn press(&mut self, at: Point, oval: Option<bool>) {
        self.drag = Some(match oval {
            Some(oval) => AreaDrag::Box {
                start: at,
                now: at,
                oval,
            },
            None => AreaDrag::Lasso { points: vec![at] },
        });
    }

    /// The pointer moved to `at`; a lasso keeps points `step` units apart.
    pub fn drag_to(&mut self, at: Point, step: f64) {
        match &mut self.drag {
            Some(AreaDrag::Box { now, .. }) => *now = at,
            Some(AreaDrag::Lasso { points })
                if points.last().is_none_or(|last| last.distance(at) >= step) =>
            {
                points.push(at);
            }
            _ => {}
        }
    }

    /// The pointer let go. A drag shorter than `slop` units was a click: a
    /// box tool's click selects nothing, a lasso's adds a point to the one
    /// being clicked out, which ends on its first point. Answers the
    /// selection made, if any.
    pub fn release(&mut self, slop: f64, mode: Option<String>) -> Option<Invoke<Artwork>> {
        match self.drag.take()? {
            AreaDrag::Box { start, now, oval } => {
                let area = Rect::from_points(start, now);
                if area.width().max(area.height()) < slop {
                    // A click outside any area drops the selection.
                    return mode.is_none().then(none);
                }
                let bounds = [area.x0, area.y0, area.width(), area.height()];
                Some(select(
                    Select {
                        rect: (!oval).then_some(bounds),
                        ellipse: oval.then_some(bounds),
                        ..Select::default()
                    },
                    mode,
                ))
            }
            AreaDrag::Lasso { points } => {
                let start = points.first().copied()?;
                let reach = points
                    .iter()
                    .map(|point| point.distance(start))
                    .fold(0.0, f64::max);
                if reach >= slop && self.polygon.is_empty() {
                    return lasso(&points, mode);
                }
                // A click: a corner of the lasso being clicked out, which
                // ends where it began or on a double click.
                let closes = self
                    .polygon
                    .first()
                    .is_some_and(|first| first.distance(start) < slop * 2.0);
                if !closes {
                    self.polygon.push(start);
                }
                if closes && self.polygon.len() >= 3 {
                    let polygon = std::mem::take(&mut self.polygon);
                    return lasso(&polygon, mode);
                }
                None
            }
        }
    }

    /// End the lasso being clicked out (Enter): its selection, if it has
    /// enough corners.
    pub fn finish(&mut self, mode: Option<String>) -> Option<Invoke<Artwork>> {
        let polygon = std::mem::take(&mut self.polygon);
        (polygon.len() >= 3)
            .then(|| lasso(&polygon, mode))
            .flatten()
    }

    /// Drop whatever is under way.
    pub fn cancel(&mut self) {
        self.drag = None;
        self.polygon.clear();
    }

    /// The outline of what is being made, in artwork units, the lasso
    /// reaching to `cursor`.
    pub fn outline(&self, cursor: Point) -> Option<BezPath> {
        let mut path = BezPath::new();
        match &self.drag {
            Some(AreaDrag::Box { start, now, oval }) => {
                let area = Rect::from_points(*start, *now);
                if *oval {
                    return Some(ui::vello::kurbo::Shape::to_path(
                        &ui::vello::kurbo::Ellipse::from_rect(area),
                        0.1,
                    ));
                }
                return Some(ui::vello::kurbo::Shape::to_path(&area, 0.1));
            }
            Some(AreaDrag::Lasso { points }) if self.polygon.is_empty() => {
                for (index, point) in points.iter().enumerate() {
                    if index == 0 {
                        path.move_to(*point);
                    } else {
                        path.line_to(*point);
                    }
                }
            }
            _ => {
                let mut corners = self.polygon.iter();
                path.move_to(*corners.next()?);
                for corner in corners {
                    path.line_to(*corner);
                }
                path.line_to(cursor);
            }
        }
        Some(path)
    }
}

fn select(select: Select, mode: Option<String>) -> Invoke<Artwork> {
    Artwork::SELECT.with(Select { mode, ..select })
}

/// Select nothing.
pub fn none() -> Invoke<Artwork> {
    select(Select::default(), Some("none".into()))
}

fn lasso(points: &[Point], mode: Option<String>) -> Option<Invoke<Artwork>> {
    (points.len() >= 3).then(|| {
        select(
            Select {
                points: Some(points.iter().map(|point| [point.x, point.y]).collect()),
                ..Select::default()
            },
            mode,
        )
    })
}

/// The selection of an area found by colour.
pub fn found(region: &Region, mode: Option<String>) -> Invoke<Artwork> {
    select(
        Select {
            d: Some(path_data(&region.to_path())),
            ..Select::default()
        },
        mode,
    )
}

/// The bucket's fill of an area found by colour, kept inside the selection
/// when there is one: a new mark `id` in the ink's colour.
pub fn bucket(art: &Artwork, region: &Region, ink: Ink, id: String) -> Option<Invoke<Artwork>> {
    let region = match &art.selection {
        Some(selection) => region.combine(&selection.region(), Op::Intersect),
        None => region.clone(),
    };
    if region.is_empty() {
        return None;
    }
    Some(Artwork::DRAW.with(Draw {
        layer: ink.layer,
        marks: vec![NewMark {
            id: Some(id),
            fill: Some(ink.colour),
            feather: art
                .selection
                .as_ref()
                .map(|selection| selection.feather)
                .filter(|feather| *feather > 0.0),
            ..NewMark::new(Shape::Path {
                d: path_data(&region.to_path()),
            })
        }],
    }))
}

/// Draw `path` (already where it is shown) as a selection's outline.
pub fn draw_outline(scene: &mut Scene, path: &BezPath, phase: f64) {
    crate::render::selection_outline(scene, path, 1.0, phase);
}

/// The selection's outline where the screen shows it.
pub fn selection_path(art: &Artwork, to_screen: Affine) -> Option<BezPath> {
    let selection = art.selection.as_ref()?;
    let mut path = BezPath::from_svg(&selection.d).ok()?;
    path.apply_affine(to_screen);
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lasso_is_dragged_or_clicked_out() {
        let mut gesture = AreaGesture::default();
        gesture.press(Point::new(0.0, 0.0), None);
        for at in [(50.0, 0.0), (50.0, 50.0), (0.0, 50.0)] {
            gesture.drag_to(Point::new(at.0, at.1), 2.0);
        }
        let Some(select) = (gesture.release(4.0, None)).and_then(|invoke| {
            crate::actions::read_as::<crate::selection::Select>(&invoke, Artwork::SELECT.id)
        }) else {
            panic!("a dragged lasso selects");
        };
        assert_eq!(select.points.as_ref().map(Vec::len), Some(4));

        for at in [(0.0, 0.0), (80.0, 0.0), (80.0, 80.0)] {
            gesture.press(Point::new(at.0, at.1), None);
            assert!(gesture.release(4.0, None).is_none());
        }
        assert_eq!(gesture.polygon.len(), 3);
        // Clicking its first point closes it, adding to the selection.
        gesture.press(Point::new(1.0, 1.0), None);
        let Some(select) = (gesture.release(4.0, mode(true, false))).and_then(|invoke| {
            crate::actions::read_as::<crate::selection::Select>(&invoke, Artwork::SELECT.id)
        }) else {
            panic!("closing the lasso selects");
        };
        assert_eq!(select.mode.as_deref(), Some("add"));
        assert_eq!(select.points.as_ref().map(Vec::len), Some(3));
        assert!(!gesture.active());
    }

    #[test]
    fn a_box_selects_and_a_click_selects_nothing() {
        let mut gesture = AreaGesture::default();
        gesture.press(Point::new(10.0, 10.0), Some(true));
        gesture.drag_to(Point::new(110.0, 60.0), 1.0);
        let Some(select) = (gesture.release(4.0, None)).and_then(|invoke| {
            crate::actions::read_as::<crate::selection::Select>(&invoke, Artwork::SELECT.id)
        }) else {
            panic!("a box selects");
        };
        assert_eq!(select.ellipse, Some([10.0, 10.0, 100.0, 50.0]));
        gesture.press(Point::new(10.0, 10.0), Some(false));
        let Some(select) = (gesture.release(4.0, None)).and_then(|invoke| {
            crate::actions::read_as::<crate::selection::Select>(&invoke, Artwork::SELECT.id)
        }) else {
            panic!("a click selects nothing");
        };
        assert_eq!(select.mode.as_deref(), Some("none"));
    }
}
