//! Toolbar zoom control and page-scale transform.

use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use crate::num;
use crate::paint_context::PaintContext;
use ui::text_render::TextRenderer;

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
    metrics: Metrics,
}

#[derive(Clone, Copy)]
struct Metrics {
    button_width: f64,
    value_width: f64,
    height: f64,
    gap: f64,
    out_size: (f64, f64),
    value_size: (f64, f64),
    in_size: (f64, f64),
    radius: f64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            button_width: 18.0,
            value_width: 44.0,
            height: 18.0,
            gap: 4.0,
            out_size: (0.0, 0.0),
            value_size: (0.0, 0.0),
            in_size: (0.0, 0.0),
            radius: 4.0,
        }
    }
}

impl ZoomControl {
    /// Stable semantic targets for the three controls painted by this component.
    /// Keeping these beside the hit-testing means assistive technology and the
    /// pointer path cannot grow different zoom behaviours.
    pub(crate) const TARGET_OUT: &'static str = "word-processor.zoom:out";
    pub(crate) const TARGET_RESET: &'static str = "word-processor.zoom:reset";
    pub(crate) const TARGET_IN: &'static str = "word-processor.zoom:in";

    pub(crate) fn new() -> Self {
        Self {
            factor: 1.0,
            anchor: Point::ZERO,
            hits: Vec::new(),
            metrics: Metrics::default(),
        }
    }

    /// Refresh the toolbar reservation before its owner packs rows. The same
    /// measured geometry is used for painting and hit testing below.
    pub(crate) fn measure(&mut self, text: &mut TextRenderer) {
        let scale = f64::from(text.text_scale());
        let percent = self.percent();
        let dimensions = |size: (f32, f32)| (f64::from(size.0), f64::from(size.1));
        let out_size = dimensions(text.measure("−", 13.0, None));
        let in_size = dimensions(text.measure("+", 13.0, None));
        let value_size = dimensions(text.measure(&percent, 12.5, None));
        // Reserve a full three-digit percentage even at low zoom, preventing
        // ordinary zoom clicks from repacking the toolbar around the control.
        let widest_percent = ["100%", "200%", "300%"]
            .into_iter()
            .map(|label| f64::from(text.measure(label, 12.5, None).0))
            .fold(value_size.0, f64::max);
        self.metrics = Metrics {
            button_width: (18.0 * scale).max(out_size.0.max(in_size.0) + 4.0 * scale),
            value_width: (44.0 * scale).max(widest_percent + 8.0 * scale),
            height: (18.0 * scale).max(out_size.1.max(in_size.1).max(value_size.1)),
            gap: 4.0 * scale,
            out_size,
            value_size,
            in_size,
            radius: 4.0 * scale,
        };
    }

    pub(crate) fn width(&self) -> f64 {
        self.metrics.button_width * 2.0 + self.metrics.value_width + self.metrics.gap * 2.0
    }

    pub(crate) fn height(&self) -> f64 {
        self.metrics.height
    }

    fn percent(&self) -> String {
        format!("{}%", num::i32_of_f64((self.factor * 100.0).round()))
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
        self.apply(action)
    }

    /// Activate a painted zoom control by its stable accessibility target.
    /// This deliberately uses the same `apply` routine as pointer input.
    pub(crate) fn activate_target(&mut self, target: &str) -> bool {
        let action = match target {
            Self::TARGET_OUT => Action::Out,
            Self::TARGET_RESET => Action::Reset,
            Self::TARGET_IN => Action::In,
            _ => return false,
        };
        self.apply(action);
        true
    }

    /// The rendered hit bounds paired with their stable semantic target.
    /// Empty until the component has been drawn, just like the pointer targets.
    pub(crate) fn accessibility_targets(&self) -> Vec<(&'static str, Rect)> {
        self.hits
            .iter()
            .map(|(action, rect)| {
                let target = match action {
                    Action::Out => Self::TARGET_OUT,
                    Action::Reset => Self::TARGET_RESET,
                    Action::In => Self::TARGET_IN,
                };
                (target, *rect)
            })
            .collect()
    }

    fn apply(&mut self, action: Action) -> bool {
        match action {
            Action::Out => self.step(-STEP),
            Action::Reset => self.set(1.0),
            Action::In => self.step(STEP),
        }
    }

    pub(crate) fn draw(
        &mut self,
        context: &mut PaintContext<'_>,
        row: Rect,
        chip: Color,
        text_color: Color,
    ) {
        let transform = context.transform;
        self.hits.clear();
        self.measure(context.text);
        let metrics = self.metrics;
        let center_y = f64::midpoint(row.y0, row.y1);
        let y = center_y - metrics.height / 2.0;
        let mut x = row.x1 - 14.0 - self.width();
        let out = Rect::new(x, y, x + metrics.button_width, y + metrics.height);
        self.draw_button(context, out, Action::Out, chip, text_color);
        x += metrics.button_width + metrics.gap;
        let value = Rect::new(x, y, x + metrics.value_width, y + metrics.height);
        context.text.draw_text(
            context.scene,
            &self.percent(),
            12.5,
            text_color,
            transform
                * Affine::translate((
                    value.center().x - metrics.value_size.0 / 2.0,
                    center_y - metrics.value_size.1 / 2.0,
                )),
            None,
        );
        self.hits.push((Action::Reset, value));
        x += metrics.value_width + metrics.gap;
        let increase = Rect::new(x, y, x + metrics.button_width, y + metrics.height);
        self.draw_button(context, increase, Action::In, chip, text_color);
    }

