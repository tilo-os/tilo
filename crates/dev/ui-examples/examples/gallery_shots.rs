//! Offscreen renders of the component gallery in **both** light and dark,
//! so the showcase can be self-inspected without opening a window. Drives the
//! real five-pass pipeline (`sync` → `build_layout` → `compute_layout` →
//! `apply_layout` → `draw`) on the same widget tree the interactive `gallery`
//! example builds.
//!
//! Run:  `cargo run -p ui-examples --example ui-shots -- gallery_shots [out-dir]`
//! Out:  `out-dir`, else `$GALLERY_SHOTS_OUT` (default `/tmp/gallery_shots`) — gallery-dark.png,
//!       gallery-light.png
//!
//! Fractional `HiDPI`: set `GALLERY_SHOTS_SCALE=1.5` (or 1.25 / 1.75 / 2.0)
//! to render at that display scale. This reproduces the exact live-window
//! path — a physical-pixel surface sized `logical × scale`, taffy solved
//! at the logical size, and the whole scene composited through
//! `Affine::scale(scale)` — so the PNG is crisp (vector-rasterized) at a
//! fractional factor rather than a blurry upscale of a 1× frame.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};

use tilo_ui::State;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::Widget;

use super::gallery_scene;

const WIDTH: u32 = 1200;
const HEIGHT: u32 = 2300;

fn shoot(theme: &Theme, out: &Path, scale: f64) {
    let mut state = State::new();
    let mut root = gallery_scene::build_showcase(&mut state, theme);

    let catalog = tilo_ui::font_catalog::FontCatalog::controlled_fixture(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/ui-core/assets/fonts"),
        "en-US",
    );
    #[allow(
        clippy::expect_used,
        reason = "gallery shots require the checked-in typography fixture"
    )]
    let mut tr =
        TextRenderer::with_font_catalog(&catalog).expect("checked-in typography font catalog");
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    // sync pulls the seeded control state out of the CRDT.
    root.sync(&mut state.doc, &state.root);

    // Layout is solved in LOGICAL space (the same WIDTH/HEIGHT regardless
    // of scale) — HiDPI must not change the logical layout, only how many
    // physical pixels each logical unit covers.
    let node = root.build_layout(&mut taffy, theme, &mut node_map);
    // WIDTH/HEIGHT are small fixed logical-pixel constants, far below
    // f32's exact-integer range.
    #[allow(
        clippy::cast_precision_loss,
        reason = "WIDTH/HEIGHT are small fixed logical-pixel constants, far below f32's exact-integer range"
    )]
    let _ = taffy.compute_layout_with_measure(
        node,
        Size {
            width: AvailableSpace::Definite(WIDTH as f32),
            height: AvailableSpace::Definite(HEIGHT as f32),
        },
        |known, available, node_id, _, _| {
            node_map.get(&node_id).map_or(Size::ZERO, |&p| unsafe {
                (*p).measure(known, available, &mut tr)
            })
        },
    );
    root.apply_layout(&taffy, node, Point::ZERO);

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    // Composite through a pure scale by the (possibly fractional) factor —
    // exactly what `App` does live with `Affine::scale(window.scale_factor())`.
    root.draw(&mut scene, Affine::scale(scale), &mut tr, theme, &presence);

    // The offscreen target is sized in PHYSICAL pixels: logical × scale.
    // WIDTH/HEIGHT → f64 is lossless; the rounded product is always a
    // small positive pixel count that fits u32.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rounded logical-pixels × scale is always a small positive pixel count"
    )]
    let (phys_w, phys_h) = (
        (f64::from(WIDTH) * scale).round() as u32,
        (f64::from(HEIGHT) * scale).round() as u32,
    );
    let mut r = OffscreenRenderer::new(phys_w, phys_h);
    r.render_png(&scene, theme.tokens.colors.surfaces.content, out);
    println!("wrote {} ({phys_w}x{phys_h} px @ {scale}x)", out.display());
}

pub fn run(args: &[String]) {
    tilo_ui::i18n::I18nManager::init("en-US");
    // The output directory: the first argument, then `$GALLERY_SHOTS_OUT`.
    let dir = args.first().cloned().unwrap_or_else(|| {
        std::env::var("GALLERY_SHOTS_OUT").unwrap_or_else(|_| "/tmp/gallery_shots".to_string())
    });
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("gallery_shots: failed to create output dir {dir}: {e}");
        return;
    }
    let dir = PathBuf::from(dir);

    // Fractional HiDPI factor from the environment (default 1.0). 1.5 / 1.25
    // / 1.75 exercise the crisp fractional-scale render path.
    let scale = std::env::var("GALLERY_SHOTS_SCALE")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|s| *s > 0.0)
        .unwrap_or(1.0);

    shoot(&Theme::dark(), &dir.join("gallery-dark.png"), scale);
    shoot(&Theme::light(), &dir.join("gallery-light.png"), scale);
}
