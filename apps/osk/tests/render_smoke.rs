//! Offscreen render guard for [`OskWidget`]: drives the full
//! `build_layout` → `compute_layout` → `apply_layout` → draw pipeline and asserts
//! the keyboard rasterises substantial non-background content (surface +
//! toolbar + keycaps + labels).
//!
//! Skips gracefully when no wgpu adapter is available, matching the other
//! offscreen tests (e.g. `crates/sdk/ui-core/tests/icon_grid_test.rs`). Set
//! `TILO_UI_SCREENSHOT_DIR=<path>` to dump a PNG.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::cast_precision_loss,
    reason = "test viewport dims are small hard-coded literals (a few hundred px), always exactly representable in f32"
)]

use std::collections::HashMap;

use osk::OskWidget;
use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

fn try_new_renderer(w: u32, h: u32) -> Option<OffscreenRenderer> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        OffscreenRenderer::new(w, h)
    }))
    .ok()
}

#[test]
fn osk_renders_keys_and_toolbar() {
    let (w, h) = (720u32, 320u32);
    let Some(mut renderer) = try_new_renderer(w, h) else {
        eprintln!("no wgpu adapter — skipping osk_renders_keys_and_toolbar");
        return;
    };

    let mut osk = OskWidget::new();
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut tr = TextRenderer::new();

    let child = osk.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(w as f32),
                    height: Dimension::length(h as f32),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = node_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    Size::ZERO
                }
            },
        )
        .expect("taffy compute_layout");
    osk.apply_layout(&taffy, child, Point::ORIGIN);

    // The keyboard laid out a full grid of keys.
    assert!(
        osk.key_geometry().len() > 30,
        "expected a full key grid, got {}",
        osk.key_geometry().len()
    );
    assert_eq!(osk.toolbar_geometry().len(), 4, "default toolbar present");

    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    osk.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    // Distinct background so painted content (surface, caps, labels) differs.
    let bg = Color::from_rgb8(0xff, 0x00, 0xff);
    let rgba = renderer.render(&scene, bg);
    let bg_rgba = [0xff, 0x00, 0xff, 0xff];
    let non_bg = rgba.chunks_exact(4).filter(|px| px[..4] != bg_rgba).count();
    assert!(
        non_bg > 1000,
        "expected substantial painted content, got {non_bg} px"
    );

    if let Ok(dir) = ui::env::var("TILO_UI_SCREENSHOT_DIR") {
        let _ = std::fs::create_dir_all(&dir);
        let path = std::path::Path::new(&dir).join("osk.png");
        // Reuse the toolkit's PNG writer so the test needs no `png` dep.
        renderer.render_png(&scene, bg, &path);
        eprintln!("wrote {}", path.display());
    }
}