    /// Paint the zoom-out or zoom-in chip in `rect` and record its hit target.
    fn draw_button(
        &mut self,
        context: &mut PaintContext<'_>,
        rect: Rect,
        action: Action,
        chip: Color,
        text_color: Color,
    ) {
        let (label, label_size) = if action == Action::Out {
            ("−", self.metrics.out_size)
        } else {
            ("+", self.metrics.in_size)
        };
        // The chip face with its derived rest edge (HIG §9.5): the fill alone is
        // the bar's own tone and disappeared on the dark preset.
        ui::surface::card(
            context.scene,
            context.transform,
            rect,
            self.metrics.radius,
            chip,
        );
        context.text.draw_text(
            context.scene,
            label,
            13.0,
            text_color,
            context.transform
                * Affine::translate((
                    rect.center().x - label_size.0 / 2.0,
                    rect.center().y - label_size.1 / 2.0,
                )),
            None,
        );
        self.hits.push((action, rect));
    }

    #[cfg(test)]
    pub(crate) fn hit_rects(&self) -> Vec<Rect> {
        self.hits.iter().map(|(_, rect)| *rect).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::Scene;

    /// Scaled zoom labels fit their measured hit targets and scaled targets keep out/reset/in behaviour.
    #[test]
    fn zoom_labels_and_targets_fit_their_measured_hit_areas() {
        // Phase: scaled_zoom_labels_fit_their_measured_hit_targets.
        {
            for scale in [1.0, 2.0] {
                for factor in [0.55, 1.0, 3.0] {
                    let mut zoom = ZoomControl::new();
                    zoom.set(factor);
                    let mut text = TextRenderer::new();
                    text.set_text_scale(scale);
                    let row = Rect::new(0.0, 0.0, 480.0, 64.0);
                    zoom.draw(
                        &mut PaintContext::new(&mut Scene::new(), Affine::IDENTITY, &mut text),
                        row,
                        Color::BLACK,
                        Color::WHITE,
                    );
                    let percent = format!("{}%", num::i32_of_f64((factor * 100.0).round()));
                    for ((_, rect), (label, size)) in
                        zoom.hits
                            .iter()
                            .zip([("−", 13.0), (percent.as_str(), 12.5), ("+", 13.0)])
                    {
                        let (width, height) = text.measure(label, size, None);
                        assert!(
                            rect.width() >= f64::from(width),
                            "{label} at {scale}× needs width {width}, got {rect:?}"
                        );
                        assert!(
                            rect.height() >= f64::from(height),
                            "{label} at {scale}× needs height {height}, got {rect:?}"
                        );
                        assert_eq!(rect.intersect(row), *rect);
                    }
                    for pair in zoom.hits.windows(2) {
                        assert!(
                            pair[0].1.x1 < pair[1].1.x0,
                            "adjacent targets must not overlap"
                        );
                    }
                }
            }
        }

        // Phase: scaled_zoom_targets_keep_out_reset_in_behavior.
        {
            for scale in [1.0, 2.0] {
                let mut zoom = ZoomControl::new();
                zoom.set(1.2);
                let mut text = TextRenderer::new();
                text.set_text_scale(scale);
                zoom.draw(
                    &mut PaintContext::new(&mut Scene::new(), Affine::IDENTITY, &mut text),
                    Rect::new(0.0, 0.0, 480.0, 64.0),
                    Color::BLACK,
                    Color::WHITE,
                );
                let hits = zoom.hit_rects();
                assert!(zoom.click(hits[0].center()));
                assert!((zoom.factor() - 1.1).abs() < 1e-9);
                assert!(zoom.click(hits[2].center()));
                assert!((zoom.factor() - 1.2).abs() < 1e-9);
                assert!(zoom.click(hits[1].center()));
                assert_eq!(zoom.factor(), 1.0);
                assert!(!zoom.click(Point::new(0.0, 0.0)));
            }
        }
    }

    /// The scale is clamped with the anchor fixed, and coordinate conversion round-trips.
    #[test]
    fn zoom_scale_is_clamped_about_a_fixed_anchor_and_converts_back() {
        // Phase: scale_is_clamped_and_anchor_is_fixed.
        {
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

        // Phase: coordinate_conversion_round_trips.
        {
            let mut zoom = ZoomControl::new();
            zoom.set_anchor(Point::new(100.0, 0.0));
            zoom.set(1.7);
            let content = 240.0;
            let screen = zoom.content_x_to_screen(content);
            assert!((zoom.screen_x_to_content(screen) - content).abs() < 1e-9);
        }
    }
}
