//! Theme-driven drawing primitives shared across the manager's panels.
//!
//! Every helper here reads its colours and spacing from the passed-in
//! [`Theme`] — nothing hard-codes a colour — so the manager re-themes with
//! the rest of the OS. They paint directly into a Vello [`Scene`]; the app
//! widget owns hit-testing and state, these are pure paint.

use ui::icons::Icon as UiIcon;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::ColorCompat;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use vello::Scene;

use crate::model::ContainerStatus;

/// Semantic accent for a container status — used for the status dot/pill.
pub fn status_color(theme: &Theme, status: ContainerStatus) -> Color {
    let c = &theme.tokens.colors;
    match status {
        ContainerStatus::Running => c.semantic.success,
        ContainerStatus::Restarting => c.semantic.info,
        ContainerStatus::Paused => c.semantic.warning,
        ContainerStatus::Exited => c.text_dim,
        ContainerStatus::Created => c.semantic.info,
        ContainerStatus::Dead => c.semantic.danger,
    }
}

/// Mix two colours by `t` (0 = a, 1 = b), preserving alpha of `a`.
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| ((x as f32) * (1.0 - t) + (y as f32) * t).round() as u8;
    Color::from_rgba8(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()), a.a())
}

/// Apply an explicit alpha (0..=255) to a colour.
pub fn with_alpha(c: Color, alpha: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), alpha)
}

/// The base transform every helper composes onto. Set once per frame from the
/// host's DPI transform (`Affine::scale(scale_factor)`); a thread-local keeps
/// the helper signatures terse while still honouring HiDPI scaling.
use std::cell::Cell;
thread_local! {
    static BASE: Cell<Affine> = const { Cell::new(Affine::IDENTITY) };
}

/// Install the per-frame base transform. Call at the top of the root draw.
pub fn set_base(transform: Affine) {
    BASE.with(|b| b.set(transform));
}

/// The per-frame base transform installed by [`set_base`]. Exposed so
/// hosted `ui` widgets (which take an explicit transform) compose onto
/// the same HiDPI transform the immediate-mode `draw::*` helpers use.
pub fn base() -> Affine {
    BASE.with(|b| b.get())
}

/// Fill a rounded rectangle.
pub fn fill_round(scene: &mut Scene, rect: Rect, radius: f64, color: Color) {
    let shape = RoundedRect::from_rect(rect, radius);
    scene.fill(Fill::NonZero, base(), color, None, &shape);
}

/// Stroke a rounded rectangle outline.
pub fn stroke_round(scene: &mut Scene, rect: Rect, radius: f64, width: f64, color: Color) {
    let shape = RoundedRect::from_rect(rect, radius);
    scene.stroke(&Stroke::new(width), base(), color, None, &shape);
}

/// Draw a single line of text, returning its measured width.
pub fn text(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    s: &str,
    size: f32,
    color: Color,
    at: Point,
    weight: TextWeight,
) -> f32 {
    let (w, _) = tr.measure_weighted(s, size, None, weight);
    tr.draw_text_weighted(scene, s, size, color, base() * Affine::translate((at.x, at.y)), None, weight);
    w
}

