//! A strip's windows shown on a canvas: each window group at a place of its
//! own on a 2D surface, seen through a camera that pans and zooms.
//!
//! The canvas is another arrangement of the same windows (the desktop's
//! `workspace-model.md` §6): the row keeps its groups, their tabs and their
//! hosts, and only where it lays them out changes. Places are canvas pixels;
//! the camera says which canvas point stands at the row's top-left corner
//! and how many screen pixels a canvas pixel takes. A window is laid out at
//! its zoomed size, so what is drawn, hit-tested and damaged is where it is
//! on screen; a client window keeps its canvas size and is drawn scaled
//! (the host configures it to [`CanvasView::canvas_size_of`]).
//!
//! Panning moves the laid-out windows without restyling them, as a strip's
//! pan does; zooming or moving a window restyles them in place.

use std::collections::HashMap;

use taffy::{Dimension, LengthPercentageAuto, Position, Size, Style};
use vello::kurbo::{Point, Rect, Vec2};

use super::GroupRow;
use crate::num::f32_of_f64;
use crate::widget::Widget as _;

/// The smallest and largest zoom a canvas is shown at.
pub const CANVAS_ZOOM_MIN: f64 = 0.2;
pub const CANVAS_ZOOM_MAX: f64 = 2.0;

/// How a row's windows stand on a canvas, and how much of it shows.
#[derive(Clone, Debug, PartialEq)]
pub struct CanvasView {
    /// The canvas point at the row's top-left corner.
    pub origin: Point,
    /// Screen pixels per canvas pixel.
    pub zoom: f64,
    /// Each placed window group's rect, in canvas pixels. A group with no
    /// place yet is not shown until it gets one.
    pub places: HashMap<u64, Rect>,
}

impl Default for CanvasView {
    fn default() -> Self {
        Self {
            origin: Point::ORIGIN,
            zoom: 1.0,
            places: HashMap::new(),
        }
    }
}

impl CanvasView {
    /// Where canvas point `p` is on screen, for a row whose top-left corner
    /// is at `corner`.
    #[must_use]
    pub fn to_screen(&self, corner: Point, p: Point) -> Point {
        corner + (p - self.origin) * self.zoom
    }

    /// The canvas point under screen point `p`, for a row whose top-left
    /// corner is at `corner`.
    #[must_use]
    pub fn to_canvas(&self, corner: Point, p: Point) -> Point {
        self.origin + (p - corner) / self.zoom.max(f64::EPSILON)
    }

    /// Canvas rect `r` on screen.
    #[must_use]
    pub fn rect_to_screen(&self, corner: Point, r: Rect) -> Rect {
        Rect::from_points(
            self.to_screen(corner, r.origin()),
            self.to_screen(corner, Point::new(r.x1, r.y1)),
        )
    }

    /// Screen rect `r` on the canvas.
    #[must_use]
    pub fn rect_to_canvas(&self, corner: Point, r: Rect) -> Rect {
        Rect::from_points(
            self.to_canvas(corner, r.origin()),
            self.to_canvas(corner, Point::new(r.x1, r.y1)),
        )
    }

    /// The size a window drawn `on_screen` has on the canvas: what its
    /// client is configured to, so zooming scales its pixels rather than
    /// resizing it.
    #[must_use]
    pub fn canvas_size_of(&self, on_screen: Rect) -> (f64, f64) {
        let zoom = self.zoom.max(f64::EPSILON);
        (on_screen.width() / zoom, on_screen.height() / zoom)
    }

    /// Zoom by `factor` about screen point `about`, for a row whose
    /// top-left corner is at `corner`: the canvas point under `about` stays
    /// under it. The zoom stays within [`CANVAS_ZOOM_MIN`] and
    /// [`CANVAS_ZOOM_MAX`].
    pub fn zoom_about(&mut self, corner: Point, about: Point, factor: f64) {
        let held = self.to_canvas(corner, about);
        self.zoom = (self.zoom * factor).clamp(CANVAS_ZOOM_MIN, CANVAS_ZOOM_MAX);
        self.origin = held - (about - corner) / self.zoom;
    }

