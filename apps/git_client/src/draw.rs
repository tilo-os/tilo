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
    let tw = f64::from(tr.measure_weighted(label, size, None, TextWeight::Medium).0);
    let w = tw + pad * 2.0;
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
        y + (height - f64::from(size)) / 2.0 - 1.0,
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
        ChangeKind::Deleted => c.semantic.danger,
        ChangeKind::Renamed => c.semantic.info,
        ChangeKind::Conflicted => c.semantic.danger,
    }
}

/// A stable, theme-anchored colour for an author, derived from a hash of
/// their email so the same person keeps the same hue across views. Uses
/// the accent ramp + a couple of semantic anchors so colours stay in
/// palette.
pub fn author_color(theme: &Theme, email: &str) -> Color {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    email.hash(&mut h);
    let c = &theme.tokens.colors;
    let palette = [
        c.accent[5],
        c.semantic.success,
        c.semantic.warning,
        c.semantic.info,
        c.accent[6],
        c.semantic.danger,
    ];
    palette[(h.finish() % palette.len() as u64) as usize]
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
    palette[lane % palette.len()]
}

/// Two-initials avatar disc for an author. `color` is the author's stable
/// per-email hue (see [`author_color`]); the disc + centred monogram are
/// painted by the shared toolkit avatar so shape/placement match every other
/// app. Returns the disc diameter.
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
