//! What a map does for the person's answer (`docs/designs/app-language/canvas.md` §7,
//! "Everything stays in view"): the place that answers where they pointed stands whole in view
//! as it grows, a place arriving after it is shown only as far as the answer stays in view, and
//! the rectangle they point at is outlined softly in the theme's own role.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Vec2};
use vello::peniko::Color;

use super::{ARRIVAL_MARGIN, Map, MapCamera};
use crate::theme::Theme;
use crate::widget::primitives::SurfaceLevel;

impl Map {
    /// The person pointed at `at` (canvas pixels): what arrives after this is the answer to it and
    /// grows out of it, the answer kept so far is let go, and the part of the canvas they see is
    /// reported first, so what answers can stand where they look.
    pub(super) fn point_at(&mut self, at: Point) {
        self.asked = Some(at);
        self.answer = None;
        if let Some(seen) = self.seen_rect() {
            self.seen.fire(Some(crate::component::Value::List(
                [seen.x0, seen.y0, seen.width(), seen.height()]
                    .into_iter()
                    .map(|value| crate::component::Value::Int(value.round() as i64))
                    .collect(),
            )));
        }
    }

    /// The part of the canvas the viewport shows where the camera is going, in canvas pixels.
    /// `None` on a map not laid out yet.
    pub(super) fn seen_rect(&self) -> Option<Rect> {
        let (width, height) = (self.rect.width(), self.rect.height());
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        let zoom = self.camera.zoom.max(f64::EPSILON);
        Some(Rect::from_origin_size(
            self.camera.origin,
            (width / zoom, height / zoom),
        ))
    }

    /// Take up the answer the map built before this one kept (`held` its pins, `answer` its
    /// answer), and choose it again among the places that arrived since: of those that grow from
    /// where the person pointed, the one standing nearest the point is the answer (the turn at
    /// the caret, a thing found at a click), unless the answer kept stands nearer.
    pub(super) fn take_answer(&mut self, held: &[String], answer: Option<String>) {
        let Some(asked) = self.asked else {
            self.answer = None;
            return;
        };
        let distance = |x: f64, y: f64| (x - asked.x).hypot(y - asked.y);
        let mut best: Option<(String, f64)> = answer.and_then(|answer| {
            self.pins
                .iter()
                .find(|(key, _, _)| *key == answer)
                .map(|(key, place, _)| (key.clone(), distance(place.x, place.y)))
        });
        for (key, place, _) in &self.pins {
            if !place.grows || held.contains(key) {
                continue;
            }
            let from = distance(place.x, place.y);
            if best.as_ref().is_none_or(|(_, nearest)| from < *nearest) {
                best = Some((key.clone(), from));
            }
        }
        self.answer = best.map(|(key, _)| key);
    }

    /// Where the pin `key` stands on the canvas, in canvas pixels, its caption's band included: as
    /// tall as the region laid it out last (a hugging answer as tall as its words have grown),
    /// else as its place says.
    pub(super) fn area_of(&self, key: &str) -> Option<Rect> {
        let (_, place, _) = self.pins.iter().find(|(held, _, _)| held == key)?;
        let zoom = self.camera.zoom.max(f64::EPSILON);
        let band = place.band / zoom;
        let height = match (self.laid.borrow().get(key), place.height) {
            (Some(laid), _) => laid.height() / zoom,
            // Not laid out yet: one that hugs what it holds is shown by its top, as it arrives.
            (None, Some(height)) if place.hug => height.min(super::LEAST_SIDE) + band,
            (None, Some(height)) => height + band,
            (None, None) => place.width / 1.6 + band,
        };
        Some(Rect::from_origin_size(
            Point::new(place.x, place.y - band),
            (place.width.max(1.0), height.max(1.0)),
        ))
    }

    /// Keep the person's answer whole in view as it grows: the least pan that shows all of it,
    /// never a zoom. Nothing while the person's own hand is on the canvas, and nothing once they
    /// moved the lens themselves ([`Map::set_camera`] lets the answer go).
    pub(super) fn keep_answer(&mut self) {
        if self.pan.is_some() || self.edge_drag.is_some() || self.free_press.is_some() {
            return;
        }
        let Some(area) = self.answer.as_deref().and_then(|key| self.area_of(key)) else {
            return;
        };
        let margin = ARRIVAL_MARGIN / self.camera.zoom.max(f64::EPSILON);
        if let Some(camera) = self.revealing(area, margin) {
            self.pan_to(camera);
        }
    }

    /// Where the camera shows `area` (canvas pixels) as well as it can while `answer` stays whole
    /// in view: with everything in `kept` when all fit, else with the answer alone, else nowhere
    /// new. `None` leaves the camera where it is: a place an agent put off-screen is not chased at
    /// the cost of the answer the person is reading.
    pub(super) fn revealing_keeping(
        &self,
        area: Rect,
        margin: f64,
        kept: &[Rect],
        answer: Rect,
    ) -> Option<MapCamera> {
        let mut all: Vec<Rect> = kept.to_vec();
        all.push(answer);
        [all.as_slice(), std::slice::from_ref(&answer)]
            .into_iter()
            .filter_map(|keep| self.revealing_with(area, margin, keep))
            .find(|camera| self.shows(*camera, area.union(answer), margin))
    }

    /// Whether `camera` shows `area` (canvas pixels) whole, `margin` inside the viewport's edges.
    fn shows(&self, camera: MapCamera, area: Rect, margin: f64) -> bool {
        let zoom = camera.zoom.max(f64::EPSILON);
        let span = Vec2::new(self.rect.width(), self.rect.height()) / zoom;
        let seen = Rect::from_origin_size(camera.origin, (span.x, span.y)).inflate(0.5, 0.5);
        let area = area.inflate(margin, margin);
        area.x0 >= seen.x0 && area.y0 >= seen.y0 && area.x1 <= seen.x1 && area.y1 <= seen.y1
    }
}

/// How the rectangle the person points at is outlined (`canvas.md` §1, §5): a soft line rounded
/// to the theme's place radius, so it reads as where a place will stand, in the theme's
/// selection wash, a role lighter than the focus accent, so it never reads as a control in focus.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct OutlineLook {
    /// The line's colour: the theme's selection wash.
    pub(super) colour: Color,
    /// How round its corners are at zoom 1, in logical pixels: a place's own.
    pub(super) radius: f64,
    /// How wide the line is on screen, in logical pixels, at any zoom.
    pub(super) width: f64,
}

impl OutlineLook {
    /// The outline under `theme`.
    pub(super) fn of(theme: &Theme) -> Self {
        Self {
            colour: theme.tokens.colors.intent.selection_tint,
            radius: SurfaceLevel::Window.radius(theme),
            width: 1.0,
        }
    }

    /// Outline `rect` (on screen) as a place drawn at `zoom` is rounded.
    pub(super) fn stroke(self, scene: &mut Scene, transform: Affine, rect: Rect, zoom: f64) {
        let radius = (self.radius * zoom)
            .min(rect.width() / 2.0)
            .min(rect.height() / 2.0)
            .max(0.0);
        scene.stroke(
            &vello::kurbo::Stroke::new(self.width),
            transform,
            self.colour,
            None,
            &RoundedRect::from_rect(rect, radius),
        );
    }
}
