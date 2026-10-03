//! Offscreen renders of the component gallery at different **device scales** and
//! **user font sizes**, so fractional scaling can be self-inspected without a
//! real high-DPI display.
//!
//! Every shot drives the identical logical widget tree (the same one the
//! interactive `gallery` builds) through the real five-pass pipeline. The only
//! thing that changes between shots is:
//!   * the **device scale** — the physical buffer is `logical × scale` and the
//!     scene is drawn through `Affine::scale(device_scale)`, exactly as the App's
//!     render loop composites a fractional-scaled output; and
//!   * the **user scale** — a `Theme::with_ui_scale` / `with_text_scale` applied
//!     to the theme handed to the layout pass.
//!
//! At 1.5× the text and strokes are rasterised at 1.5× the pixels from the same
//! vector outlines — crisp, not a blurry upscale of the 1.0× raster.
//!
//! Run:  `cargo run -p ui-examples --example ui-shots -- fractional_scale_shots`
//! Out:  `$FRAC_SCALE_SHOTS_OUT` (default `/tmp/frac_scale_shots`)

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::Point;

use tilo_ui::State;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::scale::{composite_transform, physical_len};
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::Widget;

use super::gallery_scene;

/// Logical canvas size — fixed across shots; only the physical buffer grows
/// with the device scale.
const LOGICAL_W: f64 = 1200.0;
const LOGICAL_H: f64 = 1940.0;

/// Narrows a screen-space `f64` (rect extent or coordinate) to the `f32`
/// taffy layout math expects.
#[allow(
    clippy::cast_possible_truncation,
    reason = "on-screen layout coordinates never approach f32::MAX; this narrowing is exact for every practical UI extent"
)]
fn to_f32(v: f64) -> f32 {
    v as f32
}

/// Rounds a physical-pixel dimension to the nearest whole pixel. Every
/// caller passes a positive, finite `physical_len` result, so `v` is
/// always in `[0.0, u32::MAX]` — the `.clamp` here makes the
/// truncating/sign-losing cast provably safe regardless.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "v is clamped into [0.0, u32::MAX] on the line above, so the cast can never truncate or flip sign"
)]
fn px_dim(v: f64) -> u32 {
    v.round().clamp(0.0, f64::from(u32::MAX)) as u32
}

/// Render the showcase at a given device scale + themed with the given user
/// UI-scale / text-scale, and write a PNG.
fn shoot(base: &Theme, ui_scale: f32, text_scale: f32, device_scale: f64, out: &Path) {
    let theme = base
        .clone()
        .with_ui_scale(ui_scale)
        .with_text_scale(text_scale);

    let mut state = State::new();
    let mut root = gallery_scene::build_showcase(&mut state, &theme);

    let mut tr = TextRenderer::new();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    root.sync(&mut state.doc, &state.root);
    let node = root.build_layout(&mut taffy, &theme, &mut node_map);

    // Layout in LOGICAL px — device scale never enters the solver.
    let layout_result = taffy.compute_layout_with_measure(
        node,
        Size {
            width: AvailableSpace::Definite(to_f32(LOGICAL_W)),
            height: AvailableSpace::Definite(to_f32(LOGICAL_H)),
        },
        |known, available, node_id, _, _| {
            node_map.get(&node_id).map_or(Size::ZERO, |&p| unsafe {
                (*p).measure(known, available, &mut tr)
            })
        },
    );
    if let Err(e) = layout_result {
        eprintln!("fractional_scale_shots: layout failed: {e}");
        return;
    }
    root.apply_layout(&taffy, node, Point::ZERO);

    // Composite the logical scene at the device scale — crisp physical raster.
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    root.draw(
        &mut scene,
        composite_transform(device_scale),
        &mut tr,
        &theme,
        &presence,
    );

    let pw = px_dim(physical_len(LOGICAL_W, device_scale));
    let ph = px_dim(physical_len(LOGICAL_H, device_scale));
    let mut r = OffscreenRenderer::new(pw, ph);
    r.render_png(&scene, theme.bg_app, out);
    println!(
        "wrote {} ({}×{} px, device {device_scale}×, ui {ui_scale}×, text {text_scale}×)",
        out.display(),
        pw,
        ph
    );
}

pub fn run(_args: &[String]) {
    tilo_ui::i18n::I18nManager::init("en-US");
    let dir = std::env::var("FRAC_SCALE_SHOTS_OUT")
        .unwrap_or_else(|_| "/tmp/frac_scale_shots".to_string());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("failed to create output dir {dir}: {e}");
        return;
    }
    let dir = PathBuf::from(dir);
    let base = Theme::dark();

    // Same logical UI, two device scales: 1.0 vs 1.5 (fractional). The 1.5 shot
    // is 1.5× the pixels of the 1.0 shot, rasterised from the same outlines.
    shoot(&base, 1.0, 1.0, 1.0, &dir.join("gallery-device-1.0.png"));
    shoot(&base, 1.0, 1.0, 1.5, &dir.join("gallery-device-1.5.png"));

    // Two user font sizes at device 1.0 so the type-scale change is legible.
    shoot(&base, 1.0, 0.85, 1.0, &dir.join("gallery-text-small.png"));
    shoot(&base, 1.0, 1.30, 1.0, &dir.join("gallery-text-large.png"));

    // The composition: user UI-scale 1.25 AND a fractional 1.5 device scale.
    shoot(
        &base,
        1.25,
        1.0,
        1.5,
        &dir.join("gallery-ui1.25-device1.5.png"),
    );
}
