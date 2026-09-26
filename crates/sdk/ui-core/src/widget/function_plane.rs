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
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;

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
    /// every major step. `ground` is the canvas fill the rules derive their
    /// edge against; `label_size` is the renderer-relative size the host sets
    /// its other canvas text at.
    pub fn paint_grid(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        ground: Color,
        label_size: f32,
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
        self.paint_labels(
            scene,
            transform,
            text_renderer,
            theme,
            label_size,
            span,
            (step, origin),
        );
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
        size: f32,
        span: WorldSpan,
        (step, origin): (f64, Point),
    ) {
        let color = theme.tokens.colors.text_dim;
        let gap = f64::from(theme.tokens.spacing.s1);
        let inset = gap / 2.0;
        let (_, line_h) = text_renderer.measure_weighted("0", size, None, TextWeight::Regular);
        let line_h = f64::from(line_h);
        let mut label = |scene: &mut Scene, text: &str, x: f64, top: f64| {
            let (w, _) = text_renderer.measure_weighted(text, size, None, TextWeight::Regular);
            let x = x.min(self.canvas.x1 - f64::from(w) - inset);
            text_renderer.draw_text_weighted(
                scene,
                text,
                size,
                color,
                transform * Affine::translate((x, top)),
                None,
                TextWeight::Regular,
            );
        };
        // x labels hang under the x axis.
        let x_top = (origin.y + gap).clamp(self.canvas.y0 + inset, self.canvas.y1 - line_h - inset);
        for k in step_indices(span.left, span.right, step) {
            if k == 0 {
                continue;
            }
            let wx = k as f64 * step;
            let x = self.to_screen(Point::new(wx, 0.0)).x;
            label(scene, &format_tick(wx, step), x + inset, x_top);
        }
        // y labels sit right of the y axis, just above their rule.
        let y_left = (origin.x + gap).max(self.canvas.x0 + inset);
        for k in step_indices(span.bottom, span.top, step) {
            if k == 0 {
                continue;
            }
            let wy = k as f64 * step;
            let y = self.to_screen(Point::new(0.0, wy)).y;
            label(scene, &format_tick(wy, step), y_left, y - line_h);
        }
    }
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
    fn a_degenerate_zoom_is_bounded() {
        assert!(step_indices(-1e300, 1e300, 1e-300).count() <= 100_001);
    }
}
