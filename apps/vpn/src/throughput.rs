//! Dual sparkline renderer for REAL throughput.
//!
//! This module only *plots* rate series it is handed; it never generates
//! them. The series come from real, measured kernel byte counters
//! (`wg show <iface> transfer` / `/sys/class/net/<iface>/statistics/`),
//! differenced over time in [`crate::app`]. Nothing here fabricates a
//! waveform — an empty slice draws an empty (honest) chart.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Line, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

/// Maximum number of measured points retained / plotted across the chart.
pub const SAMPLES: usize = 56;

/// Draw a dual-series area sparkline (download + upload) inside `rect`.
///
/// Colours are pulled from the theme: `down` uses the info/accent blue,
/// `up` uses the success green. A faint card + gridlines anchor the plot.
#[allow(
    clippy::too_many_arguments,
    reason = "throughput painting combines scene, transform, bounds and theme with both data series and connection state"
)]
pub fn draw_chart(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    theme: &crate::Theme,
    down: &[f64],
    up: &[f64],
    connected: bool,
) {
    let radius = f64::from(theme.tokens.radii.lg);
    // The plot is a raised card like the stat tiles above it, with the edge
    // derived from its own fill (HIG §9.5).
    ui::surface::card(
        scene,
        transform,
        rect,
        radius,
        theme.tokens.colors.surfaces.raised,
    );

    // Plot inset.
    let pad = 12.0;
    let plot = Rect::new(rect.x0 + pad, rect.y0 + pad, rect.x1 - pad, rect.y1 - pad);
    if plot.width() <= 1.0 || plot.height() <= 1.0 {
        return;
    }

    // Horizontal gridlines (3 divisions).
    let grid_color = ui::surface::edge_on(theme.tokens.colors.surfaces.raised).multiply_alpha(0.6);
    for k in 0..=3 {
        let y = plot.y0 + plot.height() * (f64::from(k) / 3.0);
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
        .copied()
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
    scene.fill(
        Fill::NonZero,
        transform,
        color.multiply_alpha(0.16),
        None,
        &area,
    );

    // Stroked line on top. Both `.first()`/`.last()` are unreachable `None`
    // here — the `series.len() < 2` guard above already ensures at least two
    // elements — but `.get()`-family documents that instead of indexing.
    let Some(&first) = series.first() else {
        return;
    };
    let mut line = BezPath::new();
    line.move_to(Point::new(x_at(0), y_at(first)));
    for (i, &v) in series.iter().enumerate().skip(1) {
        line.line_to(Point::new(x_at(i), y_at(v)));
    }
    scene.stroke(&Stroke::new(2.0), transform, color, None, &line);

    // Highlight dot at the leading edge.
    let Some(&last_val) = series.last() else {
        return;
    };
    let last = Point::new(x_at(n - 1), y_at(last_val));
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
    ui::format::rate(bytes_per_sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_label_formats_bytes_per_sec() {
        // base-1024 rate → IEC binary units.
        assert_eq!(rate_label(6_710_886.0), "6.4 MiB/s");
        assert_eq!(rate_label(0.0), "0 B/s");
    }

    #[test]
    fn empty_series_plots_without_panicking() {
        // An empty measured series must render a blank chart, never a curve.
        let mut scene = Scene::new();
        // A trivial theme suffices; draw_chart reads only tokens/colors.
        let theme = crate::Theme::default();
        let rect = Rect::new(0.0, 0.0, 200.0, 100.0);
        draw_chart(&mut scene, Affine::IDENTITY, rect, &theme, &[], &[], false);
    }
}
