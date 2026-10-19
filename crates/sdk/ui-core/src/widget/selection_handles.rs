//! Selection handles for a canvas: the frame around what is selected, its
//! resize handles at the corners and (once the frame is big enough on
//! screen, or always with [`SelectionHandles::with_edges`]) at the middle of
//! each edge, and its rotation handle above (unless
//! [`SelectionHandles::without_rotation`]: a crop, a text box, a clip),
//! drawn in your colour and hit-tested at a constant size on screen whatever
//! the zoom.
//!
//! The frame may be turned (a rotated shape keeps a frame along its own
//! sides), and every drag is answered in the frame's own axes.
//!
//! The frame owns no document state. A canvas keeps its selection, asks
//! [`SelectionHandles::handle_at`] what a press landed on, and turns the drag
//! into its own action with [`SelectionHandles::stretch_for`] or
//! [`SelectionHandles::rotation_for`], so every edit still goes through the
//! canvas's published vocabulary.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo's geometry operators (Affine, Point, Vec2) are float geometry, not overflowing integer arithmetic"
)]

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, Stroke, Vec2};
use vello::peniko::Fill;

use crate::theme::Theme;
use crate::tokens::Metrics;

/// Feedback over authored canvas content. The contrasting under-stroke keeps
/// a brush footprint or erased boundary visible over either light or dark paint.
#[derive(Clone, Copy, Debug)]
pub enum ToolFeedback {
    /// A previously erased region, emphasized when hovered or selected.
    Erased { marked: bool },
    /// The region the current brush or eraser will affect.
    Footprint,
}

impl ToolFeedback {
    /// Paint a screen-space boundary without changing the authored artwork.
    pub fn draw(self, scene: &mut Scene, path: &BezPath, theme: &Theme) {
        let colors = &theme.tokens.colors;
        let rule = f64::from(theme.tokens.metrics.chrome_rule_px).max(1.0);
        let (outline, ink) = match self {
            Self::Erased { marked } => (
                Stroke::new(rule).with_dashes(0.0, if marked { [6.0, 3.0] } else { [2.0, 3.0] }),
                if marked {
                    colors.intent.accent_ink
                } else {
                    colors.text_dim
                },
            ),
            Self::Footprint => (Stroke::new(rule), colors.text_primary),
        };
        scene.stroke(
            &Stroke::new(rule * 3.0),
            Affine::IDENTITY,
            colors.surfaces.base,
            None,
            path,
        );
        scene.stroke(&outline, Affine::IDENTITY, ink, None, path);
    }
}

/// A grip on the selection frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
    /// Above the frame: drag around the centre to rotate.
    Rotate,
}

impl Handle {
    pub const CORNERS: [Self; 4] = [
        Self::TopLeft,
        Self::TopRight,
        Self::BottomRight,
        Self::BottomLeft,
    ];
    pub const EDGES: [Self; 4] = [Self::Top, Self::Right, Self::Bottom, Self::Left];

    /// Where the handle sits on `bounds`, as fractions of its width and
    /// height from the top-left; the rotation handle sits at the top centre.
    fn place(self) -> (f64, f64) {
        match self {
            Self::TopLeft => (0.0, 0.0),
            Self::Top | Self::Rotate => (0.5, 0.0),
            Self::TopRight => (1.0, 0.0),
            Self::Right => (1.0, 0.5),
            Self::BottomRight => (1.0, 1.0),
            Self::Bottom => (0.5, 1.0),
            Self::BottomLeft => (0.0, 1.0),
            Self::Left => (0.0, 0.5),
        }
    }

    /// The point of `bounds` a resize keeps still: the one opposite this
    /// handle (the centre for the rotation handle).
    pub fn anchor(self, bounds: Rect) -> Point {
        if self == Self::Rotate {
            return bounds.center();
        }
        let (u, v) = self.place();
        Point::new(
            bounds.x0 + (1.0 - u) * bounds.width(),
            bounds.y0 + (1.0 - v) * bounds.height(),
        )
    }

    /// Whether dragging this handle resizes along one axis only.
    #[must_use]
    pub fn is_edge(self) -> bool {
        Self::EDGES.contains(&self)
    }

    /// Whether dragging the handle changes the width, the height.
    fn axes(self) -> (bool, bool) {
        match self {
            Self::Top | Self::Bottom => (false, true),
            Self::Left | Self::Right => (true, false),
            Self::Rotate => (false, false),
            _ => (true, true),
        }
    }

