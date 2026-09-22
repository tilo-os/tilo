//! Low-level Vello drawing helpers shared by the canvas and the panels.
//!
//! All colours come from the caller (resolved from the `Theme`); nothing
//! here hard-codes a palette. These are thin wrappers over `vello::Scene`
//! that keep the panel/canvas code declarative.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Circle, Join, Line, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

/// Linear blend between two sRGB colours (`t` in `0..=1`, 0 → a, 1 → b).
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let a = a.to_rgba8();
    let b = b.to_rgba8();
    let l = |x: u8, y: u8| {
        (f32::from(x) * (1.0 - t) + f32::from(y) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::from_rgba8(l(a.r, b.r), l(a.g, b.g), l(a.b, b.b), l(a.a, b.a))
}

/// Apply an alpha multiplier to a colour.
pub fn alpha(c: Color, a: f32) -> Color {
    c.multiply_alpha(a)
}

/// Parse a hex colour string (`rrggbb`, optional leading `#`) to a `Color`.
/// Used for authentic GTFS transit line colours (`routeColor`).
pub fn hex_color(s: &str) -> Option<Color> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(s.get(0..2)?, 16).ok()?;
    let g = u8::from_str_radix(s.get(2..4)?, 16).ok()?;
    let b = u8::from_str_radix(s.get(4..6)?, 16).ok()?;
    Some(Color::from_rgba8(r, g, b, 255))
}

pub fn fill_rect(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &rect);
}

pub fn fill_rrect(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &rect.to_rounded_rect(radius));
}

pub fn stroke_rrect(
    scene: &mut Scene,
    t: Affine,
    rect: Rect,
    radius: f64,
    width: f64,
    color: Color,
) {
    scene.stroke(
        &Stroke::new(width),
        t,
        color,
        None,
        &rect.to_rounded_rect(radius),
    );
}

pub fn fill_circle(scene: &mut Scene, t: Affine, center: Point, r: f64, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &Circle::new(center, r));
}

pub fn stroke_circle(
    scene: &mut Scene,
    t: Affine,
    center: Point,
    r: f64,
    width: f64,
    color: Color,
) {
    scene.stroke(&Stroke::new(width), t, color, None, &Circle::new(center, r));
}

pub fn line(scene: &mut Scene, t: Affine, a: Point, b: Point, width: f64, color: Color) {
    scene.stroke(&Stroke::new(width), t, color, None, &Line::new(a, b));
}

/// Stroke an open polyline with round caps/joins.
pub fn polyline(scene: &mut Scene, t: Affine, pts: &[Point], width: f64, color: Color) {
    if pts.len() < 2 {
        return;
    }
    let mut path = BezPath::new();
    let Some((&first, rest)) = pts.split_first() else {
        return;
    };
    path.move_to(first);
    for p in rest {
        path.line_to(*p);
    }
    let stroke = Stroke::new(width)
        .with_caps(Cap::Round)
        .with_join(Join::Round);
    scene.stroke(&stroke, t, color, None, &path);
}

/// Fill a prebuilt path (non-zero winding).
pub fn fill_path(scene: &mut Scene, t: Affine, path: &BezPath, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, path);
}

/// Stroke a prebuilt path with round caps/joins.
pub fn stroke_path(scene: &mut Scene, t: Affine, path: &BezPath, width: f64, color: Color) {
    let stroke = Stroke::new(width)
        .with_caps(Cap::Round)
        .with_join(Join::Round);
    scene.stroke(&stroke, t, color, None, path);
}

/// Fill a closed polygon ring.
pub fn polygon(scene: &mut Scene, t: Affine, pts: &[Point], color: Color) {
    if pts.len() < 3 {
        return;
    }
    let mut path = BezPath::new();
    let Some((&first, rest)) = pts.split_first() else {
        return;
    };
    path.move_to(first);
    for p in rest {
        path.line_to(*p);
    }
    path.close_path();
    scene.fill(Fill::NonZero, t, color, None, &path);
}

/// The faux drop-shadow every floating map surface casts: a slightly
/// larger, very transparent black plate seated a few pixels below.
fn floating_shadow(scene: &mut Scene, t: Affine, rect: Rect, radius: f64) {
    let shadow = Rect::new(rect.x0 - 1.0, rect.y0 + 3.0, rect.x1 + 1.0, rect.y1 + 5.0);
    fill_rrect(
        scene,
        t,
        shadow,
        radius + 1.0,
        Color::from_rgba8(0, 0, 0, 46),
    );
}

/// A soft elevated card floating over the map: shadow, fill, and the 1 px
/// rest edge *derived* from the fill (`ui::surface::card`, HIG §9.5) — never
/// a declared `border_standard`, which vanishes on a raised dark surface.
pub fn card(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, fill: Color) {
    floating_shadow(scene, t, rect, radius);
    ui::surface::card(scene, t, rect, radius, fill);
}

/// [`card`] whose edge carries a *state* rather than the rest hairline: the
/// focus ring on the search field, the accent ring on an active control.
/// The ring is stroked centred on the outline at `width`, like every
/// toolkit control's focus ring.
pub fn card_ring(
    scene: &mut Scene,
    t: Affine,
    rect: Rect,
    radius: f64,
    fill: Color,
    ring: Color,
    width: f64,
) {
    floating_shadow(scene, t, rect, radius);
    fill_rrect(scene, t, rect, radius, fill);
    stroke_rrect(scene, t, rect, radius, width, ring);
}

