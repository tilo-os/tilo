//! Shared low-level drawing helpers built directly on the Vello scene.
//!
//! The git client renders its diff gutters, commit graph and dense list
//! rows with pixel-level control, so most views paint into the scene
//! themselves rather than composing high-level widgets. These helpers
//! keep that painting consistent and — crucially — theme-driven: every
//! colour comes from a [`Theme`] token, never a literal, so the app
//! tracks the OS theme like every other system app.

use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Avatar;

use crate::model::ChangeKind;

/// Fill a plain rectangle.
pub fn fill_rect(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &rect);
}

/// Fill a rounded rectangle.
pub fn fill_round(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, color: Color) {
    let r = RoundedRect::from_rect(rect, radius);
    scene.fill(Fill::NonZero, t, color, None, &r);
}

/// Stroke a rounded rectangle outline.
pub fn stroke_round(
    scene: &mut Scene,
    t: Affine,
    rect: Rect,
    radius: f64,
    width: f64,
    color: Color,
) {
    let r = RoundedRect::from_rect(rect, radius);
    scene.stroke(&Stroke::new(width), t, color, None, &r);
}

/// Draw a horizontal hairline separator.
pub fn hline(scene: &mut Scene, t: Affine, y: f64, x0: f64, x1: f64, color: Color) {
    let line = Line::new(Point::new(x0, y), Point::new(x1, y));
    scene.stroke(&Stroke::new(1.0), t, color, None, &line);
}

/// Draw a vertical hairline separator.
pub fn vline(scene: &mut Scene, t: Affine, x: f64, y0: f64, y1: f64, color: Color) {
    let line = Line::new(Point::new(x, y0), Point::new(x, y1));
    scene.stroke(&Stroke::new(1.0), t, color, None, &line);
}

/// Left-aligned text at a baseline-ish position. `y` is the top of the line.
#[allow(
    clippy::too_many_arguments,
    reason = "a thin pass-through over TextRenderer::draw_text's own geometry + style params, matching the sibling draw helpers below"
)]
pub fn text(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x: f64,
    y: f64,
    max_w: Option<f32>,
) {
    tr.draw_text(scene, s, size, color, t * Affine::translate((x, y)), max_w);
}

/// Weighted text variant.
#[allow(clippy::too_many_arguments)]
pub fn text_weighted(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x: f64,
    y: f64,
    max_w: Option<f32>,
    weight: TextWeight,
) {
    tr.draw_text_weighted(
        scene,
        s,
        size,
        color,
        t * Affine::translate((x, y)),
        max_w,
        weight,
    );
}

/// Monospace styled text (for code, hashes, diff bodies).
#[allow(clippy::too_many_arguments)]
pub fn mono(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x: f64,
    y: f64,
) {
    tr.draw_text_styled(
        scene,
        s,
        size,
        color,
        t * Affine::translate((x, y)),
        None,
        TextWeight::Regular,
        false,
        true,
    );
}

/// Measure a monospace string's advance width.
pub fn mono_width(tr: &mut TextRenderer, s: &str, size: f32) -> f32 {
    tr.measure_styled(s, size, None, TextWeight::Regular, false, true)
        .0
}

/// Top-of-line `y` that vertically centres `s` (drawn at `size`/`weight`)
/// inside the band starting at `y0` with height `h`.
///
/// Centring uses the measured line height rather than the font size, so a
/// 12px title, an 11px badge and a 13px tab label placed in one band share
/// one centre line at every text scale.
pub fn centered_top(
    tr: &mut TextRenderer,
    s: &str,
    size: f32,
    weight: TextWeight,
    y0: f64,
    h: f64,
) -> f64 {
    let (_, th) = tr.measure_weighted(s, size, None, weight);
    y0 + (h - f64::from(th)) / 2.0
}

/// Monospace counterpart of [`centered_top`], measured with the same styled
/// path [`mono`] paints with.
pub fn mono_centered_top(tr: &mut TextRenderer, s: &str, size: f32, y0: f64, h: f64) -> f64 {
    let (_, th) = tr.measure_styled(s, size, None, TextWeight::Regular, false, true);
    y0 + (h - f64::from(th)) / 2.0
}

