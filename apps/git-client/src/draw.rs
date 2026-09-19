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
use ui::widget::{Avatar, Button, ButtonFace};

use crate::model::ChangeKind;

/// The mutable resources shared by one immediate-mode paint pass.
///
/// Views previously threaded these three values through every entry point and
/// helper independently. Keeping them together makes it impossible to pair a
/// scene with the wrong text renderer or transform while a view is split into
/// smaller drawing responsibilities.
pub struct PaintCtx<'a> {
    pub scene: &'a mut Scene,
    pub text: &'a mut TextRenderer,
    pub transform: Affine,
}

impl<'a> PaintCtx<'a> {
    pub fn new(scene: &'a mut Scene, text: &'a mut TextRenderer, transform: Affine) -> Self {
        Self {
            scene,
            text,
            transform,
        }
    }

    pub fn parts(&mut self) -> (&mut Scene, &mut TextRenderer, Affine) {
        (self.scene, self.text, self.transform)
    }
}

/// Fill a plain rectangle.
pub fn fill_rect(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &rect);
}

/// Fill a rounded rectangle.
pub fn fill_round(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, color: Color) {
    let r = RoundedRect::from_rect(rect, radius);
    scene.fill(Fill::NonZero, t, color, None, &r);
}

/// A card, row or control face with its derived rest edge (HIG §9.5): the
/// fill plus a crisp 1 px inset hairline measured against that fill, so the
/// edge registers on the dark preset where a declared `border_standard`
/// stroke on a raised surface vanished.
pub fn card(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, fill: Color) {
    ui::surface::card(scene, t, rect, radius, fill);
}

/// The derived hairline for a surface painted in `fill` — what a divider
/// between that surface and its neighbour is stroked in.
#[must_use]
pub fn edge(fill: Color) -> Color {
    ui::surface::edge_on(fill)
}

/// Paint a button through the shared face so rest, hover, disabled and
/// primary/danger looks come from one painter. The caller keeps its own hit
/// rect; this only paints.
pub fn button(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    theme: &Theme,
    face: &ButtonFace<'_>,
) {
    Button::paint(scene, t, tr, theme, face);
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
#[allow(
    clippy::too_many_arguments,
    reason = "a thin pass-through over TextRenderer::draw_text_weighted's geometry and text-style parameters"
)]
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
#[allow(
    clippy::too_many_arguments,
    reason = "a low-level text primitive whose arguments directly select content, geometry, and colour"
)]
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

/// Leading a pill keeps around its measured label line. A label measures
/// 1.25× its size, so 10/12/14 px labels (12.5/15/17.5 px) reproduce the
/// historical 16/18/20 px pills exactly, while a 2× label grows the pill.
const PILL_LEADING: f64 = 2.5;

/// Horizontal label padding inside a [`pill`] (the same 6 px the pill has
/// always used; shared by the width, height and paint helpers).
fn pill_pad() -> f64 {
    6.0
}

/// Height of a [`pill`] whose label is painted at `size`: the measured line
/// height plus leading, never shorter than the label size plus padding.
///
/// Callers size the pill from its label rather than the label from the pill,
/// because the renderer scales `size` by the text scale — deriving a size from
/// a fixed height painted 2× glyphs inside a 1× pill.
pub fn pill_height(tr: &mut TextRenderer, size: f32) -> f64 {
    let (_, th) = tr.measure_weighted("Ag", size, None, TextWeight::Medium);
    (f64::from(th) + PILL_LEADING).max(f64::from(size) + pill_pad())
}

/// Painted width of a [`pill`] carrying `label` at `size` (without the
/// trailing gap the draw call adds to its advance).
pub fn pill_width(tr: &mut TextRenderer, label: &str, size: f32) -> f64 {
    let (tw, _) = tr.measure_weighted(label, size, None, TextWeight::Medium);
    f64::from(tw) + pill_pad() * 2.0
}

/// Truncate monospace `s` to fit `max_width` at `size`, appending an ellipsis
/// when anything was dropped — the mono counterpart of
/// [`TextRenderer::elide_to_width`], measured with the same styled path
/// [`mono`] paints with.
pub fn elide_mono(tr: &mut TextRenderer, s: &str, size: f32, max_width: f64) -> String {
    const ELLIPSIS: &str = "\u{2026}";
    if s.is_empty() || max_width <= 0.0 {
        return String::new();
    }
    if f64::from(mono_width(tr, s, size)) <= max_width {
        return s.to_string();
    }
    let ell_w = f64::from(mono_width(tr, ELLIPSIS, size));
    if ell_w > max_width {
        return ELLIPSIS.to_string();
    }
    let budget = max_width - ell_w;
    let mut best = String::new();
    let mut candidate = String::new();
    for ch in s.chars() {
        candidate.push(ch);
        if f64::from(mono_width(tr, &candidate, size)) <= budget {
            best.push(ch);
        } else {
            break;
        }
    }
    // Trim the space before the ellipsis, as the toolkit's sans-serif
    // elision does, so `impl Scene …` and `impl Scene…` never sit side by side.
    let trimmed = best.trim_end().len();
    best.truncate(trimmed);
    best.push_str(ELLIPSIS);
    best
}

/// How a [`pill`] closes its edge (HIG §9.5 rest edges).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PillEdge {
    /// A quiet pill on a neutral fill: the hairline is derived from the fill
    /// (`ui::surface::edge_on`), so it reads on every theme.
    Derived,
    /// An accent or semantic fill is self-defining and carries no edge.
    Bare,
}

/// A small rounded "pill" with a label — used for ref decorations, badges,
/// ahead/behind counts. The label is painted at `size`; the pill is
/// [`pill_height`] tall. Returns the advance width consumed (pill + gap).
#[allow(
    clippy::too_many_arguments,
    reason = "Git decoration pills expose position, text size and label, foreground/background colors, and edge style beside render context."
)]
pub fn pill(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    x: f64,
    y: f64,
    size: f32,
    label: &str,
    fg: Color,
    bg: Color,
    edge: PillEdge,
) -> f64 {
    let pad = pill_pad();
    let height = pill_height(tr, size);
    // Measure once with the exact style painted below: the width sizes the
    // pill and the line height centres the label in it. Centring on the
    // measured line (not the font size) removes the old `- 1.0` nudge.
    let (tw, th) = tr.measure_weighted(label, size, None, TextWeight::Medium);
    let w = f64::from(tw) + pad * 2.0;
    let rect = Rect::new(x, y, x + w, y + height);
    match edge {
        PillEdge::Derived => card(scene, t, rect, height / 2.0, bg),
        PillEdge::Bare => fill_round(scene, t, rect, height / 2.0, bg),
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
