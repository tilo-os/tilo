//! Toolbar zoom control and page-scale transform.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace};

use crate::num;

pub(crate) const MIN: f64 = 0.4;
pub(crate) const MAX: f64 = 3.0;
pub(crate) const STEP: f64 = 0.1;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    Out,
    Reset,
    In,
}

/// Owns the local zoom preference, transform geometry and toolbar interaction.
pub(crate) struct ZoomControl {
    factor: f64,
    anchor: Point,
    hits: Vec<(Action, Rect)>,
}

impl ZoomControl {
    pub(crate) const WIDTH: f64 = 120.0;

    pub(crate) fn width(text_scale: f64) -> f64 {
        Self::WIDTH * text_scale
    }

    pub(crate) fn new() -> Self {
        Self {
            factor: 1.0,
            anchor: Point::ZERO,
            hits: Vec::new(),
        }
    }

    pub(crate) fn factor(&self) -> f64 {
        self.factor
    }

    pub(crate) fn anchor(&self) -> Point {
        self.anchor
    }

    pub(crate) fn set_anchor(&mut self, anchor: Point) {
        self.anchor = anchor;
    }

    pub(crate) fn set(&mut self, factor: f64) -> bool {
        let factor = factor.clamp(MIN, MAX);
        if (factor - self.factor).abs() <= 1e-6 {
            return false;
        }
        self.factor = factor;
        true
    }

    pub(crate) fn step(&mut self, delta: f64) -> bool {
        self.set(((self.factor + delta) * 10.0).round() / 10.0)
    }

    pub(crate) fn transform(&self, base: Affine) -> Affine {
        base * Affine::scale_about(self.factor, self.anchor)
    }

    pub(crate) fn screen_to_content(&self, point: Point) -> Point {
        Point::new(
            self.anchor.x + (point.x - self.anchor.x) / self.factor,
            self.anchor.y + (point.y - self.anchor.y) / self.factor,
        )
    }

    #[cfg(test)]
    pub(crate) fn content_x_to_screen(&self, x: f64) -> f64 {
        self.anchor.x + (x - self.anchor.x) * self.factor
    }

    #[cfg(test)]
    pub(crate) fn screen_x_to_content(&self, x: f64) -> f64 {
        self.anchor.x + (x - self.anchor.x) / self.factor
    }

    pub(crate) fn click(&mut self, point: Point) -> bool {
        let Some(action) = self
            .hits
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(action, _)| *action)
        else {
            return false;
        };
        match action {
            Action::Out => self.step(-STEP),
            Action::Reset => self.set(1.0),
            Action::In => self.step(STEP),
        }
    }

    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        row: Rect,
        theme: &Theme,
    ) {
        self.hits.clear();
        let scale = f64::from(text.text_scale());
        let center_y = f64::midpoint(row.y0, row.y1);
        let button_width = 32.0 * scale;
        let button_height = 32.0 * scale;
        let y = center_y - button_height / 2.0;
        let percent = format!("{}%", num::i32_of_f64((self.factor * 100.0).round()));
        let value_width = 48.0 * scale;
        let total = button_width * 2.0 + value_width + 8.0 * scale;
        let mut x = row.x1 - 14.0 * scale - total;

        self.hits.push((
            Action::Out,
            draw_button(scene, transform, text, theme, x, y, scale, "−"),
        ));
        x += button_width + 4.0 * scale;
        let value = Rect::new(x, y, x + value_width, y + button_height);
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(value, &percent).ghost().text_size(12.5),
        );
        self.hits.push((Action::Reset, value));
        x += value_width + 4.0 * scale;
        self.hits.push((
            Action::In,
            draw_button(scene, transform, text, theme, x, y, scale, "+"),
        ));
    }

    #[cfg(test)]
    pub(crate) fn hit_rects(&self) -> Vec<Rect> {
        self.hits.iter().map(|(_, rect)| *rect).collect()
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_button(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    x: f64,
    y: f64,
    scale: f64,
    label: &str,
) -> Rect {
    let width = 32.0 * scale;
    let rect = Rect::new(x, y, x + width, y + 32.0 * scale);
    Button::paint(
        scene,
        transform,
        text,
        theme,
        &ButtonFace::new(rect, label).text_size(13.0),
    );
    rect
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_is_clamped_and_anchor_is_fixed() {
        let mut zoom = ZoomControl::new();
        zoom.set_anchor(Point::new(50.0, 20.0));
        zoom.set(99.0);
        assert_eq!(zoom.factor(), MAX);
        assert_eq!(
            zoom.transform(Affine::IDENTITY) * zoom.anchor(),
            zoom.anchor()
        );
        zoom.set(0.0);
        assert_eq!(zoom.factor(), MIN);
    }

    #[test]
    fn coordinate_conversion_round_trips() {
        let mut zoom = ZoomControl::new();
        zoom.set_anchor(Point::new(100.0, 0.0));
        zoom.set(1.7);
        let content = 240.0;
        let screen = zoom.content_x_to_screen(content);
        assert!((zoom.screen_x_to_content(screen) - content).abs() < 1e-9);
    }
}
