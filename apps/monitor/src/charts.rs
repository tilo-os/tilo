//! Custom Vello drawing primitives for the monitor: rounded panels,
//! rolling area charts, radial ring gauges, horizontal bar gauges and
//! sparklines. Everything reads its colours from the passed [`Theme`] so
//! the OS owns the palette; nothing here hard-codes a brand colour.

use std::f64::consts::FRAC_PI_2;

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Arc, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

/// Same colour with a replaced 8-bit alpha. Lets us tint theme colours
/// for fills/grids without depending on `ui`'s internal colour-compat
/// trait.
pub fn with_alpha(c: Color, a: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), a)
}

/// Theme type sizes (`theme.tokens.typography.scale.*`) already carry the
/// accessibility text scale, and the legacy size-based renderer APIs
/// (`measure`, `draw_text*`, `ButtonFace::text_size`) apply that scale once
/// more. Route every token through here so the multiplier lands exactly once:
/// 2× glyphs at 200 %, not 4×. Identity at 1×.
pub fn ts(tr: &TextRenderer, resolved: f32) -> f32 {
    tr.renderer_input_style(&TextStyle::new(resolved)).size
}

/// Measured line height of a token size as the renderer will paint it —
/// the single number the app's text-bearing rows derive their pitch from.
pub fn line_h(tr: &mut TextRenderer, resolved: f32) -> f64 {
    line_box(tr, ts(tr, resolved))
}

/// Measured line box of an already renderer-relative `size` (the value the
/// app passes to `draw_text*`). Vertical steps between stacked lines and
/// centring inside a band use this, never the bare font size: cosmic-text
/// lays a legacy line out at `1.25 × size`, and the gap grows with the
/// accessibility scale.
pub fn line_box(tr: &mut TextRenderer, size: f32) -> f64 {
    let (_, h) = tr.measure("0", size, None);
    f64::from(h)
}

pub fn fill_rrect(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, color: Color) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    scene.fill(
        Fill::NonZero,
        t,
        color,
        None,
        &RoundedRect::from_rect(rect, radius),
    );
}

pub fn stroke_rrect(
    scene: &mut Scene,
    t: Affine,
    rect: Rect,
    radius: f64,
    color: Color,
    width: f64,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    scene.stroke(
        &Stroke::new(width),
        t,
        color,
        None,
        &RoundedRect::from_rect(rect, radius),
    );
}

/// Draw a raised card with its derived rest edge (HIG §7, §9.5) and return
/// the padded inner content rect. Routed through [`ui::surface::card`] so the
/// edge is measured against the card itself — the declared `border_standard`
/// it replaced is three shades from the dark `raised` tier and vanished.
pub fn panel(scene: &mut Scene, t: Affine, rect: Rect, theme: &Theme, pad: f64) -> Rect {
    let radius = f64::from(theme.tokens.radii.md) + 2.0;
    ui::surface::card(scene, t, rect, radius, theme.tokens.colors.surfaces.raised);
    Rect::new(rect.x0 + pad, rect.y0 + pad, rect.x1 - pad, rect.y1 - pad)
}

/// A faint rule or track *inside* a raised card (chart grid, gauge track,
/// scrollbar groove): the card's hairline polarity at `alpha`, so it reads on
/// dark and light alike instead of a declared border at a fixed alpha.
pub fn inner_rule(theme: &Theme, alpha: u8) -> Color {
    ui::surface::edge_on_with(theme.tokens.colors.surfaces.raised, alpha)
}

/// A single data series for [`area_chart`].
pub struct Series<'a> {
    pub data: &'a [f32],
    pub color: Color,
    pub legend: String,
}

