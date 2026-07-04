//! Live-looking throughput model + dual sparkline renderer.
//!
//! The rate series are generated deterministically from a time phase so
//! the chart is fully populated on a single offscreen frame (`--shots`)
//! yet animates smoothly in the live loop. Nothing here touches the
//! network — it shapes a believable waveform around each connection's
//! baseline rate.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use crate::model::human_rate;

/// Number of points plotted across the chart.
pub const SAMPLES: usize = 56;
/// Seconds between samples (controls horizontal time window).
const DT: f64 = 0.25;

/// Generates a believable transfer-rate series in **bytes/sec**.
#[derive(Clone, Copy, Debug)]
pub struct ThroughputModel {
    /// Per-direction salt so up/down don't move in lockstep.
    salt: f64,
}

impl ThroughputModel {
    pub fn new(salt: f64) -> Self {
        Self { salt }
    }

    /// Smooth pseudo-random scalar in roughly `[0,1]` for a continuous time.
    fn wave(&self, t: f64) -> f64 {
        let s = self.salt;
        let base = 0.5
            + 0.26 * (t * 0.9 + s).sin()
            + 0.14 * (t * 2.3 + s * 2.0).sin()
            + 0.08 * (t * 5.1 + s * 0.7).sin();
        base.clamp(0.04, 1.0)
    }

    /// The full series ending "now". `base_mbps` is the mean rate; the
    /// returned values are bytes/sec. `now_secs` lets the window scroll.
    pub fn series(&self, base_mbps: f64, now_secs: f64) -> Vec<f64> {
        let base_bps = base_mbps * 1024.0 * 1024.0;
        (0..SAMPLES)
            .map(|i| {
                let age = (SAMPLES - 1 - i) as f64 * DT;
                let t = now_secs - age;
                base_bps * self.wave(t)
            })
            .collect()
    }

    /// The current (most-recent) rate in bytes/sec.
    pub fn current(&self, base_mbps: f64, now_secs: f64) -> f64 {
        base_mbps * 1024.0 * 1024.0 * self.wave(now_secs)
    }
}

/// Draw a dual-series area sparkline (download + upload) inside `rect`.
///
/// Colours are pulled from the theme: `down` uses the info/accent blue,
/// `up` uses the success green. A faint card + gridlines anchor the plot.
#[allow(clippy::too_many_arguments)]
pub fn draw_chart(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    theme: &crate::Theme,
    down: &[f64],
    up: &[f64],
    connected: bool,
) {
    let radius = theme.tokens.radii.lg as f64;
    let card = RoundedRect::from_rect(rect, radius);
    scene.fill(Fill::NonZero, transform, theme.tokens.colors.surfaces.base, None, &card);
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        theme.tokens.colors.border_white_10,
        None,
        &card,
    );

    // Plot inset.
    let pad = 12.0;
    let plot = Rect::new(rect.x0 + pad, rect.y0 + pad, rect.x1 - pad, rect.y1 - pad);
    if plot.width() <= 1.0 || plot.height() <= 1.0 {
        return;
    }

    // Horizontal gridlines (3 divisions).
    let grid_color = theme.tokens.colors.border_white_10.multiply_alpha(0.6);
    for k in 0..=3 {
        let y = plot.y0 + plot.height() * (k as f64 / 3.0);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            grid_color,
            None,
            &Line::new(Point::new(plot.x0, y), Point::new(plot.x1, y)),
        );
    }

    if !connected {
        return;
    }

    // Shared scale so the two series are comparable.
    let peak = down
        .iter()
        .chain(up.iter())
        .cloned()
        .fold(0.0_f64, f64::max)
        .max(1.0);
    let scale = peak * 1.15;

    let down_color = theme.tokens.colors.semantic.info;
    let up_color = theme.tokens.colors.semantic.success;
    plot_series(scene, transform, plot, down, scale, down_color);
    plot_series(scene, transform, plot, up, scale, up_color);
}

/// Plot one filled-area line series within `plot`.
fn plot_series(
    scene: &mut Scene,
    transform: Affine,
    plot: Rect,
    series: &[f64],
    scale: f64,
    color: Color,
) {
    if series.len() < 2 {
        return;
    }
    let n = series.len();
    let x_at = |i: usize| plot.x0 + plot.width() * (i as f64 / (n - 1) as f64);
    let y_at = |v: f64| plot.y1 - (v / scale).clamp(0.0, 1.0) * plot.height();

    // Filled area under the curve.
    let mut area = BezPath::new();
    area.move_to(Point::new(x_at(0), plot.y1));
    for (i, &v) in series.iter().enumerate() {
        area.line_to(Point::new(x_at(i), y_at(v)));
    }
    area.line_to(Point::new(x_at(n - 1), plot.y1));
    area.close_path();
    scene.fill(Fill::NonZero, transform, color.multiply_alpha(0.16), None, &area);

    // Stroked line on top.
    let mut line = BezPath::new();
    line.move_to(Point::new(x_at(0), y_at(series[0])));
    for (i, &v) in series.iter().enumerate().skip(1) {
        line.line_to(Point::new(x_at(i), y_at(v)));
    }
    scene.stroke(&Stroke::new(2.0), transform, color, None, &line);

    // Highlight dot at the leading edge.
    let last = Point::new(x_at(n - 1), y_at(series[n - 1]));
    scene.fill(
        Fill::NonZero,
        transform,
        color,
        None,
        &vello::kurbo::Circle::new(last, 3.0),
    );
}

/// Format a rate for the legend chip.
pub fn rate_label(bytes_per_sec: f64) -> String {
    human_rate(bytes_per_sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_is_populated_and_bounded() {
        let m = ThroughputModel::new(1.0);
        let s = m.series(6.4, 12.0);
        assert_eq!(s.len(), SAMPLES);
        // All non-negative, and the mean is within a believable band of base.
        let base_bps = 6.4 * 1024.0 * 1024.0;
        let mean: f64 = s.iter().sum::<f64>() / s.len() as f64;
        assert!(s.iter().all(|&v| v >= 0.0));
        assert!(mean > base_bps * 0.2 && mean < base_bps * 1.2, "mean={mean}");
    }

    #[test]
    fn current_is_deterministic_for_same_time() {
        let m = ThroughputModel::new(2.0);
        assert_eq!(m.current(3.0, 5.0), m.current(3.0, 5.0));
    }
}
