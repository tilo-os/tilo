//! Low-level Vello drawing helpers shared by every view.
//!
//! These are thin wrappers over `vello::Scene` primitives plus the
//! `TextRenderer`. Everything takes a `transform` (the host-supplied affine)
//! and every colour is passed in by the caller from the `Theme` — nothing
//! here hard-codes a palette.

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use vello::Scene;
use vello::kurbo::{
    Affine, Arc, BezPath, Cap, Circle, Line, Point, Rect, RoundedRect, Stroke, Vec2,
};
use vello::peniko::{Color, Fill};

pub fn fill_rect(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &rect);
}

pub fn fill_rrect(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, color: Color) {
    let shape = RoundedRect::from_rect(rect, radius);
    scene.fill(Fill::NonZero, t, color, None, &shape);
}

pub fn stroke_rrect(
    scene: &mut Scene,
    t: Affine,
    rect: Rect,
    radius: f64,
    color: Color,
    width: f64,
) {
    let shape = RoundedRect::from_rect(rect, radius);
    scene.stroke(&Stroke::new(width), t, color, None, &shape);
}

/// Filled rounded rect with its derived 1px rest edge — the canonical
/// "card" treatment, routed through `ui::surface::card` so the edge comes
/// from the fill it sits on (HIG §9.5) rather than a declared colour.
pub fn card(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, fill: Color) {
    ui::surface::card(scene, t, rect, radius, fill);
}

/// A 1px rule between rows inside `surface`, derived against it and inset
/// `inset` from both ends (`ui::surface::separator`).
pub fn separator(
    scene: &mut Scene,
    t: Affine,
    x0: f64,
    x1: f64,
    y: f64,
    inset: f64,
    surface: Color,
) {
    ui::surface::separator(scene, t, x0, x1, y, inset, surface);
}

pub fn hline(scene: &mut Scene, t: Affine, x0: f64, x1: f64, y: f64, color: Color, width: f64) {
    scene.stroke(
        &Stroke::new(width),
        t,
        color,
        None,
        &Line::new((x0, y), (x1, y)),
    );
}

pub fn dot(scene: &mut Scene, t: Affine, center: Point, r: f64, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &Circle::new(center, r));
}

pub fn ring_dot(scene: &mut Scene, t: Affine, center: Point, r: f64, fill: Color, ring: Color) {
    scene.fill(Fill::NonZero, t, ring, None, &Circle::new(center, r + 2.5));
    scene.fill(Fill::NonZero, t, fill, None, &Circle::new(center, r));
}

// These leaf adapters deliberately mirror Vello's immediate-mode vocabulary:
// target, renderer, transform, content, style, then geometry. The values are
// independent per draw and carry no domain invariant; wrapping each call's
// coordinates and colours in one-use structs would add ceremony without making
// invalid states unrepresentable. Higher-level view helpers use typed contexts.
#[allow(
    clippy::too_many_arguments,
    reason = "leaf graphics adapter exposes independent Vello text and anchor values"
)]
/// Left-top anchored text on a single line.
pub fn text(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x: f64,
    y: f64,
) {
    tr.draw_text(scene, s, size, color, t * Affine::translate((x, y)), None);
}

#[allow(
    clippy::too_many_arguments,
    reason = "leaf graphics adapter exposes independent Vello text and anchor values"
)]
pub fn text_bold(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x: f64,
    y: f64,
) {
    tr.draw_text_weighted(
        scene,
        s,
        size,
        color,
        t * Affine::translate((x, y)),
        None,
        TextWeight::Bold,
    );
}

#[allow(
    clippy::too_many_arguments,
    reason = "leaf graphics adapter exposes independent Vello text and anchor values"
)]
/// Text whose right edge sits at `x_right`.
pub fn text_right(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x_right: f64,
    y: f64,
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

#[allow(
    clippy::too_many_arguments,
    reason = "leaf graphics adapter exposes independent Vello text and bounds values"
)]
/// Text centred horizontally within `[x0, x1]`.
pub fn text_center(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x0: f64,
    x1: f64,
    y: f64,
) {
    let (w, _) = tr.measure(s, size, None);
    let cx = x0 + ((x1 - x0) - f64::from(w)) / 2.0;
    tr.draw_text(scene, s, size, color, t * Affine::translate((cx, y)), None);
}

#[allow(
    clippy::too_many_arguments,
    reason = "leaf graphics adapter exposes independent Vello text and row values"
)]
/// Vertically-centred left text within a row of height `h` starting at `y0`.
pub fn text_vc(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x: f64,
    y0: f64,
    h: f64,
) {
    let (_, th) = tr.measure(s, size, None);
    let ty = y0 + (h - f64::from(th)) / 2.0;
    tr.draw_text(scene, s, size, color, t * Affine::translate((x, ty)), None);
}

