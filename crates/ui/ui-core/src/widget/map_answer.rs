//! What a map does for the person's answer (`docs/designs/app-language/canvas.md` §7,
//! "Everything stays in view"): the place that answers where they pointed stands whole in view
//! as it grows, framed with what arrived beside it as far as they fit together, and
//! the rectangle they point at is outlined softly in the theme's own role.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Vec2};
use vello::peniko::Color;

use super::{ARRIVAL_MARGIN, Map, MapCamera};
use crate::widget::primitives::SurfaceLevel;
use tilo_ui_base::theme::Theme;

impl Map {
    /// The person pointed at `at` (canvas pixels): what arrives after this is the answer to it and
    /// grows out of it, the answer kept so far is let go, and the part of the canvas they see is
    /// reported first, so what answers can stand where they look.
    pub(super) fn point_at(&mut self, at: Point) {
        self.asked = Some(at);
        self.answer = None;
        self.asked_among = self.pins.iter().map(|(key, _, _)| key.clone()).collect();
        if let Some(seen) = self.seen_rect() {
            self.seen.fire(Some(tilo_ui_base::component::Value::List(
                [seen.x0, seen.y0, seen.width(), seen.height()]
                    .into_iter()
                    .map(|value| tilo_ui_base::component::Value::Int(value.round() as i64))
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
    /// where the person pointed, the one access nearest the point is the answer (the turn at
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

    /// Keep the person's answer whole in view as it grows, with what arrived beside it since they
    /// pointed (`canvas.md` §7): an agent's turn and the inbox it opened stand in view together.
    /// The camera moves only when something of them is not in view, and then pans the least that
    /// brings them in, keeping every place the person could see whole as far as that allows; a
    /// turn that stands in view moves nothing, whatever margin it keeps from the edge. Never a
    /// zoom; nothing while the person's own hand is on the canvas, and nothing once they moved
    /// the lens themselves ([`Map::set_camera`] lets the answer go).
    pub(super) fn keep_answer(&mut self) {
        if self.pan.is_some() || self.edge_drag.is_some() || self.free_press.is_some() {
            return;
        }
        let Some(key) = self.answer.clone() else {
            return;
        };
        // A view that has said no height yet (a turn before its words) stands at its most for
        // now: the camera waits for what it says rather than going to its foot.
        if self.unsaid.borrow().contains(&key) {
            return;
        }
        let Some(answer) = self.area_of(&key) else {
            return;
        };
        let margin = ARRIVAL_MARGIN / self.camera.zoom.max(f64::EPSILON);
        let group = self.answer_group(&key, answer, margin);
        // Everything new is in view: the person's view stays their own. A place put far off,
        // that cannot stand in view beside the answer, is not in the group, so it is never
        // chased at the answer's cost.
        if self.shows(self.camera, group, 0.0) {
            return;
        }
        // What the person was looking at: every place that stood before they pointed and stands
        // whole in view now. The least pan keeps them where they are as far as the group allows.
        let kept: Vec<Rect> = self
            .pins
            .iter()
            .filter(|(held, _, _)| self.asked_among.contains(held))
            .filter_map(|(held, _, _)| self.area_of(held))
            .filter(|rect| self.shows(self.camera, *rect, 0.0))
            .collect();
        if let Some(camera) = self.revealing_with(group, margin, &kept) {
            self.pan_to(camera);
        }
    }

    /// The answer `key` (at `answer`, canvas pixels) with every place that arrived since
    /// the person pointed and fits in the viewport beside it, nearest first: what the camera
    /// frames together.
    fn answer_group(&self, key: &str, answer: Rect, margin: f64) -> Rect {
        let gap = |rect: &Rect| {
            let dx = (rect.x0 - answer.x1).max(answer.x0 - rect.x1).max(0.0);
            let dy = (rect.y0 - answer.y1).max(answer.y0 - rect.y1).max(0.0);
            dx.hypot(dy)
        };
        let mut arrived: Vec<Rect> = self
            .pins
            .iter()
            .filter(|(held, _, _)| held != key && !self.asked_among.contains(held))
            .filter_map(|(held, _, _)| self.area_of(held))
            .collect();
        arrived.sort_by(|a, b| gap(a).total_cmp(&gap(b)));
        arrived.into_iter().fold(answer, |group, rect| {
            let grown = group.union(rect);
            if self.fits(grown, margin) {
                grown
            } else {
                group
            }
        })
    }

    /// Whether `area` (canvas pixels), `margin` inside the viewport's edges, fits in the viewport
    /// at the camera's zoom.
    fn fits(&self, area: Rect, margin: f64) -> bool {
        let zoom = self.camera.zoom.max(f64::EPSILON);
        let span = Vec2::new(self.rect.width(), self.rect.height()) / zoom;
        area.width() + 2.0 * margin <= span.x + 0.5 && area.height() + 2.0 * margin <= span.y + 0.5
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