    /// The resize cursor for the handle on a frame turned by `angle`
    /// radians, the move cursor for the rotation handle.
    pub fn cursor(self, angle: f64) -> winit::window::CursorIcon {
        use winit::window::CursorIcon;
        if self == Self::Rotate {
            return CursorIcon::Grab;
        }
        let (u, v) = self.place();
        let direction = (v - 0.5).atan2(u - 0.5) + angle;
        // Eighths of a turn, folded onto the four double-headed cursors.
        let eighth = (direction / std::f64::consts::FRAC_PI_4)
            .round()
            .rem_euclid(4.0);
        if eighth < 0.5 {
            CursorIcon::EwResize
        } else if eighth < 1.5 {
            CursorIcon::NwseResize
        } else if eighth < 2.5 {
            CursorIcon::NsResize
        } else {
            CursorIcon::NeswResize
        }
    }
}

/// The frame around a selection, in content coordinates: `bounds` turned by
/// `angle` about its centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectionHandles {
    pub bounds: Rect,
    /// Radians, clockwise on a y-down canvas.
    pub angle: f64,
    /// The pointer target around each handle, in screen pixels.
    target: f64,
    /// Whether the frame offers its rotation handle.
    rotation: bool,
    /// Whether the edge handles show however small the frame is on screen.
    edges: bool,
}

impl SelectionHandles {
    /// The frame around `bounds`, its handles as easy to hit as any control
    /// under `metrics`.
    pub fn new(bounds: Rect, metrics: &Metrics) -> Self {
        Self {
            bounds,
            angle: 0.0,
            target: f64::from(metrics.min_pointer_target),
            rotation: true,
            edges: false,
        }
    }

    /// Offer the edge handles however small the frame is on screen, for a
    /// one-axis resize that must always be there (a crop, a text box's
    /// width).
    #[must_use]
    pub fn with_edges(mut self) -> Self {
        self.edges = true;
        self
    }

    /// No rotation handle: for what cannot turn (a crop, a clip, a frame
    /// aligned to its page).
    #[must_use]
    pub fn without_rotation(mut self) -> Self {
        self.rotation = false;
        self
    }

    /// The same frame turned by `angle` radians about its centre.
    #[must_use]
    pub fn turned(self, angle: f64) -> Self {
        Self { angle, ..self }
    }

    /// Frame space to content space.
    pub fn orientation(&self) -> Affine {
        Affine::rotate_about(self.angle, self.bounds.center())
    }

    /// How big a handle is drawn on screen; its target is larger.
    fn grip(&self) -> f64 {
        self.target * 0.3
    }

    /// How far above the frame the rotation handle sits on screen: one
    /// target, so its target never overlaps a corner's.
    fn rotate_reach(&self) -> f64 {
        self.target
    }

    /// Whether the edge handles show at `zoom`: only when the frame is big
    /// enough on screen that they do not crowd the corners.
    fn edges_shown(&self, zoom: f64) -> bool {
        if self.edges {
            return true;
        }
        let least = self.bounds.width().min(self.bounds.height()) * zoom;
        least >= self.target * 2.5
    }

    /// The handles that can be grabbed at `zoom`, the rotation handle first.
    pub fn handles(&self, zoom: f64) -> impl Iterator<Item = Handle> {
        let edges = self.edges_shown(zoom);
        let rotation = self.rotation;
        std::iter::once(Handle::Rotate)
            .filter(move |_| rotation)
            .chain(Handle::CORNERS)
            .chain(Handle::EDGES.into_iter().filter(move |_| edges))
    }

    /// Where `handle` sits in frame space at `zoom` (screen pixels per
    /// content unit).
    fn local(&self, handle: Handle, zoom: f64) -> Point {
        let b = self.bounds;
        let (u, v) = handle.place();
        let at = Point::new(b.x0 + u * b.width(), b.y0 + v * b.height());
        if handle == Handle::Rotate {
            at - Vec2::new(0.0, self.rotate_reach() / zoom.max(1e-6))
        } else {
            at
        }
    }

    /// Where `handle` sits in content coordinates at `zoom`.
    pub fn position(&self, handle: Handle, zoom: f64) -> Point {
        self.orientation() * self.local(handle, zoom)
    }