/// Draw text truncated with an ellipsis to fit `max_width`.
pub fn text_clip(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    s: &str,
    size: f32,
    color: Color,
    at: Point,
    max_width: f64,
    weight: TextWeight,
) {
    let (w, _) = tr.measure_weighted(s, size, None, weight);
    if (w as f64) <= max_width {
        tr.draw_text_weighted(scene, s, size, color, base() * Affine::translate((at.x, at.y)), None, weight);
        return;
    }
    // Binary-search the longest prefix that fits with an ellipsis.
    let chars: Vec<char> = s.chars().collect();
    let ell = "…";
    let mut lo = 0usize;
    let mut hi = chars.len();
    let mut best = String::new();
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let candidate: String = chars[..mid].iter().collect::<String>() + ell;
        let (cw, _) = tr.measure_weighted(&candidate, size, None, weight);
        if (cw as f64) <= max_width {
            best = candidate;
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if best.is_empty() {
        best = ell.to_string();
    }
    tr.draw_text_weighted(scene, &best, size, color, base() * Affine::translate((at.x, at.y)), None, weight);
}

/// A status pill: a filled-dot + label inside a faint rounded chip.
pub fn status_pill(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    theme: &Theme,
    status: ContainerStatus,
    label: &str,
    origin: Point,
    height: f64,
) -> f64 {
    let color = status_color(theme, status);
    let size = theme.tokens.typography.scale.sm;
    let (tw, _) = tr.measure_weighted(label, size, None, TextWeight::Medium);
    let pad_x = 10.0;
    let dot_r = 3.5;
    let dot_gap = 6.0;
    let width = pad_x * 2.0 + dot_r * 2.0 + dot_gap + tw as f64;
    let rect = Rect::new(origin.x, origin.y, origin.x + width, origin.y + height);
    fill_round(scene, rect, height / 2.0, with_alpha(color, 38));
    let cy = rect.center().y;
    let dot = Circle::new(Point::new(rect.x0 + pad_x + dot_r, cy), dot_r);
    scene.fill(Fill::NonZero, base(), color, None, &dot);
    let ty = cy - (size as f64) / 2.0 - 1.0;
    text(
        scene,
        tr,
        label,
        size,
        color,
        Point::new(rect.x0 + pad_x + dot_r * 2.0 + dot_gap, ty),
        TextWeight::Medium,
    );
    width
}

/// A small key/value chip used for ports.
pub fn chip(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    theme: &Theme,
    label: &str,
    origin: Point,
    height: f64,
    accent: Color,
) -> f64 {
    let size = theme.tokens.typography.scale.xs;
    let (tw, _) = tr.measure(label, size, None);
    let pad_x = 8.0;
    let width = pad_x * 2.0 + tw as f64;
    let rect = Rect::new(origin.x, origin.y, origin.x + width, origin.y + height);
    fill_round(scene, rect, 4.0, with_alpha(accent, 30));
    let ty = rect.center().y - (size as f64) / 2.0 - 1.0;
    text(scene, tr, label, size, accent, Point::new(rect.x0 + pad_x, ty), TextWeight::Medium);
    width
}

/// A horizontal progress/usage bar.
pub fn usage_bar(scene: &mut Scene, theme: &Theme, rect: Rect, fraction: f32, color: Color) {
    let track = theme.tokens.colors.neutral[3];
    fill_round(scene, rect, rect.height() / 2.0, with_alpha(track, 120));
    let frac = fraction.clamp(0.0, 1.0) as f64;
    if frac > 0.0 {
        let filled = Rect::new(rect.x0, rect.y0, rect.x0 + rect.width() * frac, rect.y1);
        fill_round(scene, filled, rect.height() / 2.0, color);
    }
}

/// A thin divider line spanning `[x0, x1]` at `y`.
pub fn divider(scene: &mut Scene, x0: f64, x1: f64, y: f64, color: Color) {
    let line = Line::new(Point::new(x0, y), Point::new(x1, y));
    scene.stroke(&Stroke::new(1.0), base(), color, None, &line);
}

/// Draw a control glyph centred in `rect`, in `color`.
///
/// The glyph pixels come from the shared [`ui::icons::Icon`] set — the one
/// themable, Lucide-based icon set the whole OS draws from — so the manager's
/// controls match every other app. The local [`Icon`] enum is just a
/// domain-readable alias that this function maps onto that shared set.
pub fn icon(scene: &mut Scene, kind: Icon, rect: Rect, color: Color, width: f64) {
    match kind {
        // "Running" and "stop" affordances read as solid marks, not outlines.
        Icon::Play => UiIcon::Play.draw_filled(scene, base(), rect, color),
        Icon::Stop => UiIcon::Square.draw_filled(scene, base(), rect, color),
        _ => {
            let glyph = match kind {
                Icon::Box => UiIcon::Package,
                Icon::Layers => UiIcon::Layers,
                Icon::Terminal => UiIcon::Terminal,
                Icon::Restart => UiIcon::RotateCw,
                Icon::Pause => UiIcon::Pause,
                Icon::Trash => UiIcon::Trash2,
                Icon::Plus => UiIcon::Plus,
                Icon::Search => UiIcon::Search,
                Icon::Enter => UiIcon::CornerDownLeft,
                // Play / Stop are drawn filled above.
                Icon::Play | Icon::Stop => unreachable!(),
            };
            glyph.draw_with_stroke(scene, base(), rect, color, width);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Box,
    Layers,
    Terminal,
    Play,
    Stop,
    Restart,
    Pause,
    Trash,
    Plus,
    Search,
    Enter,
}
