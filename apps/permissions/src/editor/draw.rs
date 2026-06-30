//! Shared drawing primitives for the editor panes.
//!
//! Every text size here is read against the OS typography scale
//! (`theme.tokens.typography.scale`): `xl=20` (titles), `lg=16`
//! (sub-titles), `base=14` (body), `sm=12` (secondary), `xs=10`
//! (the smallest caption). These match the contacts/settings house style
//! — the previous editor rendered everything ~2pt smaller and read as
//! miniature beside the other system apps.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::ColorCompat;

use crate::check::Source;
use crate::model::{Decision, Grant};

/// Width of one allow/deny/inherit segment. Widened from the old 56 so the
/// glyphs + captions have room to breathe at the larger type sizes.
pub const SEG_W: f64 = 60.0;

pub fn packed_color(rgb: u32) -> Color {
    Color::from_rgb8(((rgb >> 16) & 0xff) as u8, ((rgb >> 8) & 0xff) as u8, (rgb & 0xff) as u8)
}

/// Standard body/title/caption sizes pulled from the theme so every pane
/// renders consistently and the OS can retune type centrally.
pub struct Sizes {
    pub title: f32,    // screen / detail title  (xl, 20)
    pub body: f32,     // row + grid body text   (base, 14)
    pub secondary: f32, // sub-row text          (sm, 12)
    pub caption: f32,  // smallest labels        (11 — one notch above the 10 xs floor)
}

impl Sizes {
    pub fn from_theme(theme: &Theme) -> Self {
        let s = &theme.tokens.typography.scale;
        Sizes {
            title: s.xl,            // 20
            body: s.base,           // 14
            secondary: s.sm,        // 12
            caption: s.sm - 1.0,    // 11 — one notch above the 10 xs floor
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn seg_button(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    active: bool,
) {
    let c = &theme.tokens.colors;
    let sz = Sizes::from_theme(theme);
    let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
    if active {
        scene.fill(Fill::NonZero, t, theme.accent_primary, None, &shape);
    } else {
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &shape);
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
    }
    let col = if active { Color::WHITE } else { c.text_primary };
    let tw = tr.measure(label, sz.body, None).0 as f64;
    tr.draw_text_weighted(
        scene,
        label,
        sz.body,
        col,
        t * Affine::translate((rect.center().x - tw / 2.0, rect.center().y - sz.body as f64 / 2.0)),
        None,
        TextWeight::Medium,
    );
}

pub fn toolbar_button(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    primary: bool,
) {
    if rect.width() <= 0.0 {
        return;
    }
    let c = &theme.tokens.colors;
    let sz = Sizes::from_theme(theme);
    let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
    if primary {
        scene.fill(Fill::NonZero, t, theme.accent_primary, None, &shape);
    } else {
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &shape);
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
    }
    let col = if primary { Color::WHITE } else { c.text_primary };
    let tw = tr.measure(label, sz.body, None).0 as f64;
    tr.draw_text_weighted(
        scene,
        label,
        sz.body,
        col,
        t * Affine::translate((rect.center().x - tw / 2.0, rect.center().y - sz.body as f64 / 2.0)),
        None,
        TextWeight::Medium,
    );
}

pub fn danger_button(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
) {
    let c = &theme.tokens.colors;
    let sz = Sizes::from_theme(theme);
    let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
    scene.fill(Fill::NonZero, t, c.semantic.danger.with_alpha_factor(0.16), None, &shape);
    scene.stroke(&Stroke::new(1.0), t, c.semantic.danger, None, &shape);
    let tw = tr.measure(label, sz.body, None).0 as f64;
    tr.draw_text_weighted(
        scene,
        label,
        sz.body,
        c.semantic.danger,
        t * Affine::translate((rect.center().x - tw / 2.0, rect.center().y - sz.body as f64 / 2.0)),
        None,
        TextWeight::Medium,
    );
}

pub fn role_chip(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    color: Color,
    held: bool,
) {
    let c = &theme.tokens.colors;
    let sz = Sizes::from_theme(theme);
    let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
    let bg = if held { color.with_alpha_factor(0.22) } else { c.surfaces.raised };
    scene.fill(Fill::NonZero, t, bg, None, &shape);
    let border = if held { color } else { c.border_standard };
    scene.stroke(&Stroke::new(if held { 1.6 } else { 1.0 }), t, border, None, &shape);
    // Check box.
    let box_r = Rect::new(rect.x0 + 10.0, rect.center().y - 8.0, rect.x0 + 26.0, rect.center().y + 8.0);
    let bsh = RoundedRect::from_rect(box_r, 3.0);
    if held {
        scene.fill(Fill::NonZero, t, color, None, &bsh);
        let cx = box_r.center();
        let mut p = vello::kurbo::BezPath::new();
        p.move_to((cx.x - 4.0, cx.y));
        p.line_to((cx.x - 1.0, cx.y + 3.0));
        p.line_to((cx.x + 4.0, cx.y - 3.0));
        scene.stroke(&Stroke::new(1.8), t, Color::WHITE, None, &p);
    } else {
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &bsh);
    }
    tr.draw_text(
        scene,
        label,
        sz.body,
        c.text_primary,
        t * Affine::translate((box_r.x1 + 8.0, rect.center().y - sz.body as f64 / 2.0)),
        Some((rect.x1 - box_r.x1 - 14.0) as f32),
    );
}

