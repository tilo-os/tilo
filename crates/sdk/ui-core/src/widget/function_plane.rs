//! [`FunctionPlane`] — the world ↔ screen mapping and grid of a pannable,
//! zoomable function plot (a grapher's `y = f(x)` canvas).
//!
//! Unlike [`crate::widget::CartesianPlot`], whose domain is fitted to its
//! data, a function plane is an unbounded world the user pans and zooms: the
//! axes run through the world origin wherever it lands, and the grid steps
//! follow a 1-2-5 progression so a label never reads `0.30000000000000004`.
//! The host samples its own curves through [`FunctionPlane::to_screen`] and
//! paints them over [`FunctionPlane::paint_grid`].

use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};

use crate::surface::edge_on_with;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;

/// Minor rules per major step.
const MINOR_DIVISIONS: u8 = 5;
/// Alphas of the minor rules, major rules and axes, each derived against the
/// canvas ground (HIG §9.5) so the grid reads on light, dark and
/// high-contrast grounds alike.
const MINOR_ALPHA: u8 = 18;
const MAJOR_ALPHA: u8 = 38;
const AXIS_ALPHA: u8 = 96;
const AXIS_WIDTH: f64 = 1.6;

/// A function plot's viewport: `center` is the world point under the canvas
/// centre and `scale` the pixels per world unit (y up in world space).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FunctionPlane {
    pub canvas: Rect,
    pub center: Point,
    pub scale: f64,
    /// Target on-screen distance between major grid rules, in logical
    /// pixels; the step itself snaps to the nearest 1-2-5 value.
    pub major_spacing: f64,
}

impl FunctionPlane {
    /// A plane whose major rules land roughly an inch (72 px) apart.
    pub fn new(canvas: Rect, center: Point, scale: f64) -> Self {
        Self {
            canvas,
            center,
            scale,
            major_spacing: 72.0,
        }
    }

    #[must_use]
    pub fn with_major_spacing(mut self, spacing: f64) -> Self {
        self.major_spacing = spacing;
        self
    }

    /// World → screen.
    pub fn to_screen(&self, world: Point) -> Point {
        let c = self.canvas.center();
        Point::new(
            c.x + (world.x - self.center.x) * self.scale,
            c.y - (world.y - self.center.y) * self.scale,
        )
    }

    /// Screen → world.
    pub fn to_world(&self, screen: Point) -> Point {
        let c = self.canvas.center();
        Point::new(
            self.center.x + (screen.x - c.x) / self.scale,
            self.center.y - (screen.y - c.y) / self.scale,
        )
    }

    /// The major grid step in world units: the 1-2-5 step nearest to
    /// [`Self::major_spacing`] on screen at the current zoom.
    pub fn major_step(&self) -> f64 {
        nice_step(self.major_spacing / self.scale)
    }