/// A rolling filled line/area chart inside a titled panel.
///
/// `max` is the y-axis ceiling; pass the data's own max (with headroom)
/// for unbounded units like network throughput, or a fixed `100.0` for a
/// percentage chart. `axis_hi`/`axis_lo` are right-aligned tick labels.
#[allow(
    clippy::too_many_arguments,
    reason = "area charts combine paint context and bounds with title, series, numeric ceiling and formatted axis labels"
)]
pub fn area_chart(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    rect: Rect,
    theme: &Theme,
    title: &str,
    series: &[Series],
    max: f32,
    axis_hi: &str,
    axis_lo: &str,
) {
    let colors = &theme.tokens.colors;
    let inner = panel(scene, t, rect, theme, 12.0);

    // Header row: title left, legend (coloured dot + label) right.
    tr.draw_text_weighted(
        scene,
        title,
        ts(tr, theme.tokens.typography.scale.sm),
        colors.text_primary,
        t * Affine::translate((inner.x0, inner.y0)),
        None,
        TextWeight::Semibold,
    );
    let mut lx = inner.x1;
    for s in series.iter().rev() {
        let (lw, _) = tr.measure(&s.legend, ts(tr, theme.tokens.typography.scale.xs), None);
        lx -= f64::from(lw);
        tr.draw_text(
            scene,
            &s.legend,
            ts(tr, theme.tokens.typography.scale.xs),
            colors.text_dim,
            t * Affine::translate((lx, inner.y0 + 1.0)),
            None,
        );
        lx -= 8.0;
        let dot = Circle::new(Point::new(lx - 3.0, inner.y0 + 6.0), 3.0);
        scene.fill(Fill::NonZero, t, s.color, None, &dot);
        lx -= 14.0;
    }

    // Plot area sits below the header.
    let plot = Rect::new(inner.x0, inner.y0 + 22.0, inner.x1, inner.y1);
    // Grid: four horizontal rules.
    let grid = inner_rule(theme, 26);
    for i in 0..=4 {
        let y = plot.y0 + plot.height() * (f64::from(i) / 4.0);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            grid,
            None,
            &Line::new((plot.x0, y), (plot.x1, y)),
        );
    }
    // Axis tick labels (top + bottom of plot, right edge).
    tr.draw_text(
        scene,
        axis_hi,
        ts(tr, theme.tokens.typography.scale.xs),
        colors.text_dim,
        t * Affine::translate((plot.x1 - 46.0, plot.y0 - 1.0)),
        None,
    );
    tr.draw_text(
        scene,
        axis_lo,
        ts(tr, theme.tokens.typography.scale.xs),
        colors.text_dim,
        t * Affine::translate((plot.x1 - 46.0, plot.y1 - 12.0)),
        None,
    );

    let ceil = max.max(1e-3);
    for s in series {
        if s.data.len() < 2 {
            continue;
        }
        let n = s.data.len();
        let dx = plot.width() / (n as f64 - 1.0);
        let map = |i: usize, v: f32| -> Point {
            let x = plot.x0 + dx * i as f64;
            let frac = f64::from((v / ceil).clamp(0.0, 1.0));
            let y = plot.y1 - frac * plot.height();
            Point::new(x, y)
        };
        // Filled area.
        let mut area = BezPath::new();
        area.move_to(Point::new(plot.x0, plot.y1));
        for (i, &v) in s.data.iter().enumerate() {
            area.line_to(map(i, v));
        }
        area.line_to(Point::new(plot.x1, plot.y1));
        area.close_path();
        scene.fill(Fill::NonZero, t, with_alpha(s.color, 46), None, &area);
        // Top line.
        let mut line = BezPath::new();
        if let Some(&first) = s.data.first() {
            line.move_to(map(0, first));
        }
        for (i, &v) in s.data.iter().enumerate().skip(1) {
            line.line_to(map(i, v));
        }
        scene.stroke(&Stroke::new(1.8), t, s.color, None, &line);
    }
}

/// A radial ring gauge. `frac` is 0..=1; draws a track ring plus an arc
/// that sweeps clockwise from 12 o'clock, with a big value and a small
/// caption centred inside.
#[allow(
    clippy::too_many_arguments,
    reason = "ring gauges combine paint context, center/radius/thickness, fraction and color with value and caption text"
)]
pub fn ring_gauge(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    center: Point,
    radius: f64,
    thickness: f64,
    frac: f32,
    color: Color,
    theme: &Theme,
    big: &str,
    small: &str,
) {
    let colors = &theme.tokens.colors;
    // Track.
    scene.stroke(
        &Stroke::new(thickness),
        t,
        inner_rule(theme, 56),
        None,
        &Circle::new(center, radius),
    );
    // Value arc — start at top, sweep clockwise (positive sweep is
    // clockwise under y-down screen coords).
    let frac = f64::from(frac.clamp(0.0, 1.0));
    if frac > 0.0 {
        let arc = Arc::new(
            center,
            (radius, radius),
            -FRAC_PI_2,
            frac * std::f64::consts::TAU,
            0.0,
        );
        let stroke = Stroke::new(thickness)
            .with_caps(vello::kurbo::Cap::Round)
            .with_join(vello::kurbo::Join::Round);
        scene.stroke(&stroke, t, color, None, &arc);
    }

    // Centre labels.
    let big_size = ts(tr, theme.tokens.typography.scale.xl);
    let (bw, bh) = tr.measure_weighted(big, big_size, None, TextWeight::Bold);
    tr.draw_text_weighted(
        scene,
        big,
        big_size,
        colors.text_primary,
        t * Affine::translate((
            center.x - f64::from(bw) / 2.0,
            center.y - f64::from(bh) / 2.0 - 4.0,
        )),
        None,
        TextWeight::Bold,
    );
    let small_size = ts(tr, theme.tokens.typography.scale.xs);
    let (sw, _) = tr.measure(small, small_size, None);
    tr.draw_text(
        scene,
        small,
        small_size,
        colors.text_dim,
        t * Affine::translate((
            center.x - f64::from(sw) / 2.0,
            center.y + f64::from(bh) / 2.0 - 2.0,
        )),
        None,
    );
}