    /// Pan by `delta` screen pixels: the canvas follows the hand.
    pub fn pan_by(&mut self, delta: Vec2) {
        self.origin -= delta / self.zoom.max(f64::EPSILON);
    }

    /// Whether only the camera's position differs from `other`: the windows
    /// move without being laid out again.
    fn only_panned_from(&self, other: &Self) -> bool {
        (self.zoom - other.zoom).abs() <= f64::EPSILON && self.places == other.places
    }
}

impl GroupRow {
    /// Show this row's windows on a canvas (`Some`), or in their strip.
    pub fn set_canvas(&mut self, mut view: Option<CanvasView>) {
        // A window being carried stays under the hand, whatever place the
        // host still holds for it.
        if let (Some(drag), Some(was), Some(now)) =
            (self.group_drag, self.canvas.as_ref(), view.as_mut())
            && self.canvas_drag_from.is_some()
            && let Some(carried) = was.places.get(&drag.group_id)
        {
            now.places.insert(drag.group_id, *carried);
        }
        if self.canvas == view {
            return;
        }
        let only_panned = match (&self.canvas, &view) {
            (Some(was), Some(now)) => now.only_panned_from(was),
            _ => false,
        };
        self.canvas = view;
        let carriable = self.canvas.is_some();
        for group in &mut self.groups {
            group.set_carriable(carriable);
        }
        if only_panned {
            crate::widget::note_layout_moved();
        } else {
            self.structure_dirty = true;
        }
    }

    /// How the row's windows stand on its canvas, while it shows one.
    #[must_use]
    pub fn canvas(&self) -> Option<&CanvasView> {
        self.canvas.as_ref()
    }

    /// The window group whose drawn rect holds screen point `p`, while the
    /// row shows a canvas.
    #[must_use]
    pub fn canvas_group_at(&self, p: Point) -> Option<u64> {
        self.canvas.as_ref()?;
        self.groups
            .iter()
            .rev()
            .find(|group| group.get_rect().contains(p))
            .map(|group| group.id)
    }

    /// A window carried by its bar to a new place on the canvas, once let
    /// go: its group and the place, in canvas pixels, for the host to keep.
    pub fn take_pending_canvas_move(&mut self) -> Option<(u64, Rect)> {
        self.pending_canvas_move.take()
    }

    /// The hand carrying window `id` has moved `by` screen pixels since it
    /// took it: the window follows.
    pub(super) fn carry_on_canvas(&mut self, id: u64, by: Vec2) {
        let Some(view) = self.canvas.as_mut() else {
            return;
        };
        let Some(place) = view.places.get(&id).copied() else {
            return;
        };
        let from = *self.canvas_drag_from.get_or_insert(place);
        let moved = from + by / view.zoom.max(f64::EPSILON);
        if moved != place {
            view.places.insert(id, moved);
            self.structure_dirty = true;
        }
    }

    /// The hand let window `id` go: kept where it is (`keep`), or back where
    /// it was.
    pub(super) fn put_down_on_canvas(&mut self, id: u64, keep: bool) {
        let Some(from) = self.canvas_drag_from.take() else {
            return;
        };
        let Some(view) = self.canvas.as_mut() else {
            return;
        };
        let Some(place) = view.places.get(&id).copied() else {
            return;
        };
        if keep {
            self.pending_canvas_move = Some((id, place));
        } else {
            view.places.insert(id, from);
            self.structure_dirty = true;
        }
    }