    /// Paint the grid clipped to the canvas: minor and major rules, the axes
    /// through the world origin (when it is on screen), and numeric labels at
    /// major steps in `Caption` tabular figures. `ground` is the canvas fill
    /// the rules derive their edge against. A label that would touch its
    /// neighbour is skipped, and every label stays whole inside the canvas.
    pub fn paint_grid(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        ground: Color,
    ) {
        let step = self.major_step();
        let top_left = self.to_world(Point::new(self.canvas.x0, self.canvas.y0));
        let bottom_right = self.to_world(Point::new(self.canvas.x1, self.canvas.y1));
        let span = WorldSpan {
            left: top_left.x,
            right: bottom_right.x,
            bottom: bottom_right.y,
            top: top_left.y,
        };

        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &self.canvas,
        );
        self.paint_rules(
            scene,
            transform,
            edge_on_with(ground, MINOR_ALPHA),
            span,
            step / f64::from(MINOR_DIVISIONS),
        );
        self.paint_rules(
            scene,
            transform,
            edge_on_with(ground, MAJOR_ALPHA),
            span,
            step,
        );
        let origin = self.paint_axes(scene, transform, edge_on_with(ground, AXIS_ALPHA));
        self.paint_labels(scene, transform, text_renderer, theme, span, (step, origin));
        scene.pop_layer();
    }

    fn paint_rules(
        &self,
        scene: &mut Scene,
        transform: Affine,
        color: Color,
        span: WorldSpan,
        spacing: f64,
    ) {
        let stroke = Stroke::new(1.0);
        for k in step_indices(span.left, span.right, spacing) {
            let x = self.to_screen(Point::new(k as f64 * spacing, 0.0)).x;
            scene.stroke(
                &stroke,
                transform,
                color,
                None,
                &Line::new((x, self.canvas.y0), (x, self.canvas.y1)),
            );
        }
        for k in step_indices(span.bottom, span.top, spacing) {
            let y = self.to_screen(Point::new(0.0, k as f64 * spacing)).y;
            scene.stroke(
                &stroke,
                transform,
                color,
                None,
                &Line::new((self.canvas.x0, y), (self.canvas.x1, y)),
            );
        }
    }

    /// The axes through the world origin, each only while it is on screen.
    /// Returns the origin in screen space for the label pass.
    fn paint_axes(&self, scene: &mut Scene, transform: Affine, color: Color) -> Point {
        let origin = self.to_screen(Point::ZERO);
        let stroke = Stroke::new(AXIS_WIDTH);
        if (self.canvas.y0..=self.canvas.y1).contains(&origin.y) {
            scene.stroke(
                &stroke,
                transform,
                color,
                None,
                &Line::new((self.canvas.x0, origin.y), (self.canvas.x1, origin.y)),
            );
        }
        if (self.canvas.x0..=self.canvas.x1).contains(&origin.x) {
            scene.stroke(
                &stroke,
                transform,
                color,
                None,
                &Line::new((origin.x, self.canvas.y0), (origin.x, self.canvas.y1)),
            );
        }
        origin
    }

    /// Tick labels beside each axis at the major step, a step of the theme's
    /// spacing off the axis; an axis scrolled off screen pins its labels
    /// inside the nearest canvas edge.
    fn paint_labels(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        span: WorldSpan,
        (step, origin): (f64, Point),
    ) {
        let style = theme.text(TextRole::Caption).numeric();
        let color = theme.tokens.colors.text_dim;
        for placed in self.label_layout(text_renderer, theme, span, (step, origin)) {
            text_renderer.draw_style(
                scene,
                &placed.text,
                color,
                transform * Affine::translate((placed.rect.x0, placed.rect.y0)),
                None,
                &style,
            );
        }
    }

    /// Where every tick label goes: x labels under the x axis, y labels
    /// right of the y axis just above their rule, each clamped whole inside
    /// the canvas on both sides, and a label that would come within a
    /// spacing step of the one before it skipped.
    fn label_layout(
        &self,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        span: WorldSpan,
        (step, origin): (f64, Point),
    ) -> Vec<TickLabel> {
        let style = theme.text(TextRole::Caption).numeric();
        let gap = f64::from(theme.tokens.spacing.s1);
        let inset = gap / 2.0;
        let line_h = f64::from(text_renderer.measure_style("0", None, &style).1);
        let canvas = self.canvas;
        let mut out = Vec::new();
        let place = |text: String, x: f64, top: f64, text_renderer: &mut TextRenderer| {
            let w = f64::from(text_renderer.measure_style(&text, None, &style).0);
            let x = x.clamp(
                canvas.x0 + inset,
                (canvas.x1 - w - inset).max(canvas.x0 + inset),
            );
            let top = top.clamp(
                canvas.y0 + inset,
                (canvas.y1 - line_h - inset).max(canvas.y0 + inset),
            );
            TickLabel {
                text,
                rect: Rect::new(x, top, x + w, top + line_h),
            }
        };
        // x labels hang under the x axis, left to right.
        let x_top = origin.y + gap;
        let mut last: Option<Rect> = None;
        for k in step_indices(span.left, span.right, step) {
            if k == 0 {
                continue;
            }
            let wx = k as f64 * step;
            let x = self.to_screen(Point::new(wx, 0.0)).x;
            let label = place(format_tick(wx, step), x + inset, x_top, text_renderer);
            if !canvas.contains(Point::new(label.rect.center().x, label.rect.y0 + inset))
                || last.is_some_and(|prev| label.rect.x0 < prev.x1 + gap)
            {
                continue;
            }
            last = Some(label.rect);
            out.push(label);
        }
        // y labels sit right of the y axis, just above their rule, top to
        // bottom.
        let y_left = origin.x + gap;
        let mut last: Option<Rect> = None;
        let mut ys: Vec<i64> = step_indices(span.bottom, span.top, step).collect();
        ys.reverse();
        for k in ys {
            if k == 0 {
                continue;
            }
            let wy = k as f64 * step;
            let y = self.to_screen(Point::new(0.0, wy)).y;
            let label = place(format_tick(wy, step), y_left, y - line_h, text_renderer);
            if !(canvas.y0..=canvas.y1).contains(&y)
                || last.is_some_and(|prev| label.rect.y0 < prev.y1 + gap)
                || out
                    .iter()
                    .any(|x_label: &TickLabel| overlaps(x_label.rect, label.rect, gap))
            {
                continue;
            }
            last = Some(label.rect);
            out.push(label);
        }
        out
    }
}