    /// The handle under `point` (content coordinates), if any.
    pub fn handle_at(&self, point: Point, zoom: f64) -> Option<Handle> {
        let reach = self.target / 2.0 / zoom.max(1e-6);
        let mut best: Option<(f64, Handle)> = None;
        for handle in self.handles(zoom) {
            let distance = self.position(handle, zoom).distance(point);
            if distance <= reach && best.is_none_or(|(nearest, _)| distance < nearest) {
                best = Some((distance, handle));
            }
        }
        best.map(|(_, handle)| handle)
    }

    /// Whether `point` (content coordinates) is inside the frame.
    pub fn contains(&self, point: Point) -> bool {
        self.bounds.contains(self.orientation().inverse() * point)
    }

    /// The resize a drag of `handle` from `from` to `to` asks for, as an
    /// affine in content space. The opposite side stays still, or the centre
    /// with `from_centre`; `keep_aspect` scales both ways alike. A drag past
    /// the anchor flips; the frame never collapses below half a unit.
    pub fn stretch_for(
        &self,
        handle: Handle,
        (from, to): (Point, Point),
        keep_aspect: bool,
        from_centre: bool,
    ) -> Affine {
        if handle == Handle::Rotate {
            return Affine::IDENTITY;
        }
        let into = self.orientation().inverse();
        let (from, to) = (into * from, into * to);
        let anchor = if from_centre {
            self.bounds.center()
        } else {
            handle.anchor(self.bounds)
        };
        let (along_x, along_y) = handle.axes();
        let ratio = |to: f64, from: f64, anchor: f64| {
            let before = from - anchor;
            if before.abs() < 1e-9 {
                1.0
            } else {
                (to - anchor) / before
            }
        };
        let mut sx = if along_x {
            ratio(to.x, from.x, anchor.x)
        } else {
            1.0
        };
        let mut sy = if along_y {
            ratio(to.y, from.y, anchor.y)
        } else {
            1.0
        };
        if keep_aspect {
            let uniform = match (along_x, along_y) {
                (true, true) => {
                    // The scale along the diagonal the pointer pulls.
                    let before = from - anchor;
                    let after = to - anchor;
                    after.dot(before) / before.hypot2().max(1e-12)
                }
                (true, false) => sx,
                _ => sy,
            };
            sx = uniform;
            sy = uniform;
        }
        let least = |scale: f64, extent: f64| {
            let floor = 0.5 / extent.max(1e-9);
            if scale.abs() < floor {
                floor.copysign(if scale == 0.0 { 1.0 } else { scale })
            } else {
                scale
            }
        };
        let sx = least(sx, self.bounds.width());
        let sy = least(sy, self.bounds.height());
        let local = Affine::translate(anchor.to_vec2())
            * Affine::scale_non_uniform(sx, sy)
            * Affine::translate(-anchor.to_vec2());
        self.orientation() * local * into
    }

    /// The rotation in degrees, clockwise, a drag of the rotation handle from
    /// `from` to `to` asks for about the centre; with `snap`, the frame's
    /// resulting angle lands on steps of 15.
    pub fn rotation_for(&self, from: Point, to: Point, snap: bool) -> f64 {
        let centre = self.bounds.center();
        let angle = |point: Point| (point.y - centre.y).atan2(point.x - centre.x);
        let degrees = (angle(to) - angle(from)).to_degrees();
        let degrees = (degrees + 540.0).rem_euclid(360.0) - 180.0;
        if snap {
            let start = self.angle.to_degrees();
            ((start + degrees) / 15.0).round() * 15.0 - start
        } else {
            degrees
        }
    }

    /// The rotation a drag asks for, as an affine about the frame's centre.
    pub fn rotation_affine(&self, from: Point, to: Point, snap: bool) -> Affine {
        let degrees = self.rotation_for(from, to, snap);
        Affine::rotate_about(degrees.to_radians(), self.bounds.center())
    }

    /// Draw the frame and its handles; `to_screen` maps content to screen.
    pub fn draw(&self, scene: &mut Scene, to_screen: Affine, zoom: f64, theme: &Theme) {
        self.draw_moved(scene, to_screen, Affine::IDENTITY, zoom, theme);
    }