    /// The style of window `id`'s node on the canvas over its `own`: placed
    /// absolutely at its zoomed place, relative to the row's corner, before
    /// the camera's pan (which [`Self::apply_canvas_layout`] adds, as a
    /// strip's pan is added).
    pub(super) fn canvas_node_style(view: &CanvasView, id: u64, own: Style) -> Style {
        let Some(place) = view.places.get(&id) else {
            return Style {
                display: taffy::Display::None,
                ..own
            };
        };
        let zoom = view.zoom;
        let length = |v: f64| LengthPercentageAuto::length(f32_of_f64(v * zoom));
        Style {
            position: Position::Absolute,
            inset: taffy::Rect {
                left: length(place.x0),
                top: length(place.y0),
                right: LengthPercentageAuto::auto(),
                bottom: LengthPercentageAuto::auto(),
            },
            size: Size {
                width: Dimension::length(f32_of_f64(place.width() * zoom)),
                height: Dimension::length(f32_of_f64(place.height() * zoom)),
            },
            min_size: Size::auto(),
            max_size: Size::auto(),
            margin: taffy::Rect::zero(),
            flex_grow: 0.0,
            flex_shrink: 0.0,
            ..own
        }
    }

    /// Lay the windows out where the canvas has them: the row's corner less
    /// the camera's origin, zoomed, plus each window's place.
    pub(super) fn apply_canvas_layout(&mut self, taffy: &taffy::TaffyTree<()>, view: &CanvasView) {
        let corner = Point::new(self.rect.x0, self.rect.y0);
        let origin = corner - view.origin.to_vec2() * view.zoom;
        self.pan.viewport = self.rect;
        self.pan.inset = 0.0;
        for (group, node) in self.groups.iter_mut().zip(self.node_ids.iter()) {
            // A window that came to the row while it shows a canvas is
            // carried as the others are.
            group.set_carriable(true);
            if !view.places.contains_key(&group.id) {
                continue;
            }
            group.apply_layout(taffy, *node, origin);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{
        dispatch_at, make_group, press_at, relayout_row_owned, relayout_strip,
    };
    use super::*;
    use winit::event::{ElementState, MouseButton, WindowEvent};

    fn moved_to(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }

    fn released() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        }
    }

    /// On a canvas a window is carried by its title, and the carry owns the
    /// pointer until it is let go: the deck lays every strip's row over the
    /// canvas, and another row must not take the motion or the release. A
    /// drag on empty canvas is never a strip pan.
    #[test]
    fn a_window_is_carried_by_its_title_and_owns_the_pointer_until_let_go() {
        let mut row = GroupRow::new(vec![make_group(1, &[10]), make_group(2, &[20])]);
        row.set_canvas(Some(view(
            Point::ORIGIN,
            1.0,
            &[
                (1, Rect::new(40.0, 40.0, 640.0, 440.0)),
                (2, Rect::new(700.0, 40.0, 1200.0, 440.0)),
            ],
        )));
        let (mut row, taffy, root) = relayout_row_owned(row, 1920.0);
        let title = row.groups[0].tab_rects()[0].center();
        assert!(
            row.groups[0].carry_grab_hit(title),
            "the title carries the window"
        );
        press_at(&mut row, &taffy, root, title);
        for step in 1..=6 {
            let at = title + Vec2::new(0.0, 30.0 * f64::from(step));
            dispatch_at(&mut row, &taffy, root, &moved_to(at));
        }
        assert!(row.wants_pointer_capture(), "the carry owns the pointer");
        dispatch_at(&mut row, &taffy, root, &released());
        assert!(!row.wants_pointer_capture());
        let (group, place) = row.take_pending_canvas_move().expect("the carry is kept");
        assert_eq!(group, 1);
        assert!((place.y0 - 220.0).abs() < 1.0, "{place:?}");

        // Empty canvas: no strip pan, nothing claimed.
        let empty = Point::new(1500.0, 800.0);
        press_at(&mut row, &taffy, root, empty);
        for step in 1..=6 {
            let at = empty + Vec2::new(-40.0 * f64::from(step), 0.0);
            dispatch_at(&mut row, &taffy, root, &moved_to(at));
        }
        assert!(!row.wants_pointer_capture(), "a canvas has no strip to pan");
        dispatch_at(&mut row, &taffy, root, &released());
    }