/// A row, field or track *inside* a panel: fill plus its derived rest edge,
/// no shadow (it sits on the panel, it does not float over the map).
pub fn edged(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, fill: Color) {
    ui::surface::card(scene, t, rect, radius, fill);
}

/// Alpha of the chosen row's wash: one step up `ui::surface::EDGE_ALPHA_STEPS`
/// from the hover hairline, so chosen and hovered never look alike.
const SELECTED_WASH_ALPHA: u8 = 40;

/// The chosen row of a list on the raised panel: a *lift* (HIG §9.3 — never
/// an accent wash) derived against the panel — white over a dark one, black
/// over a light one — with the panel's derived edge. The wash is
/// translucent, so the edge is judged against the panel it sits on (the
/// surface actually seen), not against the wash's own RGB.
pub fn selected_row(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, theme: &Theme) {
    let ground = theme.tokens.colors.surfaces.raised;
    fill_rrect(
        scene,
        t,
        rect,
        radius,
        ui::tokens::colors::Colors::hairline_on(ground, SELECTED_WASH_ALPHA),
    );
    scene.stroke(
        &Stroke::new(1.0),
        t,
        ui::surface::edge_on(ground),
        None,
        &ui::surface::edge_outline(rect, radius),
    );
}

/// A 1 px separator between groups inside a surface of colour `surface`,
/// derived against it and centred on `y` (`ui::surface::separator`).
pub fn separator(scene: &mut Scene, t: Affine, x0: f64, x1: f64, y: f64, surface: Color) {
    ui::surface::separator(scene, t, x0, x1, y, 0.0, surface);
}

/// Left-aligned text with top-left at `(x, y)`.
#[allow(
    clippy::too_many_arguments,
    reason = "shares the standard (scene, tr, t, ...) drawing-context params with every \
              other draw helper in this module; bundling them into a context struct is a \
              file-wide refactor out of scope here"
)]
pub fn text(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    s: &str,
    x: f64,
    y: f64,
    size: f32,
    color: Color,
) {
    tr.draw_text(scene, s, size, color, t * Affine::translate((x, y)), None);
}

/// Weighted left-aligned text.
#[allow(
    clippy::too_many_arguments,
    reason = "shares the standard (scene, tr, t, ...) drawing-context params with every \
              other draw helper in this module; bundling them into a context struct is a \
              file-wide refactor out of scope here"
)]
pub fn text_w(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    s: &str,
    x: f64,
    y: f64,
    size: f32,
    color: Color,
    weight: TextWeight,
) {
    tr.draw_text_weighted(
        scene,
        s,
        size,
        color,
        t * Affine::translate((x, y)),
        None,
        weight,
    );
}

/// Left-aligned text clipped to `max_w` (ellipsised by cosmic-text wrap).
#[allow(
    clippy::too_many_arguments,
    reason = "shares the standard (scene, tr, t, ...) drawing-context params with every \
              other draw helper in this module; bundling them into a context struct is a \
              file-wide refactor out of scope here"
)]
pub fn text_clip(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    s: &str,
    x: f64,
    y: f64,
    size: f32,
    color: Color,
    max_w: f64,
) {
    let s = ellipsize(tr, s, size, max_w);
    tr.draw_text(
        scene,
        &s,
        size,
        color,
        t * Affine::translate((x, y)),
        Some(max_w as f32 + 1.0),
    );
}

/// Right-aligned text with the right edge at `x_right`.
#[allow(
    clippy::too_many_arguments,
    reason = "shares the standard (scene, tr, t, ...) drawing-context params with every \
              other draw helper in this module; bundling them into a context struct is a \
              file-wide refactor out of scope here"
)]
pub fn text_right(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    s: &str,
    x_right: f64,
    y: f64,
    size: f32,
    color: Color,
) {
    let (w, _) = tr.measure(s, size, None);
    tr.draw_text(
        scene,
        s,
        size,
        color,
        t * Affine::translate((x_right - f64::from(w), y)),
        None,
    );
}

/// Centre text horizontally within `[x0, x1]`.
#[allow(
    clippy::too_many_arguments,
    reason = "shares the standard (scene, tr, t, ...) drawing-context params with every \
              other draw helper in this module; bundling them into a context struct is a \
              file-wide refactor out of scope here"
)]
#[allow(
    clippy::many_single_char_names,
    reason = "conventional short names for the shared drawing context (t: transform) and the \
              text baseline geometry (x0, x1, y) used consistently across this module"
)]
pub fn text_center(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    s: &str,
    x0: f64,
    x1: f64,
    y: f64,
    size: f32,
    color: Color,
) {
    let (w, _) = tr.measure(s, size, None);
    let x = x0 + ((x1 - x0) - f64::from(w)) / 2.0;
    tr.draw_text(scene, s, size, color, t * Affine::translate((x, y)), None);
}

/// Measure text width.
pub fn measure_w(tr: &mut TextRenderer, s: &str, size: f32) -> f64 {
    f64::from(tr.measure(s, size, None).0)
}

/// Hard-truncate `s` with an ellipsis so it fits within `max_w` at `size`.
pub fn ellipsize(tr: &mut TextRenderer, s: &str, size: f32, max_w: f64) -> String {
    if measure_w(tr, s, size) <= max_w {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        let trial = format!("{out}{ch}…");
        if measure_w(tr, &trial, size) > max_w {
            break;
        }
        out.push(ch);
    }
    // The ellipsis is appended either way — even a first character that
    // already overflows still yields a visible truncation marker.
    out.push('…');
    out
}