/// One placed tick label.
#[derive(Clone, Debug, PartialEq)]
struct TickLabel {
    text: String,
    rect: Rect,
}

fn overlaps(a: Rect, b: Rect, gap: f64) -> bool {
    a.x0 < b.x1 + gap && b.x0 < a.x1 + gap && a.y0 < b.y1 + gap && b.y0 < a.y1 + gap
}

/// The world-space rectangle the canvas currently shows.
#[derive(Clone, Copy)]
struct WorldSpan {
    left: f64,
    right: f64,
    bottom: f64,
    top: f64,
}

/// Inclusive integer multipliers `k` such that `k * step` covers `lo..=hi`,
/// one step of slack either side. Bounded so a degenerate zoom can never
/// ask for millions of rules.
fn step_indices(lo: f64, hi: f64, step: f64) -> impl Iterator<Item = i64> {
    // `as` saturates a non-finite or huge quotient; the integer steps
    // saturate too, so a zoom far past `f64`'s useful range cannot overflow.
    let kmin = ((lo / step).floor() as i64).saturating_sub(1);
    let kmax = ((hi / step).ceil() as i64).saturating_add(1);
    let span = kmax.saturating_sub(kmin).clamp(0, 100_000);
    (0..=span).map(move |i| kmin.saturating_add(i))
}

/// The 1-2-5 "nice" step nearest above `raw` world units. Non-finite or
/// non-positive input falls back to `1.0`.
pub fn nice_step(raw: f64) -> f64 {
    if !raw.is_finite() || raw <= 0.0 {
        return 1.0;
    }
    let magnitude = 10f64.powf(raw.log10().floor());
    let normalised = raw / magnitude;
    let nice = if normalised < 1.5 {
        1.0
    } else if normalised < 3.0 {
        2.0
    } else if normalised < 7.0 {
        5.0
    } else {
        10.0
    };
    nice * magnitude
}

/// Axis ticks on a 1-2-5 step inside `[min, max]`: at most `max_count`, at
/// least two, every one a round number at its step's precision, so an axis
/// reads 0, 10, 20, 30 instead of 0, 11.5, 23, 34.5. A range too narrow or
/// too odd for two round ticks gets its two ends.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "tick indices are small whole numbers bounded by max_count"
)]
pub fn nice_ticks(min: f64, max: f64, max_count: usize) -> Vec<f64> {
    let ends = || {
        if min.is_finite() && max.is_finite() {
            vec![min, max]
        } else {
            Vec::new()
        }
    };
    if !min.is_finite() || !max.is_finite() || max <= min || max_count < 2 {
        return ends();
    }
    let mut step = nice_step((max - min) / (max_count - 1) as f64);
    for _ in 0..8 {
        let first = (min / step - 1e-9).ceil();
        let last = (max / step + 1e-9).floor();
        let count = last - first + 1.0;
        if count > max_count as f64 {
            step = nice_step(step * 1.6);
            continue;
        }
        if count < 2.0 {
            return ends();
        }
        let decimals = if step >= 1.0 {
            0
        } else {
            (-step.log10().floor()).clamp(0.0, 12.0) as i32
        };
        let scale = 10f64.powi(decimals);
        return (0..count as i64)
            .map(|k| (((first + k as f64) * step) * scale).round() / scale)
            .collect();
    }
    ends()
}

