//! Toolbar zoom control and page-scale transform.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

use ui::text_render::TextRenderer;

use crate::num;

pub(crate) const MIN: f64 = 0.5;
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
    pub(crate) const WIDTH: f64 = 88.0;

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

    #[cfg(test)]
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

    pub(crate) fn content_x_to_screen(&self, x: f64) -> f64 {
        self.anchor.x + (x - self.anchor.x) * self.factor
    }

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
        chip: Color,
        text_color: Color,
    ) {
        self.hits.clear();
        let center_y = f64::midpoint(row.y0, row.y1);
        let button_width = 18.0;
        let button_height = 18.0;
        let y = center_y - button_height / 2.0;
        let percent = format!("{}%", num::i32_of_f64((self.factor * 100.0).round()));
        let value_width = 44.0;
        let total = button_width * 2.0 + value_width + 8.0;
        let mut x = row.x1 - 14.0 - total;

        self.hits.push((
            Action::Out,
            draw_button(
                scene, transform, text, x, y, center_y, chip, text_color, "−",
            ),
        ));
        x += button_width + 4.0;
        let value = Rect::new(x, y, x + value_width, y + button_height);
        let percent_width = num::f64_of_usize(percent.chars().count()) * 6.8;
        text.draw_text(
            scene,
            &percent,
            12.5,
            text_color,
            transform
                * Affine::translate((x + (value_width - percent_width) / 2.0, center_y - 8.0)),
            None,
        );
        self.hits.push((Action::Reset, value));
        x += value_width + 4.0;
        self.hits.push((
            Action::In,
            draw_button(
                scene, transform, text, x, y, center_y, chip, text_color, "+",
            ),
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
    x: f64,
    y: f64,
    center_y: f64,
    chip: Color,
    text_color: Color,
    label: &str,
) -> Rect {
    let width = 18.0;
    let rect = Rect::new(x, y, x + width, y + 18.0);
    scene.fill(
        Fill::NonZero,
        transform,
        chip,
        None,
        &RoundedRect::from_rect(rect, 4.0),
    );
    let label_width = num::f64_of_usize(label.chars().count()) * 6.5;
    text.draw_text(
        scene,
        label,
        13.0,
        text_color,
        transform
            * Affine::translate((
                rect.x0 + (width - label_width).max(2.0) / 2.0,
                center_y - 8.0,
            )),
        None,
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