/// A labelled horizontal bar gauge. `rect` covers the label row + the
/// track; the label sits on the first line, the value is right-aligned,
/// and the fill bar is drawn along the bottom of `rect`.
#[allow(
    clippy::too_many_arguments,
    reason = "bar gauges combine paint context and bounds with fraction, fill color, label and formatted value"
)]
pub fn bar_gauge(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    rect: Rect,
    theme: &Theme,
    frac: f32,
    color: Color,
    label: &str,
    value: &str,
) {
    let colors = &theme.tokens.colors;
    let fs = ts(tr, theme.tokens.typography.scale.xs);
    tr.draw_text_weighted(
        scene,
        label,
        fs,
        colors.text_primary,
        t * Affine::translate((rect.x0, rect.y0)),
        None,
        TextWeight::Medium,
    );
    let (vw, _) = tr.measure(value, fs, None);
    tr.draw_text(
        scene,
        value,
        fs,
        colors.text_dim,
        t * Affine::translate((rect.x1 - f64::from(vw), rect.y0)),
        None,
    );
    let bar_h = 8.0;
    let track = Rect::new(rect.x0, rect.y1 - bar_h, rect.x1, rect.y1);
    fill_rrect(scene, t, track, bar_h / 2.0, inner_rule(theme, 56));
    let frac = f64::from(frac.clamp(0.0, 1.0));
    if frac > 0.0 {
        let fill = Rect::new(
            track.x0,
            track.y0,
            track.x0 + track.width() * frac,
            track.y1,
        );
        fill_rrect(scene, t, fill, bar_h / 2.0, color);
    }
}

/// A tiny filled sparkline with no chrome — for the side rail.
pub fn sparkline(scene: &mut Scene, t: Affine, rect: Rect, data: &[f32], max: f32, color: Color) {
    if data.len() < 2 || rect.width() <= 0.0 {
        return;
    }
    let ceil = max.max(1e-3);
    let n = data.len();
    let dx = rect.width() / (n as f64 - 1.0);
    let map = |i: usize, v: f32| -> Point {
        let frac = f64::from((v / ceil).clamp(0.0, 1.0));
        Point::new(rect.x0 + dx * i as f64, rect.y1 - frac * rect.height())
    };
    let mut area = BezPath::new();
    area.move_to(Point::new(rect.x0, rect.y1));
    for (i, &v) in data.iter().enumerate() {
        area.line_to(map(i, v));
    }
    area.line_to(Point::new(rect.x1, rect.y1));
    area.close_path();
    scene.fill(Fill::NonZero, t, with_alpha(color, 54), None, &area);
    let mut line = BezPath::new();
    if let Some(&first) = data.first() {
        line.move_to(map(0, first));
    }
    for (i, &v) in data.iter().enumerate().skip(1) {
        line.line_to(map(i, v));
    }
    scene.stroke(&Stroke::new(1.5), t, color, None, &line);
}

/// Stable informational colour for utilisation gauges and values.
///
/// A high reading is not inherently an error, and aggregate process CPU can
/// legitimately exceed 100% on multicore systems. Reserve semantic
/// success/warning/danger colours for a real threshold or action outcome;
/// magnitude is already communicated by the number and gauge geometry.
pub fn load_color(theme: &Theme, _frac: f32) -> Color {
    theme.tokens.colors.accent[5]
}
