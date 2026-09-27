//! Theme-driven drawing primitives shared across the manager's panels.
//!
//! Every helper here reads its colours and spacing from the passed-in
//! [`Theme`] — nothing hard-codes a colour — so the manager re-themes with
//! the rest of the OS. They paint directly into a Vello [`Scene`]; the app
//! widget owns hit-testing and state, these are pure paint.

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextStyle as RendererStyle, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use crate::model::ContainerStatus;
pub use ui::paint::with_alpha;

/// Semantic accent for a container status — used for the status dot/pill.
pub fn status_color(theme: &Theme, status: ContainerStatus) -> Color {
    let c = &theme.tokens.colors;
    match status {
        ContainerStatus::Running => c.semantic.success,
        ContainerStatus::Restarting | ContainerStatus::Created => c.semantic.info,
        ContainerStatus::Paused => c.semantic.warning,
        ContainerStatus::Exited => c.text_dim,
        ContainerStatus::Dead => c.semantic.danger,
    }
}

/// Mix two colours by `t` (0 = a, 1 = b), preserving alpha of `a`.
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (f32::from(x) * (1.0 - t) + f32::from(y) * t).round() as u8;
    Color::from_rgba8(
        lerp(a.r(), b.r()),
        lerp(a.g(), b.g()),
        lerp(a.b(), b.b()),
        a.a(),
    )
}