    /// Draw the frame as `preview` (content space) carries it during a
    /// drag: the outline follows the preview, the grips stay upright and
    /// the same size on screen.
    pub fn draw_moved(
        &self,
        scene: &mut Scene,
        to_screen: Affine,
        preview: Affine,
        zoom: f64,
        theme: &Theme,
    ) {
        let zoom = zoom.max(1e-6);
        let accent = theme.tokens.colors.intent.accent_ink;
        let surface = theme.tokens.colors.surfaces.base;
        let line = Stroke::new(1.5);
        let place = preview * self.orientation();
        let screen = |local: Point| to_screen * (place * local);
        let b = self.bounds;
        let mut outline = BezPath::new();
        outline.move_to(screen(Point::new(b.x0, b.y0)));
        outline.line_to(screen(Point::new(b.x1, b.y0)));
        outline.line_to(screen(Point::new(b.x1, b.y1)));
        outline.line_to(screen(Point::new(b.x0, b.y1)));
        outline.close_path();
        scene.stroke(&line, Affine::IDENTITY, accent, None, &outline);
        // The rotation handle stands one target off the frame's top edge on
        // screen, whatever the preview did to the frame.
        let top = screen(self.local(Handle::Top, zoom));
        let centre = screen(b.center());
        let away = top - centre;
        let up = if away.hypot() > 1e-9 {
            away / away.hypot()
        } else {
            Vec2::new(0.0, -1.0)
        };
        let knob = top + up * self.rotate_reach();
        let grip = self.grip();
        for handle in self
            .handles(zoom)
            .filter(|handle| *handle != Handle::Rotate)
        {
            let at = screen(self.local(handle, zoom));
            // An edge grip is a bar along its edge; a corner grip a square.
            let size = match handle {
                Handle::Top | Handle::Bottom => (grip * 1.6, grip * 0.7),
                Handle::Left | Handle::Right => (grip * 0.7, grip * 1.6),
                _ => (grip, grip),
            };
            let shape = Rect::from_center_size(at, size);
            let turned = Affine::rotate_about(self.angle, at);
            scene.fill(Fill::NonZero, turned, surface, None, &shape);
            scene.stroke(&line, turned, accent, None, &shape);
        }
        if self.rotation {
            scene.stroke(&line, Affine::IDENTITY, accent, None, &Line::new(top, knob));
            let knob = Circle::new(knob, grip * 0.6);
            scene.fill(Fill::NonZero, Affine::IDENTITY, surface, None, &knob);
            scene.stroke(&line, Affine::IDENTITY, accent, None, &knob);
        }
    }

