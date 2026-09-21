//! Offscreen renders of [`tilo_ui::surface::soft_shadow`] at every geometry the
//! toolkit and the shell chrome actually ask for, so the primitive can be
//! inspected without hunting for a scene that happens to contain the
//! shape you changed.
//!
//! The point of the sheet is *aspect ratio*: the same `spread` has to
//! produce the same falloff under a 1022×235 overview card, a 360×68
//! toast, a 44×44 wheel spoke and a 22×26 close pill. Anything that ties
//! the falloff to the rect's own dimensions shows up here immediately as
//! a shadow that is fat on one surface and invisible on the next.
//!
//! Run:  `cargo run -p ui --example ui-shots -- soft_shadow_shots`
//! Out:  `$SOFT_SHADOW_SHOTS_OUT` (default `/tmp/soft_shadow_shots`) —
//!       shadow-sheet.png, plus one shadow-<name>.png per case.
//! Theme: `QOS_SHOTS_THEME=dark|light` (dark by default) — the sheet paints
//!       on that theme's base surface, so a shadow that only registers on a
//!       dark ground shows up here as a light-theme sheet with no shadows.

use std::path::PathBuf;

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::surface::soft_shadow;
use tilo_ui::theme::Theme;

/// `(name, width, height, spread, offset_y, intensity, radius)` — the
/// live call sites, in their own numbers.
const CASES: &[(&str, f64, f64, f64, f64, f32, f64)] = &[
    // `overlay_host`'s overview row: the surface whose accent glow was
    // invisible because the falloff scaled with its 1022 px width.
    ("overview-card", 1022.0, 235.0, 40.0, 0.0, 0.35, 14.0),
    // `notification`'s toast stack.
    ("toast", 360.0, 68.0, 18.0, 6.0, 0.5, 12.0),
    // The command palette / a settings modal.
    ("palette", 640.0, 470.0, 44.0, 14.0, 0.55, 16.0),
    // Small, near-square wheel-disc spoke card.
    ("spoke-card", 44.0, 44.0, 14.0, 0.0, 0.35, 10.0),
    // A window group's title pill, and its close glyph: the smallest
    // shadows in the toolkit, both shorter than twice their spread.
    ("title-pill", 96.0, 26.0, 10.0, 2.0, 0.35, 13.0),
    ("close-pill", 22.0, 26.0, 10.0, 2.0, 0.35, 11.0),
];

fn out_dir() -> PathBuf {
    std::env::var("SOFT_SHADOW_SHOTS_OUT")
        .map_or_else(|_| PathBuf::from("/tmp/soft_shadow_shots"), PathBuf::from)
}

/// The palette the sheet paints under: `QOS_SHOTS_THEME`, dark by default.
fn shot_theme() -> Theme {
    std::env::var("QOS_SHOTS_THEME")
        .ok()
        .and_then(|name| Theme::named(&name))
        .unwrap_or_else(Theme::dark)
}

/// Rounds a canvas dimension up to the nearest whole pixel. Every caller
/// passes a positive, finite sum of case geometry, so `v` is always in
/// `[0.0, u32::MAX]` — the `.clamp` here makes the truncating/sign-losing
/// cast provably safe regardless.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "v is clamped into [0.0, u32::MAX] on the line above, so the cast can never truncate or flip sign"
)]
fn px_dim(v: f64) -> u32 {
    v.ceil().clamp(0.0, f64::from(u32::MAX)) as u32
}

/// Paint one case centred in `canvas`: the shadow through the production
/// primitive, then the card itself on top, so what you see is what a
/// surface sitting on that shadow looks like.
// Standard immediate-mode paint-helper signature (scene + geometry + the
// shadow's own tunable parameters); every parameter is independently
// load-bearing for this one shot case.
#[allow(
    clippy::too_many_arguments,
    reason = "each shadow shot varies canvas and card dimensions, spread, offset, intensity and radius with one theme"
)]
fn paint(
    scene: &mut Scene,
    theme: &Theme,
    canvas: Rect,
    width: f64,
    height: f64,
    spread: f64,
    off: f64,
    intensity: f32,
    radius: f64,
) {
    let c = canvas.center();
    let rect = Rect::new(
        c.x - width / 2.0,
        c.y - height / 2.0,
        c.x + width / 2.0,
        c.y + height / 2.0,
    );
    // The shadow's hue is the elevation token's (black on every preset), not
    // `neutral[0]`: light themes run their ramp base → text, so that slot is
    // the paper itself and the shadow disappeared with it. The card is the
    // shared `surface::card`, so it carries its derived rest edge exactly as
    // the live surfaces do.
    let tint = theme.tokens.elevation.e3.color;
    // …at the authored intensity scaled the way the elevation ladder scales
    // a light theme's shadows, which is what the live call sites now do.
    let intensity = intensity * theme.tokens.elevation.shadow_scale;
    soft_shadow(scene, Affine::IDENTITY, rect, tint, spread, off, intensity);
    tilo_ui::surface::card(
        scene,
        Affine::IDENTITY,
        rect,
        radius,
        theme.tokens.colors.surfaces.raised,
    );
}

pub fn run(_args: &[String]) {
    let dir = out_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("failed to create output dir {}: {e}", dir.display());
        return;
    }
    let theme = shot_theme();
    let bg = theme.tokens.colors.surfaces.base;
    println!("soft_shadow_shots ({}) -> {}", theme.name, dir.display());

    // Per-case shots, each on a canvas with a full spread of margin so
    // the whole falloff is visible.
    for (name, w, h, spread, off, i, r) in CASES {
        let (cw, ch) = (
            px_dim(w + spread * 4.0),
            px_dim(h + spread * 4.0 + off * 2.0),
        );
        let mut scene = Scene::new();
        let canvas = Rect::new(0.0, 0.0, f64::from(cw), f64::from(ch));
        paint(&mut scene, &theme, canvas, *w, *h, *spread, *off, *i, *r);
        let mut renderer = OffscreenRenderer::new(cw, ch);
        let path = dir.join(format!("shadow-{name}.png"));
        renderer.render_png(&scene, bg, &path);
        println!("  wrote {}", path.display());
    }

    // …and one sheet stacking every case on the same background, so the
    // falloffs can be compared against each other rather than against a
    // memory of the last screenshot. Each cell is sized to its own case —
    // a fixed grid puts the palette's 470 px card through the row above.
    let cells: Vec<(f64, f64)> = CASES
        .iter()
        .map(|(_, w, h, spread, off, _, _)| (w + spread * 4.0, h + spread * 4.0 + off * 2.0))
        .collect();
    let sw = cells.iter().map(|(w, _)| *w).fold(0.0, f64::max);
    let sh: f64 = cells.iter().map(|(_, h)| *h).sum();
    let mut scene = Scene::new();
    let mut y = 0.0;
    for ((_, w, h, spread, off, i, r), (_, ch)) in CASES.iter().zip(&cells) {
        paint(
            &mut scene,
            &theme,
            Rect::new(0.0, y, sw, y + ch),
            *w,
            *h,
            *spread,
            *off,
            *i,
            *r,
        );
        y += ch;
    }
    let mut renderer = OffscreenRenderer::new(px_dim(sw), px_dim(sh));
    let path = dir.join("shadow-sheet.png");
    renderer.render_png(&scene, bg, &path);
    println!("  wrote {}", path.display());
}
