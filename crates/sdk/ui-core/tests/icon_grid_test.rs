//! Offscreen render guard for [`IconGrid`]: drives the full
//! `build_layout` → measure → `apply_layout` → draw pipeline and asserts the
//! grid rasterises non-empty pixels (icons + labels + a selection fill).
//!
//! Skips gracefully when no wgpu adapter is available, matching the other
//! offscreen tests. Set `TILO_UI_SCREENSHOT_DIR=<path>` to dump a PNG.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::support::offscreen::OffscreenRenderer;
use std::collections::HashMap;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

use tilo_ui::icons::Icon;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::selection::SelectionModel;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{IconGrid, IconGridItem, Widget};

fn try_new_renderer(w: u32, h: u32) -> Option<OffscreenRenderer> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        OffscreenRenderer::new(w, h)
    }))
    .ok()
}

#[test]
#[allow(
    clippy::cast_precision_loss,
    reason = "w/h are small fixed test-canvas dimensions, exactly representable in f32"
)]
fn icon_grid_renders_cells_and_selection() {
    let (w, h) = (520u32, 360u32);
    let Some(mut renderer) = try_new_renderer(w, h) else {
        eprintln!("no wgpu adapter — skipping icon_grid_renders_cells_and_selection");
        return;
    };

    let items = vec![
        IconGridItem::new(Icon::Folder, "Documents"),
        IconGridItem::new(Icon::Folder, "Pictures"),
        IconGridItem::new(Icon::Image, "a-really-long-photo-name.png"),
        IconGridItem::new(Icon::FileCode, "main.rs"),
        IconGridItem::new(Icon::Music, "track.flac"),
        IconGridItem::new(Icon::Film, "movie.mkv"),
        IconGridItem::new(Icon::Archive, "bundle.zip"),
        IconGridItem::new(Icon::FileText, "notes.md"),
    ];
    let mut sel = SelectionModel::new();
    sel.select_only(2); // highlight the long-name image
    let mut grid = IconGrid::new(items).with_selection(sel);

    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut tr = TextRenderer::new();

    let root = grid.build_layout(&mut taffy, &theme, &mut node_map);
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
    grid.apply_layout(&taffy, root, Point::ORIGIN);

    // 520px wide ÷ 108px stride → 4 columns.
    assert_eq!(
        grid.column_count(),
        4,
        "grid reflowed to 4 columns at 520px"
    );

    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    grid.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    let bg = Color::from_rgb8(0x0f, 0x17, 0x2a);
    let rgba = renderer.render(&scene, bg);

    // Some pixels must differ from the background (icons/labels/selection).
    let bg_rgba = [0x0f, 0x17, 0x2a, 0xff];
    let non_bg = rgba.chunks_exact(4).filter(|px| px[..4] != bg_rgba).count();
    assert!(
        non_bg > 1000,
        "expected substantial painted content, got {non_bg} px"
    );

    if let Ok(dir) = tilo_ui::env::var("TILO_UI_SCREENSHOT_DIR") {
        let _ = std::fs::create_dir_all(&dir);
        let path = format!("{dir}/icon_grid.png");
        tilo_ui::png::write_rgba8(&path, w, h, &rgba).unwrap();
        eprintln!("wrote {path}");
    }
}