    /// The frame moved by `offset`, as during a drag.
    #[must_use]
    pub fn moved(&self, offset: Vec2) -> Self {
        Self {
            bounds: self.bounds + offset,
            ..*self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Point, b: Point) -> bool {
        a.distance(b) < 1e-6
    }

    #[test]
    fn edges_resize_one_axis_and_a_frame_can_drop_its_rotation() {
        let bounds = Rect::new(100.0, 100.0, 200.0, 150.0);
        let frame = SelectionHandles::new(bounds, &Metrics::BASE)
            .with_edges()
            .without_rotation();
        assert_eq!(
            frame.handle_at(Point::new(201.0, 125.0), 1.0),
            Some(Handle::Right)
        );
        assert!(Handle::Right.is_edge() && !Handle::TopLeft.is_edge());
        assert_eq!(frame.handle_at(Point::new(150.0, 72.0), 1.0), None);
        assert!(!frame.handles(1.0).any(|handle| handle == Handle::Rotate));
        let stretch = frame.stretch_for(
            Handle::Right,
            (Point::new(200.0, 125.0), Point::new(300.0, 140.0)),
            false,
            false,
        );
        assert!(close(
            stretch * Point::new(100.0, 125.0),
            Point::new(100.0, 125.0)
        ));
        assert!(close(
            stretch * Point::new(200.0, 150.0),
            Point::new(300.0, 150.0)
        ));
    }

    #[test]
    fn handles_are_hit_at_constant_screen_size() {
        let bounds = Rect::new(100.0, 100.0, 200.0, 150.0);
        let frame = SelectionHandles::new(bounds, &Metrics::BASE);
        assert_eq!(
            frame.handle_at(Point::new(201.0, 151.0), 1.0),
            Some(Handle::BottomRight)
        );
        // The target is the theme's minimum pointer target, and grows with
        // its UI scale.
        let edge = Point::new(
            200.0 + f64::from(Metrics::BASE.min_pointer_target) * 0.7,
            150.0,
        );
        assert_eq!(frame.handle_at(edge, 1.0), None);
        let doubled = SelectionHandles::new(bounds, &Metrics::BASE.scaled(2.0));
        assert_eq!(doubled.handle_at(edge, 1.0), Some(Handle::BottomRight));
        assert_eq!(frame.handle_at(Point::new(150.0, 125.0), 1.0), None);
        // Zoomed out 4x, a handle reaches four times further in content.
        assert_eq!(
            frame.handle_at(Point::new(140.0, 100.0), 0.25),
            Some(Handle::TopLeft)
        );
        assert_eq!(
            frame.handle_at(Point::new(150.0, 72.0), 1.0),
            Some(Handle::Rotate)
        );
        // Edge handles show once the frame is big enough on screen.
        assert_eq!(frame.handle_at(Point::new(200.0, 125.0), 1.0), None);
        assert_eq!(
            frame.handle_at(Point::new(200.0, 125.0), 4.0),
            Some(Handle::Right)
        );
    }

    #[test]
    fn a_corner_stretches_freely_or_keeps_its_aspect() {
        let frame = SelectionHandles::new(Rect::new(100.0, 100.0, 200.0, 150.0), &Metrics::BASE);
        let drag = (Point::new(200.0, 150.0), Point::new(300.0, 160.0));
        let free = frame.stretch_for(Handle::BottomRight, drag, false, false);
        assert!(close(
            free * Point::new(100.0, 100.0),
            Point::new(100.0, 100.0)
        ));
        assert!(close(
            free * Point::new(200.0, 150.0),
            Point::new(300.0, 160.0)
        ));
        let kept = frame.stretch_for(Handle::BottomRight, drag, true, false);
        let corner = kept * Point::new(200.0, 150.0);
        assert!(
            ((corner.x - 100.0) / (corner.y - 100.0) - 2.0).abs() < 1e-9,
            "{corner:?}"
        );
        let centred = frame.stretch_for(Handle::Right, drag, false, true);
        assert!(close(
            centred * Point::new(150.0, 125.0),
            Point::new(150.0, 125.0)
        ));
        assert!(close(
            centred * Point::new(200.0, 150.0),
            Point::new(300.0, 150.0)
        ));
        // An edge stretches only its own axis.
        let edge = frame.stretch_for(
            Handle::Bottom,
            (Point::new(150.0, 150.0), Point::new(170.0, 200.0)),
            false,
            false,
        );
        assert!(close(
            edge * Point::new(200.0, 150.0),
            Point::new(200.0, 200.0)
        ));
    }

    #[test]
    fn a_turned_frame_stretches_along_its_own_sides() {
        let quarter = std::f64::consts::FRAC_PI_2;
        let frame = SelectionHandles::new(Rect::new(-50.0, -10.0, 50.0, 10.0), &Metrics::BASE)
            .turned(quarter);
        // Turned a quarter, the frame's right edge points down.
        let right = frame.position(Handle::Right, 1.0);
        assert!(close(right, Point::new(0.0, 50.0)), "{right:?}");
        let stretch =
            frame.stretch_for(Handle::Right, (right, Point::new(5.0, 100.0)), false, false);
        assert!(close(
            stretch * Point::new(0.0, 50.0),
            Point::new(0.0, 100.0)
        ));
        assert!(close(
            stretch * Point::new(0.0, -50.0),
            Point::new(0.0, -50.0)
        ));
        assert!(frame.contains(Point::new(5.0, 40.0)));
        assert!(!frame.contains(Point::new(40.0, 5.0)));
    }

    #[test]
    fn rotation_snaps_the_resulting_angle() {
        let frame = SelectionHandles::new(Rect::new(100.0, 100.0, 200.0, 150.0), &Metrics::BASE);
        let quarter = frame.rotation_for(Point::new(150.0, 72.0), Point::new(250.0, 125.0), true);
        assert!((quarter - 90.0).abs() < 1e-9, "{quarter}");
        // A frame turned 5 degrees nudged a little lands back square.
        let turned = frame.turned(5_f64.to_radians());
        let step = turned.rotation_for(Point::new(150.0, 72.0), Point::new(151.0, 72.0), true);
        assert!((step + 5.0).abs() < 1e-9, "{step}");
    }

    #[test]
    fn a_stretch_never_collapses_the_frame() {
        let frame = SelectionHandles::new(Rect::new(0.0, 0.0, 100.0, 100.0), &Metrics::BASE);
        let flat = frame.stretch_for(
            Handle::Right,
            (Point::new(100.0, 50.0), Point::new(0.0, 50.0)),
            false,
            false,
        );
        assert!(flat.determinant().abs() > 1e-4);
    }
}