    fn view(origin: Point, zoom: f64, places: &[(u64, Rect)]) -> CanvasView {
        CanvasView {
            origin,
            zoom,
            places: places.iter().copied().collect(),
        }
    }

    #[test]
    fn windows_stand_where_the_canvas_places_them() {
        let mut row = GroupRow::new(vec![make_group(1, &[10]), make_group(2, &[20])]);
        row.set_canvas(Some(view(
            Point::ORIGIN,
            1.0,
            &[
                (1, Rect::new(100.0, 50.0, 740.0, 530.0)),
                (2, Rect::new(800.0, 400.0, 1200.0, 700.0)),
            ],
        )));
        relayout_strip(&mut row, 1920.0);
        let first = row.groups[0].get_rect();
        let second = row.groups[1].get_rect();
        assert!(
            (first.x0 - 100.0).abs() < 0.5 && (first.y0 - 50.0).abs() < 0.5,
            "{first:?}"
        );
        assert!((first.width() - 640.0).abs() < 0.5, "{first:?}");
        assert!(
            (second.x0 - 800.0).abs() < 0.5 && (second.y0 - 400.0).abs() < 0.5,
            "{second:?}"
        );
        assert_eq!(row.canvas_group_at(Point::new(900.0, 500.0)), Some(2));
        assert_eq!(
            row.canvas_group_at(Point::new(760.0, 100.0)),
            None,
            "between windows"
        );
    }

    #[test]
    fn panning_and_zooming_move_every_window_together() {
        let mut row = GroupRow::new(vec![make_group(1, &[10])]);
        let place = Rect::new(100.0, 100.0, 500.0, 400.0);
        row.set_canvas(Some(view(Point::new(50.0, -20.0), 0.5, &[(1, place)])));
        relayout_strip(&mut row, 1920.0);
        let rect = row.groups[0].get_rect();
        // (100 - 50) * 0.5 = 25 across, (100 + 20) * 0.5 = 60 down.
        assert!(
            (rect.x0 - 25.0).abs() < 0.5 && (rect.y0 - 60.0).abs() < 0.5,
            "{rect:?}"
        );
        assert!((rect.width() - 200.0).abs() < 0.5, "{rect:?}");
    }

    #[test]
    fn a_window_with_no_place_yet_is_not_shown() {
        let mut row = GroupRow::new(vec![make_group(1, &[10]), make_group(2, &[20])]);
        row.set_canvas(Some(view(
            Point::ORIGIN,
            1.0,
            &[(2, Rect::new(0.0, 0.0, 300.0, 200.0))],
        )));
        relayout_strip(&mut row, 1920.0);
        assert_eq!(row.canvas_group_at(Point::new(10.0, 10.0)), Some(2));
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let mut camera = view(Point::new(10.0, 10.0), 1.0, &[]);
        let corner = Point::new(0.0, 32.0);
        let about = Point::new(400.0, 300.0);
        let before = camera.to_canvas(corner, about);
        camera.zoom_about(corner, about, 1.5);
        let after = camera.to_canvas(corner, about);
        assert!((before - after).hypot() < 1e-9);
        camera.zoom_about(corner, about, 100.0);
        assert!((camera.zoom - CANVAS_ZOOM_MAX).abs() < f64::EPSILON);
        let mut panned = camera.clone();
        panned.pan_by(Vec2::new(20.0, 0.0));
        assert!(
            panned.origin.x < camera.origin.x,
            "the canvas follows the hand"
        );
    }

    #[test]
    fn leaving_the_canvas_lays_the_strip_out_again() {
        let mut row = GroupRow::new(vec![make_group(1, &[10])]);
        row.set_canvas(Some(view(
            Point::ORIGIN,
            1.0,
            &[(1, Rect::new(500.0, 300.0, 900.0, 600.0))],
        )));
        relayout_strip(&mut row, 1920.0);
        row.set_canvas(None);
        relayout_strip(&mut row, 1920.0);
        let rect = row.groups[0].get_rect();
        assert!(rect.y0 < 1.0, "back in its strip: {rect:?}");
    }
}
