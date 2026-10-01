//! Shared low-level drawing helpers built directly on the Vello scene.
//!
//! The git client renders its diff gutters, commit graph and dense list
//! rows with pixel-level control, so most views paint into the scene
//! themselves rather than composing high-level widgets. These helpers
//! keep that painting consistent and — crucially — theme-driven: every
//! colour comes from a [`Theme`] token, never a literal, so the app
//! tracks the OS theme like every other system app.

use vello::kurbo::{Line, Point, Rect, Stroke};
use vello::peniko::Color;

use ui::paint::PaintCtx;
use ui::text_render::{FontChoice, TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::widget::{Avatar, Button, ButtonFace};

use crate::model::ChangeKind;

/// A card, row or control face with its derived rest edge (HIG §9.5): the
/// fill plus a crisp 1 px inset hairline measured against that fill, so the
/// edge registers on the dark preset where a declared `border_standard`
/// stroke on a raised surface vanished.
pub fn card(ctx: &mut PaintCtx<'_>, rect: Rect, radius: f64, fill: Color) {
    ui::surface::card(ctx.scene, ctx.transform, rect, radius, fill);
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
pub fn button(ctx: &mut PaintCtx<'_>, face: &ButtonFace<'_>) {
    Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, face);
}

/// Draw a horizontal hairline separator.
pub fn hline(ctx: &mut PaintCtx<'_>, y: f64, x0: f64, x1: f64, color: Color) {
    let line = Line::new(Point::new(x0, y), Point::new(x1, y));
    ctx.scene
        .stroke(&Stroke::new(1.0), ctx.transform, color, None, &line);
}

/// Draw a vertical hairline separator.
pub fn vline(ctx: &mut PaintCtx<'_>, x: f64, y0: f64, y1: f64, color: Color) {
    let line = Line::new(Point::new(x, y0), Point::new(x, y1));
    ctx.scene
        .stroke(&Stroke::new(1.0), ctx.transform, color, None, &line);
}

/// The monospace style code, hashes and diff bodies are shaped in.
#[must_use]
pub fn mono_style(size: f32) -> TextStyle {
    TextStyle::new(size).with_family(FontChoice::Monospace)
}

/// Monospace text (for code, hashes, diff bodies) with its top-left at `origin`.
pub fn mono(ctx: &mut PaintCtx<'_>, s: &str, size: f32, color: Color, origin: Point) {
    ctx.text(s, &mono_style(size), color, origin, None);
}

/// Measure a monospace string's advance width.
pub fn mono_width(tr: &mut TextRenderer, s: &str, size: f32) -> f32 {
    tr.measure_style(s, None, &mono_style(size)).0
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
    let (_, th) = tr.measure_style(s, None, &mono_style(size));
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

/// How a [`pill`] closes its edge (HIG §9.5 rest edges).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PillEdge {
    /// A quiet pill on a neutral fill: the hairline is derived from the fill
    /// (`ui::surface::edge_on`), so it reads on every theme.
    Derived,
    /// An accent or semantic fill is self-defining and carries no edge.
    Bare,
}

/// How a [`pill`] is painted: its label size, label and fill colours, and edge.
#[derive(Clone, Copy, Debug)]
pub struct PillStyle {
    pub size: f32,
    pub fg: Color,
    pub bg: Color,
    pub edge: PillEdge,
}

/// A small rounded "pill" with a label — used for ref decorations, badges,
/// ahead/behind counts. The label is painted at `style.size`; the pill is
/// [`pill_height`] tall with its top-left at `origin`. Returns the advance
/// width consumed (pill + gap).
pub fn pill(ctx: &mut PaintCtx<'_>, origin: Point, label: &str, style: &PillStyle) -> f64 {
    let pad = pill_pad();
    let height = pill_height(ctx.text, style.size);
    // Measure once with the exact style painted below: the width sizes the
    // pill and the line height centres the label in it. Centring on the
    // measured line (not the font size) removes the old `- 1.0` nudge.
    let label_style = TextStyle::new(style.size).with_weight(TextWeight::Medium);
    let (tw, th) = ctx.text.measure_style(label, None, &label_style);
    let w = f64::from(tw) + pad * 2.0;
    let rect = Rect::from_origin_size(origin, (w, height));
    match style.edge {
        PillEdge::Derived => card(ctx, rect, height / 2.0, style.bg),
        PillEdge::Bare => ctx.fill_round(rect, height / 2.0, style.bg),
    }
    let label_origin = Point::new(origin.x + pad, origin.y + (height - f64::from(th)) / 2.0);
    ctx.text(label, &label_style, style.fg, label_origin, None);
    w + 6.0
}

/// Theme colour for a diff addition row tint (translucent emerald).
pub fn add_tint(theme: &Theme) -> Color {
    ui::paint::with_alpha(theme.tokens.colors.semantic.success, 28)
}

/// Theme colour for a diff deletion row tint (translucent red).
pub fn del_tint(theme: &Theme) -> Color {
    ui::paint::with_alpha(theme.tokens.colors.semantic.danger, 28)
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
pub fn avatar(
    ctx: &mut PaintCtx<'_>,
    origin: Point,
    diameter: f64,
    name: &str,
    color: Color,
) -> f64 {
    let rect = Rect::from_origin_size(origin, (diameter, diameter));
    let initials = Avatar::initials_for_name(name);
    Avatar::draw_at(
        ctx.scene,
        ctx.transform,
        rect,
        &initials,
        color,
        ctx.theme,
        ctx.text,
    );
    diameter
}