/// Vertical padding a pill adds around its measured label. Both pill styles
/// share it so a type pill and a retention tag on one row have one height.
const PILL_PAD_Y: f64 = 3.0;

/// Draw a pill (rounded chip) with a centred label. Returns the pill width so
/// callers can lay out chips left-to-right. The pill is vertically centred on
/// `cy`; its height comes from the label the renderer will paint, so it grows
/// with the accessibility text scale instead of clipping the glyphs.
#[allow(
    clippy::too_many_arguments,
    reason = "The pill painter takes its scene, text renderer, transform, position, label size, and foreground/background colors independently."
)] // one arg per independent knob; see `text` above.
pub fn pill(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    x: f64,
    cy: f64,
    label: &str,
    size: f32,
    fg: Color,
    bg: Color,
) -> f64 {
    let (tw, th) = tr.measure(label, size, None);
    let pad = 8.0;
    let h = f64::from(th) + PILL_PAD_Y * 2.0;
    let w = f64::from(tw) + pad * 2.0;
    let rect = Rect::new(x, cy - h / 2.0, x + w, cy + h / 2.0);
    // A tinted capsule: the tint is its contrast step against the row.
    ui::surface::pill(scene, t, rect, bg, None, None);
    let ty = rect.y0 + PILL_PAD_Y;
    tr.draw_text(
        scene,
        label,
        size,
        fg,
        t * Affine::translate((x + pad, ty)),
        None,
    );
    w
}

/// Pill with a translucent fill derived from `accent` plus a matching
/// stroke — the "tag" look used for retention classes.
#[allow(
    clippy::too_many_arguments,
    reason = "leaf graphics adapter exposes independent label, anchor, and accent values"
)]
pub fn tag_pill(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    x: f64,
    cy: f64,
    label: &str,
    size: f32,
    accent: Color,
) -> f64 {
    let (tw, th) = tr.measure(label, size, None);
    let pad = 8.0;
    let h = f64::from(th) + PILL_PAD_Y * 2.0;
    let w = f64::from(tw) + pad * 2.0;
    let rect = Rect::new(x, cy - h / 2.0, x + w, cy + h / 2.0);
    ui::surface::pill(
        scene,
        t,
        rect,
        accent.with_alpha_factor(0.18),
        Some(accent.with_alpha_factor(0.55)),
        None,
    );
    let ty = rect.y0 + PILL_PAD_Y;
    tr.draw_text(
        scene,
        label,
        size,
        accent,
        t * Affine::translate((x + pad, ty)),
        None,
    );
    w
}

/// Horizontal capacity bar: a rounded track with a proportional fill.
pub fn bar(scene: &mut Scene, t: Affine, rect: Rect, frac: f64, fill: Color, track: Color) {
    let r = rect.height() / 2.0;
    fill_rrect(scene, t, rect, r, track);
    let frac = frac.clamp(0.0, 1.0);
    if frac > 0.001 {
        let w = (rect.width() * frac).max(rect.height());
        let fr = Rect::new(rect.x0, rect.y0, rect.x0 + w, rect.y1);
        fill_rrect(scene, t, fr, r, fill);
    }
}

/// Circular gauge: a faint full ring plus an arc sweeping `frac` of the way
/// round from 12 o'clock, clockwise.
#[allow(
    clippy::too_many_arguments,
    reason = "leaf graphics adapter exposes independent ring geometry and palette values"
)]
pub fn ring(
    scene: &mut Scene,
    t: Affine,
    center: Point,
    radius: f64,
    thickness: f64,
    frac: f64,
    fill: Color,
    track: Color,
) {
    scene.stroke(
        &Stroke::new(thickness),
        t,
        track,
        None,
        &Circle::new(center, radius),
    );
    let frac = frac.clamp(0.0, 1.0);
    if frac > 0.001 {
        let start = -std::f64::consts::FRAC_PI_2;
        let sweep = std::f64::consts::TAU * frac;
        let arc = Arc::new(center, Vec2::new(radius, radius), start, sweep, 0.0);
        scene.stroke(
            &Stroke::new(thickness).with_caps(Cap::Round),
            t,
            fill,
            None,
            &arc,
        );
    }
}

/// Stroke an arbitrary poly-line path (used for the small vector icons).
pub fn poly(
    scene: &mut Scene,
    t: Affine,
    pts: &[(f64, f64)],
    color: Color,
    width: f64,
    closed: bool,
) {
    if pts.len() < 2 {
        return;
    }
    let mut p = BezPath::new();
    let Some((&first, rest)) = pts.split_first() else {
        return;
    };
    p.move_to(first);
    for pt in rest {
        p.line_to(*pt);
    }
    if closed {
        p.close_path();
    }
    scene.stroke(
        &Stroke::new(width).with_caps(Cap::Round),
        t,
        color,
        None,
        &p,
    );
}