/// The base transform every helper composes onto. Set once per frame from the
/// host's DPI transform (`Affine::scale(scale_factor)`); a thread-local keeps
/// the helper signatures terse while still honouring `HiDPI` scaling.
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
/// the same `HiDPI` transform the immediate-mode `draw::*` helpers use.
pub fn base() -> Affine {
    BASE.with(std::cell::Cell::get)
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

/// A card, panel or control face with its derived rest edge (HIG §9.5):
/// the fill plus a crisp 1 px inset hairline measured against that fill, so
/// the edge registers on the dark preset where a declared `border_standard`
/// stroke on a raised surface vanished. Composes onto the per-frame base
/// transform like every helper here.
pub fn card(scene: &mut Scene, rect: Rect, radius: f64, fill: Color) {
    ui::surface::card(scene, base(), rect, radius, fill);
}

/// The derived hairline for a surface painted in `fill` — what a rule at the
/// boundary of that surface is painted in.
#[must_use]
pub fn edge(fill: Color) -> Color {
    ui::surface::edge_on(fill)
}

/// A 1 px rule inside a surface painted in `surface`, from `x0` to `x1`
/// centred on `y`; derived against that surface rather than declared.
pub fn separator(scene: &mut Scene, x0: f64, x1: f64, y: f64, surface: Color) {
    ui::surface::separator(scene, base(), x0, x1, y, 0.0, surface);
}

/// Draw a single line of text, returning its measured width.
/// Convert a resolved theme type token (already multiplied by the active text
/// scale) into the renderer-relative size the legacy `draw_text*` / `measure*`
/// APIs expect (HIG §7a). Every helper here and every measurement in the
/// panels goes through this once, so the scale is applied exactly one time;
/// passing the token straight in painted 40px glyphs for a 20px token at 200%.
pub fn px(tr: &TextRenderer, token_size: f32) -> f32 {
    tr.renderer_input_style(&RendererStyle::new(token_size))
        .size
}

/// Measured `(width, height)` of `s` at a theme token size, in logical pixels.
pub fn measure(tr: &mut TextRenderer, s: &str, token_size: f32, weight: TextWeight) -> (f32, f32) {
    let size = px(tr, token_size);
    tr.measure_weighted(s, size, None, weight)
}

/// Top-of-line `y` that centres `s` (at a theme token size) inside the band
/// `[y0, y0 + h)`, using the measured line height rather than the font size.
pub fn centered_top(
    tr: &mut TextRenderer,
    s: &str,
    token_size: f32,
    weight: TextWeight,
    y0: f64,
    h: f64,
) -> f64 {
    let (_, th) = measure(tr, s, token_size, weight);
    y0 + (h - f64::from(th)) / 2.0
}

pub fn text(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    s: &str,
    size: f32,
    color: Color,
    at: Point,
    weight: TextWeight,
) -> f32 {
    let size = px(tr, size);
    let (w, _) = tr.measure_weighted(s, size, None, weight);
    tr.draw_text_weighted(
        scene,
        s,
        size,
        color,
        base() * Affine::translate((at.x, at.y)),
        None,
        weight,
    );
    w
}

/// Size/colour/weight bundle for [`text_clip`], kept together so its
/// argument count stays within the lint limit without losing a parameter.
#[derive(Clone, Copy)]
pub struct TextStyle {
    pub size: f32,
    pub color: Color,
    pub weight: TextWeight,
}

/// Draw text truncated with an ellipsis to fit `max_width`.
pub fn text_clip(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    s: &str,
    style: &TextStyle,
    at: Point,
    max_width: f64,
) {
    let TextStyle {
        size,
        color,
        weight,
    } = *style;
    let size = px(tr, size);
    let clipped = tr.elide_to_width_weighted(s, size, ui::num::f32_of_f64(max_width), weight);
    tr.draw_text_weighted(
        scene,
        &clipped,
        size,
        color,
        base() * Affine::translate((at.x, at.y)),
        None,
        weight,
    );
}

/// Status pill geometry, shared by [`status_pill`] and [`status_pill_width`].
const PILL_PAD_X: f64 = 10.0;
const PILL_DOT_R: f64 = 3.5;
const PILL_DOT_GAP: f64 = 6.0;
/// Alpha of a semantic tint used as a pill / chip fill.
const TINT_FILL_ALPHA: u8 = 38;
/// Alpha of the same hue stroked as the pill / chip edge: strong enough to
/// close the shape on dark and light grounds, quieter than the label.
const TINT_EDGE_ALPHA: u8 = 96;

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
    let (tw, th) = measure(tr, label, size, TextWeight::Medium);
    let pad_x = PILL_PAD_X;
    let dot_r = PILL_DOT_R;
    let dot_gap = PILL_DOT_GAP;
    let width = pad_x * 2.0 + dot_r * 2.0 + dot_gap + f64::from(tw);
    let rect = Rect::new(origin.x, origin.y, origin.x + width, origin.y + height);
    // A tinted chip is edged in its own hue so it reads as a closed shape on
    // both themes; a translucent fill alone dissolves into a raised ground.
    fill_round(
        scene,
        rect,
        height / 2.0,
        with_alpha(color, TINT_FILL_ALPHA),
    );
    stroke_round(
        scene,
        rect.inset(-0.5),
        (height / 2.0 - 0.5).max(0.0),
        1.0,
        with_alpha(color, TINT_EDGE_ALPHA),
    );
    let cy = rect.center().y;
    let dot = Circle::new(Point::new(rect.x0 + pad_x + dot_r, cy), dot_r);
    scene.fill(Fill::NonZero, base(), color, None, &dot);
    // Centre on the measured line height rather than the font size.
    let ty = cy - f64::from(th) / 2.0;
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

/// The width [`status_pill`] will paint for `label`, so a caller can decide
/// whether the pill fits before committing to that layout.
pub fn status_pill_width(tr: &mut TextRenderer, theme: &Theme, label: &str) -> f64 {
    let (tw, _) = measure(
        tr,
        label,
        theme.tokens.typography.scale.sm,
        TextWeight::Medium,
    );
    PILL_PAD_X * 2.0 + PILL_DOT_R * 2.0 + PILL_DOT_GAP + f64::from(tw)
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
    let (tw, th) = measure(tr, label, size, TextWeight::Medium);
    let pad_x = 8.0;
    let width = pad_x * 2.0 + f64::from(tw);
    let rect = Rect::new(origin.x, origin.y, origin.x + width, origin.y + height);
    fill_round(scene, rect, 4.0, with_alpha(accent, TINT_FILL_ALPHA));
    stroke_round(
        scene,
        rect.inset(-0.5),
        3.5,
        1.0,
        with_alpha(accent, TINT_EDGE_ALPHA),
    );
    let ty = rect.center().y - f64::from(th) / 2.0;
    text(
        scene,
        tr,
        label,
        size,
        accent,
        Point::new(rect.x0 + pad_x, ty),
        TextWeight::Medium,
    );
    width
}

/// A horizontal progress/usage bar.
pub fn usage_bar(scene: &mut Scene, theme: &Theme, rect: Rect, fraction: f32, color: Color) {
    // The unfilled track is the theme's full lift step: a measured contrast
    // step against base and raised alike, rather than a ramp slot.
    let track = theme.tokens.colors.lift(1.0);
    fill_round(scene, rect, rect.height() / 2.0, track);
    let frac = f64::from(fraction.clamp(0.0, 1.0));
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

/// Draw a simple glyph-style icon (stroked vector) centered in `rect`.
/// The manager keeps icons primitive — a handful of recognisable marks —
/// so it has no asset dependency and re-colours with the theme. Split one
/// icon per helper so no single function collects too many single-letter
/// geometry bindings in scope at once.
pub fn icon(scene: &mut Scene, kind: Icon, rect: Rect, color: Color, width: f64) {
    let id = base();
    let c = rect.center();
    let r = rect.width().min(rect.height()) / 2.0;
    match kind {
        Icon::Box => icon_box(scene, id, c, r, color, width),
        Icon::Layers => icon_layers(scene, id, c, r, color, width),
        Icon::Terminal => icon_terminal(scene, id, c, r, color, width),
        Icon::Play => icon_play(scene, id, c, r, color),
        Icon::Stop => icon_stop(scene, id, c, r, color),
        Icon::Restart => icon_restart(scene, id, c, r, color, width),
        Icon::Trash => icon_trash(scene, id, c, r, color, width),
        Icon::Plus => icon_plus(scene, id, c, r, color, width),
        Icon::Search => icon_search(scene, id, c, r, color, width),
        Icon::Pause => icon_pause(scene, id, c, r, color),
        Icon::Enter => icon_enter(scene, id, c, r, color, width),
    }
}

/// A cube/box outline.
fn icon_box(scene: &mut Scene, id: Affine, c: Point, r: f64, color: Color, width: f64) {
    let s = Stroke::new(width);
    let b = Rect::from_center_size(c, (r * 1.6, r * 1.4));
    scene.stroke(&s, id, color, None, &RoundedRect::from_rect(b, 2.0));
    scene.stroke(
        &s,
        id,
        color,
        None,
        &Line::new(Point::new(b.x0, c.y), Point::new(b.x1, c.y)),
    );
}

/// Three stacked diamonds.
fn icon_layers(scene: &mut Scene, id: Affine, c: Point, r: f64, color: Color, width: f64) {
    let s = Stroke::new(width);
    for (i, dy) in [(-0.55_f64), 0.0, 0.55].iter().enumerate() {
        let oy = c.y + r * dy;
        let mut path = vello::kurbo::BezPath::new();
        path.move_to(Point::new(c.x, oy - r * 0.5));
        path.line_to(Point::new(c.x + r, oy));
        path.line_to(Point::new(c.x, oy + r * 0.5));
        path.line_to(Point::new(c.x - r, oy));
        path.close_path();
        let col = if i == 0 {
            color
        } else {
            with_alpha(color, 150)
        };
        scene.stroke(&s, id, col, None, &path);
    }
}

fn icon_terminal(scene: &mut Scene, id: Affine, c: Point, r: f64, color: Color, width: f64) {
    let s = Stroke::new(width);
    let b = Rect::from_center_size(c, (r * 1.8, r * 1.4));
    scene.stroke(&s, id, color, None, &RoundedRect::from_rect(b, 2.0));
    // prompt chevron
    scene.stroke(
        &s,
        id,
        color,
        None,
        &Line::new(
            Point::new(b.x0 + r * 0.35, c.y - r * 0.3),
            Point::new(b.x0 + r * 0.75, c.y),
        ),
    );
    scene.stroke(
        &s,
        id,
        color,
        None,
        &Line::new(
            Point::new(b.x0 + r * 0.75, c.y),
            Point::new(b.x0 + r * 0.35, c.y + r * 0.3),
        ),
    );
}

fn icon_play(scene: &mut Scene, id: Affine, c: Point, r: f64, color: Color) {
    let mut path = vello::kurbo::BezPath::new();
    path.move_to(Point::new(c.x - r * 0.5, c.y - r * 0.7));
    path.line_to(Point::new(c.x + r * 0.7, c.y));
    path.line_to(Point::new(c.x - r * 0.5, c.y + r * 0.7));
    path.close_path();
    scene.fill(Fill::NonZero, id, color, None, &path);
}

fn icon_stop(scene: &mut Scene, id: Affine, c: Point, r: f64, color: Color) {
    let b = Rect::from_center_size(c, (r * 1.1, r * 1.1));
    scene.fill(
        Fill::NonZero,
        id,
        color,
        None,
        &RoundedRect::from_rect(b, 2.0),
    );
}

fn icon_restart(scene: &mut Scene, id: Affine, c: Point, r: f64, color: Color, width: f64) {
    let arc = Circle::new(c, r * 0.8);
    scene.stroke(&Stroke::new(width), id, color, None, &arc);
    // arrow head
    let mut path = vello::kurbo::BezPath::new();
    let tip = Point::new(c.x + r * 0.8, c.y - r * 0.3);
    path.move_to(tip);
    path.line_to(Point::new(tip.x - r * 0.4, tip.y - r * 0.1));
    path.line_to(Point::new(tip.x - r * 0.05, tip.y + r * 0.4));
    path.close_path();
    scene.fill(Fill::NonZero, id, color, None, &path);
}

fn icon_trash(scene: &mut Scene, id: Affine, c: Point, r: f64, color: Color, width: f64) {
    let s = Stroke::new(width);
    let b = Rect::new(c.x - r * 0.6, c.y - r * 0.5, c.x + r * 0.6, c.y + r * 0.8);
    scene.stroke(&s, id, color, None, &RoundedRect::from_rect(b, 2.0));
    scene.stroke(
        &s,
        id,
        color,
        None,
        &Line::new(
            Point::new(c.x - r * 0.9, c.y - r * 0.5),
            Point::new(c.x + r * 0.9, c.y - r * 0.5),
        ),
    );
    scene.stroke(
        &s,
        id,
        color,
        None,
        &Line::new(
            Point::new(c.x - r * 0.3, c.y - r * 0.8),
            Point::new(c.x + r * 0.3, c.y - r * 0.8),
        ),
    );
}

fn icon_plus(scene: &mut Scene, id: Affine, c: Point, r: f64, color: Color, width: f64) {
    let s = Stroke::new(width);
    scene.stroke(
        &s,
        id,
        color,
        None,
        &Line::new(
            Point::new(c.x - r * 0.7, c.y),
            Point::new(c.x + r * 0.7, c.y),
        ),
    );
    scene.stroke(
        &s,
        id,
        color,
        None,
        &Line::new(
            Point::new(c.x, c.y - r * 0.7),
            Point::new(c.x, c.y + r * 0.7),
        ),
    );
}

fn icon_search(scene: &mut Scene, id: Affine, c: Point, r: f64, color: Color, width: f64) {
    let s = Stroke::new(width);
    let circle = Circle::new(Point::new(c.x - r * 0.2, c.y - r * 0.2), r * 0.55);
    scene.stroke(&s, id, color, None, &circle);
    scene.stroke(
        &s,
        id,
        color,
        None,
        &Line::new(
            Point::new(c.x + r * 0.25, c.y + r * 0.25),
            Point::new(c.x + r * 0.7, c.y + r * 0.7),
        ),
    );
}

fn icon_pause(scene: &mut Scene, id: Affine, c: Point, r: f64, color: Color) {
    let bar_w = r * 0.35;
    let bar_h = r * 1.3;
    for dx in [-r * 0.4, r * 0.4] {
        let b = Rect::from_center_size(Point::new(c.x + dx, c.y), (bar_w, bar_h));
        scene.fill(
            Fill::NonZero,
            id,
            color,
            None,
            &RoundedRect::from_rect(b, 1.5),
        );
    }
}

fn icon_enter(scene: &mut Scene, id: Affine, c: Point, r: f64, color: Color, width: f64) {
    let s = Stroke::new(width);
    // arrow pointing into a bracket
    scene.stroke(
        &s,
        id,
        color,
        None,
        &Line::new(
            Point::new(c.x - r * 0.7, c.y),
            Point::new(c.x + r * 0.4, c.y),
        ),
    );
    let mut path = vello::kurbo::BezPath::new();
    path.move_to(Point::new(c.x + r * 0.7, c.y));
    path.line_to(Point::new(c.x + r * 0.2, c.y - r * 0.4));
    path.move_to(Point::new(c.x + r * 0.7, c.y));
    path.line_to(Point::new(c.x + r * 0.2, c.y + r * 0.4));
    scene.stroke(&s, id, color, None, &path);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Token sizes apply once at 200%, status pills match their painted width, card edges register on every ground and tinted pills are edged in their hue.
    #[test]
    fn painted_metrics_and_edges_follow_tokens_and_hues() {
        // --- phase: token_sizes_are_applied_exactly_once_at_200_percent ---
        // A theme type token already carries the active text scale; the legacy
        // renderer-relative APIs scale again. `px` folds the token back once, so
        // a 28px token at 200% paints 28px glyphs (not 56px) and measures the
        // same width it paints.
        {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let token = theme.tokens.typography.scale.base;
            assert!((token - 28.0).abs() < 0.01, "base token is 14 * 2");
            assert!((px(&tr, token) - 14.0).abs() < 0.01);

            set_base(Affine::IDENTITY);
            let mut scene = Scene::new();
            let painted_w = text(
                &mut scene,
                &mut tr,
                "Containers",
                token,
                Color::WHITE,
                Point::ZERO,
                TextWeight::Semibold,
            );
            let runs = &scene.encoding().resources.glyph_runs;
            assert!(!runs.is_empty(), "text painted at least one glyph run");
            for run in runs {
                assert!(
                    (run.font_size - 28.0).abs() < 0.5,
                    "glyph run painted at {}px for a 28px token",
                    run.font_size
                );
            }
            let (measured_w, _) = measure(&mut tr, "Containers", token, TextWeight::Semibold);
            assert!(
                (painted_w - measured_w).abs() < 0.01,
                "measure and paint agree"
            );
            // And the unscaled renderer is untouched: a 14px token stays 14px.
            let plain = TextRenderer::new();
            assert!((px(&plain, 14.0) - 14.0).abs() < 0.01);
        }

        // --- phase: status_pill_width_matches_the_painted_pill ---
        {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            set_base(Affine::IDENTITY);
            let mut scene = Scene::new();
            let painted = status_pill(
                &mut scene,
                &mut tr,
                &theme,
                ContainerStatus::Running,
                ContainerStatus::Running.label(),
                Point::new(10.0, 10.0),
                20.0,
            );
            let predicted = status_pill_width(&mut tr, &theme, ContainerStatus::Running.label());
            assert!((painted - predicted).abs() < 0.01);
        }

        // --- phase: card_edges_register_on_every_containers_ground ---
        // HIG §9.5: every surface the manager closes with [`card`] — the base
        // log console, raised banners, lifted rows, the Panel ground of its
        // dialogs — gets an edge that visibly moves that surface on dark, light
        // and high-contrast alike. A declared `border_standard` did not.
        {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let c = &theme.tokens.colors;
                for (name, ground) in [
                    ("base", c.surfaces.base),
                    ("raised", c.surfaces.raised),
                    ("lift 0.35", c.lift(0.35)),
                    ("lift 0.72", c.lift(0.72)),
                    ("panel", ui::chrome::panel_ground(&theme)),
                ] {
                    let moved = ui::surface::edge_delta(edge(ground), ground);
                    assert!(
                        moved >= ui::tokens::colors::HAIRLINE_MIN_DELTA,
                        "{}: {name} edge moved only {moved:.4}",
                        theme.name
                    );
                }
            }
        }

        // --- phase: tinted_pills_and_chips_are_edged_in_their_own_hue ---
        // A tinted status pill / port chip is a closed shape: its edge is the
        // same hue as its fill at a higher alpha, so it never dissolves into a
        // raised ground the way a translucent fill alone did.
        {
            const { assert!(TINT_EDGE_ALPHA > TINT_FILL_ALPHA) };
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            set_base(Affine::IDENTITY);
            let mut scene = Scene::new();
            status_pill(
                &mut scene,
                &mut tr,
                &theme,
                ContainerStatus::Running,
                "Running",
                Point::new(10.0, 10.0),
                20.0,
            );
            // Fill, edge stroke, dot, glyph run: the stroke is the second path.
            let paths = scene.encoding().n_paths;
            assert!(
                paths >= 3,
                "pill paints a fill, an edge and a dot ({paths} paths)"
            );
        }
    }
}
