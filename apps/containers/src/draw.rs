//! Theme-driven drawing primitives shared across the manager's panels.
//!
//! Every helper here reads its colours and spacing from the passed-in
//! [`Theme`] — nothing hard-codes a colour — so the manager re-themes with
//! the rest of the OS. They paint directly into a Vello [`Scene`]; the app
//! widget owns hit-testing and state, these are pure paint.

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use crate::model::ContainerStatus;

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

/// Apply an explicit alpha (0..=255) to a colour.
pub fn with_alpha(c: Color, alpha: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), alpha)
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
    style: TextStyle,
    at: Point,
    max_width: f64,
) {
    let TextStyle {
        size,
        color,
        weight,
    } = style;
    let (w, _) = tr.measure_weighted(s, size, None, weight);
    if f64::from(w) <= max_width {
        tr.draw_text_weighted(
            scene,
            s,
            size,
            color,
            base() * Affine::translate((at.x, at.y)),
            None,
            weight,
        );
        return;
    }
    // Binary-search the longest prefix that fits with an ellipsis.
    let chars: Vec<char> = s.chars().collect();
    let ell = "…";
    let mut lo = 0usize;
    let mut hi = chars.len();
    let mut best = String::new();
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate: String = chars
            .get(..mid)
            .unwrap_or_default()
            .iter()
            .collect::<String>()
            + ell;
        let (cw, _) = tr.measure_weighted(&candidate, size, None, weight);
        if f64::from(cw) <= max_width {
            best = candidate;
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if best.is_empty() {
        best = ell.to_string();
    }
    tr.draw_text_weighted(
        scene,
        &best,
        size,
        color,
        base() * Affine::translate((at.x, at.y)),
        None,
        weight,
    );
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
    let width = pad_x * 2.0 + dot_r * 2.0 + dot_gap + f64::from(tw);
    let rect = Rect::new(origin.x, origin.y, origin.x + width, origin.y + height);
    fill_round(scene, rect, height / 2.0, with_alpha(color, 38));
    let cy = rect.center().y;
    let dot = Circle::new(Point::new(rect.x0 + pad_x + dot_r, cy), dot_r);
    scene.fill(Fill::NonZero, base(), color, None, &dot);
    let ty = cy - f64::from(size) / 2.0 - 1.0;
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
    let width = pad_x * 2.0 + f64::from(tw);
    let rect = Rect::new(origin.x, origin.y, origin.x + width, origin.y + height);
    fill_round(scene, rect, 4.0, with_alpha(accent, 30));
    let ty = rect.center().y - f64::from(size) / 2.0 - 1.0;
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
    let track = theme.tokens.colors.neutral[3];
    fill_round(scene, rect, rect.height() / 2.0, with_alpha(track, 120));
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