pub fn seg_caption(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, seg_x: f64, y: f64) {
    let c = &theme.tokens.colors;
    let sz = Sizes::from_theme(theme);
    for (i, cap) in ["Allow", "Inherit", "Deny"].iter().enumerate() {
        let cx = seg_x + i as f64 * SEG_W + SEG_W / 2.0;
        let tw = tr.measure(cap, sz.caption, None).0 as f64;
        tr.draw_text(scene, cap, sz.caption, c.text_dim, t * Affine::translate((cx - tw / 2.0, y)), None);
    }
}

/// Draw a three-segment Allow | Inherit | Deny control, highlighting the
/// active segment.
pub fn tri_state(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, seg_x: f64, y: f64, value: Grant, row_h: f64) {
    let c = &theme.tokens.colors;
    let h = row_h - 8.0;
    let track = Rect::new(seg_x, y, seg_x + SEG_W * 3.0, y + h);
    let shape = RoundedRect::from_rect(track, theme.tokens.radii.sm as f64);
    scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &shape);
    scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
    let (idx, fill) = match value {
        Grant::Allow => (0, c.semantic.success),
        Grant::Inherit => (1, c.text_dim),
        Grant::Deny => (2, c.semantic.danger),
    };
    let seg = Rect::new(seg_x + idx as f64 * SEG_W, y, seg_x + (idx as f64 + 1.0) * SEG_W, y + h);
    let segsh = RoundedRect::from_rect(seg.inset(-1.5), theme.tokens.radii.sm as f64);
    scene.fill(
        Fill::NonZero,
        t,
        fill.with_alpha_factor(if matches!(value, Grant::Inherit) { 0.18 } else { 0.9 }),
        None,
        &segsh,
    );
    let sz = Sizes::from_theme(theme);
    for (i, glyph) in ["\u{2713}", "\u{2014}", "\u{2715}"].iter().enumerate() {
        let cx = seg_x + i as f64 * SEG_W + SEG_W / 2.0;
        let on = i == idx;
        let col = if on && !matches!(value, Grant::Inherit) {
            Color::WHITE
        } else if on {
            c.text_primary
        } else {
            c.text_dim.with_alpha_factor(0.5)
        };
        let tw = tr.measure(glyph, sz.body, None).0 as f64;
        tr.draw_text(scene, glyph, sz.body, col, t * Affine::translate((cx - tw / 2.0, track.center().y - sz.body as f64 / 2.0)), None);
    }
}

/// A badge showing the resolved effective decision + its source.
pub fn effective_badge(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, at: Point, decision: Decision, source: &Source) {
    let c = &theme.tokens.colors;
    let sz = Sizes::from_theme(theme);
    let (label, col) = match decision {
        Decision::Allow => ("ALLOW", c.semantic.success),
        Decision::Deny => ("DENY", c.semantic.danger),
    };
    let w = 86.0;
    let r = Rect::new(at.x, at.y, at.x + w, at.y + 20.0);
    let sh = RoundedRect::from_rect(r, 4.0);
    scene.fill(Fill::NonZero, t, col.with_alpha_factor(0.18), None, &sh);
    tr.draw_text_weighted(scene, label, sz.caption, col, t * Affine::translate((r.x0 + 7.0, r.y0 + 3.0)), None, TextWeight::Bold);
    let src = match source {
        Source::MemberOverride => "ovr",
        Source::Role(_) => "role",
        Source::Default => "def",
    };
    tr.draw_text(scene, src, sz.caption, c.text_dim, t * Affine::translate((r.x0 + 52.0, r.y0 + 3.0)), None);
}

pub fn menu_panel(scene: &mut Scene, t: Affine, theme: &Theme, panel: Rect) {
    let c = &theme.tokens.colors;
    let shadow = RoundedRect::from_rect(panel.inset(2.0), theme.tokens.radii.md as f64 + 1.0);
    scene.fill(Fill::NonZero, t, Color::from_rgba8(0, 0, 0, 90), None, &shadow);
    let shape = RoundedRect::from_rect(panel, theme.tokens.radii.md as f64);
    scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &shape);
    scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
}

#[allow(clippy::too_many_arguments)]
pub fn menu_item(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rect: Rect, label: &str, hover: bool, selected: bool) {
    let c = &theme.tokens.colors;
    let sz = Sizes::from_theme(theme);
    if hover {
        scene.fill(Fill::NonZero, t, theme.accent_primary, None, &RoundedRect::from_rect(rect, theme.tokens.radii.sm as f64));
    } else if selected {
        scene.fill(Fill::NonZero, t, theme.accent_primary.with_alpha_factor(0.16), None, &RoundedRect::from_rect(rect, theme.tokens.radii.sm as f64));
    }
    let col = if hover { Color::WHITE } else { c.text_primary };
    tr.draw_text(scene, label, sz.body, col, t * Affine::translate((rect.x0 + 12.0, rect.center().y - sz.body as f64 / 2.0)), Some((rect.width() - 20.0) as f32));
}

pub fn color_grid_geometry(anchor: Rect, palette_len: usize) -> (Rect, Vec<Rect>) {
    let cols = 4;
    let cell = 30.0;
    let pad = 8.0;
    let rows = palette_len.div_ceil(cols);
    let w = cols as f64 * cell + pad * 2.0;
    let h = rows as f64 * cell + pad * 2.0;
    let panel = Rect::new(anchor.x0, anchor.y1 + 4.0, anchor.x0 + w, anchor.y1 + 4.0 + h);
    let mut swatches = Vec::new();
    for i in 0..palette_len {
        let r = (i / cols) as f64;
        let col = (i % cols) as f64;
        let x = panel.x0 + pad + col * cell;
        let y = panel.y0 + pad + r * cell;
        swatches.push(Rect::new(x + 2.0, y + 2.0, x + cell - 2.0, y + cell - 2.0));
    }
    (panel, swatches)
}
