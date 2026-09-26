//! The monitor's small Vello paint helpers: rounded panels, the rules
//! inside them and text metrics. Charts and gauges are the toolkit's
//! (`ui::charts`); these only frame them. Everything reads its colours from
//! the passed [`Theme`] so the OS owns the palette.

pub use ui::paint::with_alpha;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

/// Theme type sizes (`theme.tokens.typography.scale.*`) already carry the
/// accessibility text scale, and the legacy size-based renderer APIs
/// (`measure`, `draw_text*`, `ButtonFace::text_size`) apply that scale once
/// more. Route every token through here so the multiplier lands exactly once:
/// 2× glyphs at 200 %, not 4×. Identity at 1×.
pub fn ts(tr: &TextRenderer, resolved: f32) -> f32 {
    ui::text_metrics::renderer_type_size(tr, resolved)
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

/// Stable informational colour for utilisation gauges and values.
///
/// A high reading is not inherently an error, and aggregate process CPU can
/// legitimately exceed 100% on multicore systems. Reserve semantic
/// success/warning/danger colours for a real threshold or action outcome;
/// magnitude is already communicated by the number and gauge geometry.
pub fn load_color(theme: &Theme, _frac: f32) -> Color {
    theme.tokens.colors.accent[5]
}