/// A small rounded "pill" with a label — used for ref decorations, badges,
/// ahead/behind counts. Returns the advance width consumed (pill + gap).
#[allow(clippy::too_many_arguments)]
pub fn pill(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    x: f64,
    y: f64,
    height: f64,
    label: &str,
    fg: Color,
    bg: Color,
    border: Option<Color>,
) -> f64 {
    let pad = 6.0;
    let size = (height - 6.0).max(9.0) as f32;
    // Measure once with the exact style painted below: the width sizes the
    // pill and the line height centres the label in it. Centring on the
    // measured line (not the font size) removes the old `- 1.0` nudge.
    let (tw, th) = tr.measure_weighted(label, size, None, TextWeight::Medium);
    let w = f64::from(tw) + pad * 2.0;
    let rect = Rect::new(x, y, x + w, y + height);
    fill_round(scene, t, rect, height / 2.0, bg);
    if let Some(bc) = border {
        stroke_round(scene, t, rect, height / 2.0, 1.0, bc);
    }
    text_weighted(
        scene,
        tr,
        t,
        label,
        size,
        fg,
        x + pad,
        y + (height - f64::from(th)) / 2.0,
        None,
        TextWeight::Medium,
    );
    w + 6.0
}

/// Theme colour for a diff addition row tint (translucent emerald).
pub fn add_tint(theme: &Theme) -> Color {
    let c = theme.tokens.colors.semantic.success;
    Color::from_rgba8(c.r(), c.g(), c.b(), 28)
}

/// Theme colour for a diff deletion row tint (translucent red).
pub fn del_tint(theme: &Theme) -> Color {
    let c = theme.tokens.colors.semantic.danger;
    Color::from_rgba8(c.r(), c.g(), c.b(), 28)
}

/// Strong addition foreground (the +/glyph and gutter).
pub fn add_fg(theme: &Theme) -> Color {
    theme.tokens.colors.semantic.success
}

/// Strong deletion foreground.
pub fn del_fg(theme: &Theme) -> Color {
    theme.tokens.colors.semantic.danger
}

/// Map a [`ChangeKind`] to its status-badge foreground colour, sourced
/// from theme semantic tokens.
pub fn kind_color(theme: &Theme, kind: ChangeKind) -> Color {
    let c = &theme.tokens.colors;
    match kind {
        ChangeKind::Added | ChangeKind::Untracked => c.semantic.success,
        ChangeKind::Modified => c.semantic.warning,
        ChangeKind::Deleted | ChangeKind::Conflicted => c.semantic.danger,
        ChangeKind::Renamed => c.semantic.info,
    }
}

/// A stable, theme-anchored colour for an author, derived from a hash of
/// their email so the same person keeps the same hue across views. Uses
/// the theme's categorical accent series rather than semantic
/// success/warning/danger colours: an author's identity should never read
/// like a file state.
pub fn author_color(theme: &Theme, email: &str) -> Color {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    email.hash(&mut h);
    let c = &theme.tokens.colors;
    let palette = [
        c.categorical(0),
        c.categorical(1),
        c.categorical(2),
        c.categorical(3),
        c.categorical(4),
        c.categorical(5),
    ];
    // `palette` is a fixed non-empty array, so the modulo below always lands
    // in bounds; the fallback is unreachable in practice.
    let idx = (h.finish() % palette.len() as u64) as usize;
    palette
        .get(idx)
        .copied()
        .unwrap_or_else(|| c.categorical(0))
}

/// Lane colour for the commit graph — cycles through accent + semantic
/// hues so adjacent lanes stay distinguishable.
pub fn lane_color(theme: &Theme, lane: usize) -> Color {
    let c = &theme.tokens.colors;
    let palette = [
        c.accent[5],
        c.semantic.success,
        c.semantic.warning,
        c.semantic.info,
        c.accent[7],
        c.semantic.danger,
    ];
    // `palette` is a fixed non-empty array, so the modulo below always lands
    // in bounds; the fallback is unreachable in practice.
    palette
        .get(lane % palette.len())
        .copied()
        .unwrap_or(c.semantic.danger)
}

/// Two-initials avatar disc for an author. `color` is the author's stable
/// per-email hue (see [`author_color`]); the disc + centred monogram are
/// painted by the shared toolkit avatar so shape/placement match every other
/// app. Returns the disc diameter.
#[allow(
    clippy::too_many_arguments,
    reason = "a thin pass-through over the scene + geometry + style params every draw helper in this module takes"
)]
pub fn avatar(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    theme: &Theme,
    x: f64,
    y: f64,
    diameter: f64,
    name: &str,
    color: Color,
) -> f64 {
    let rect = Rect::new(x, y, x + diameter, y + diameter);
    let initials = Avatar::initials_for_name(name);
    Avatar::draw_at(scene, t, rect, &initials, color, theme, tr);
    diameter
}