/// A tick value at the precision its `step` implies, without trailing
/// zeros: `0.5`, `2`, `-0.25`, and exactly `0` at the origin.
pub fn format_tick(value: f64, step: f64) -> String {
    if value.abs() < step * 1e-6 {
        return "0".to_string();
    }
    let decimals = if step >= 1.0 {
        0
    } else {
        (-step.log10().floor()) as usize
    };
    let text = format!("{value:.decimals$}");
    if decimals == 0 {
        text
    } else {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_ticks_are_round_numbers_inside_the_range() {
        assert_eq!(nice_ticks(0.0, 46.0, 5), vec![0.0, 10.0, 20.0, 30.0, 40.0]);
        assert_eq!(nice_ticks(-0.5, 12.5, 6), vec![0.0, 5.0, 10.0]);
        assert_eq!(nice_ticks(-0.3, 5.3, 4), vec![0.0, 2.0, 4.0]);
        assert_eq!(
            nice_ticks(38.48, 60.52, 5),
            vec![40.0, 45.0, 50.0, 55.0, 60.0]
        );
        assert_eq!(nice_ticks(0.1, 0.7, 4), vec![0.2, 0.4, 0.6]);
        for (min, max, count) in [(0.0, 46.0, 5), (3.0, 3.2, 3), (-7.0, 1e6, 6)] {
            let ticks = nice_ticks(min, max, count);
            assert!(
                ticks.len() >= 2 && ticks.len() <= count,
                "{min}..{max}: {ticks:?}"
            );
            assert!(ticks.iter().all(|t| *t >= min && *t <= max), "{ticks:?}");
        }
        assert_eq!(nice_ticks(1.0, 1.0, 5), vec![1.0, 1.0]);
    }

    #[test]
    fn nice_steps_follow_one_two_five() {
        assert_eq!(nice_step(1.1), 1.0);
        assert_eq!(nice_step(0.018), 0.02);
        assert_eq!(nice_step(4.0), 5.0);
        assert_eq!(nice_step(80.0), 100.0);
        assert_eq!(nice_step(f64::NAN), 1.0);
        assert_eq!(nice_step(-3.0), 1.0);
    }

    #[test]
    fn ticks_print_at_step_precision() {
        assert_eq!(format_tick(0.30000000000000004, 0.1), "0.3");
        assert_eq!(format_tick(2.0, 1.0), "2");
        assert_eq!(format_tick(1e-12, 0.5), "0");
        assert_eq!(format_tick(-0.25, 0.05), "-0.25");
    }

    #[test]
    fn world_and_screen_round_trip() {
        let plane = FunctionPlane::new(
            Rect::new(0.0, 0.0, 400.0, 200.0),
            Point::new(1.0, -2.0),
            50.0,
        );
        assert_eq!(
            plane.to_screen(Point::new(1.0, -2.0)),
            Point::new(200.0, 100.0)
        );
        let world = Point::new(3.5, 0.25);
        let back = plane.to_world(plane.to_screen(world));
        assert!((back - world).hypot() < 1e-9);
        // World y grows upward on screen.
        assert!(plane.to_screen(Point::new(1.0, 0.0)).y < 100.0);
    }

    #[test]
    fn tick_labels_never_collide_and_stay_whole_inside_the_canvas() {
        let theme = Theme::dark().with_text_scale(2.0);
        let mut text = TextRenderer::new();
        // Zoomed out so labels are wide ("-2000") against a tight step, with
        // the y axis near the left edge and negatives on screen.
        let plane = FunctionPlane::new(
            Rect::new(0.0, 0.0, 360.0, 240.0),
            Point::new(1500.0, 0.0),
            0.09,
        )
        .with_major_spacing(40.0);
        let step = plane.major_step();
        let top_left = plane.to_world(Point::new(0.0, 0.0));
        let bottom_right = plane.to_world(Point::new(360.0, 240.0));
        let span = WorldSpan {
            left: top_left.x,
            right: bottom_right.x,
            bottom: bottom_right.y,
            top: top_left.y,
        };
        let origin = plane.to_screen(Point::ZERO);
        let labels = plane.label_layout(&mut text, &theme, span, (step, origin));
        assert!(!labels.is_empty());
        let gap = f64::from(theme.tokens.spacing.s1);
        for (i, a) in labels.iter().enumerate() {
            assert!(
                a.rect.x0 >= plane.canvas.x0 && a.rect.x1 <= plane.canvas.x1,
                "{a:?} is cut by the canvas"
            );
            for b in labels.iter().skip(i + 1) {
                assert!(!overlaps(a.rect, b.rect, gap - 0.01), "{a:?} touches {b:?}");
            }
        }
        // A negative label keeps its minus sign whole on the leading edge.
        let negative = labels.iter().find(|l| l.text.starts_with('-'));
        if let Some(negative) = negative {
            assert!(negative.rect.x0 >= plane.canvas.x0);
        }
    }

    #[test]
    fn a_degenerate_zoom_is_bounded() {
        assert!(step_indices(-1e300, 1e300, 1e-300).count() <= 100_001);
    }
}
