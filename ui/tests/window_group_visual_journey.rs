//! Visual E2E journey for a `WindowGroup`: renders the widget through
//! fresh, search-active, maximized, and post-close states and asserts each
//! produces distinct pixel output — the regression guard for paint paths
//! `window_group_test.rs` only reaches through state inspection.
//!
//! Skips gracefully (no panic) when no wgpu adapter is available, so
//! headless CI without llvmpipe stays green.
//!
//! Set `QUICK_UI_SCREENSHOT_DIR=<path>` to write one PNG per state for
//! by-eye inspection.

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

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{AppKind, TabDescriptor, TileContent, Widget, WindowGroup, WindowWidth};

fn try_new_renderer(w: u32, h: u32) -> Option<OffscreenRenderer> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        OffscreenRenderer::new(w, h)
    }))
    .ok()
}

fn make_window_group() -> WindowGroup {
    let tabs: Vec<(TabDescriptor, TileContent)> = vec![
        (
            TabDescriptor {
                id: 1,
                title: "Editor".into(),
                app_kind: Some(AppKind::Editor),
            },
            TileContent::Stub {
                kind: AppKind::Editor,
                label: "Editor".into(),
            },
        ),
        (
            TabDescriptor {
                id: 2,
                title: "Files".into(),
                app_kind: Some(AppKind::Files),
            },
            TileContent::Stub {
                kind: AppKind::Files,
                label: "Files".into(),
            },
        ),
        (
            TabDescriptor {
                id: 3,
                title: "Term".into(),
                app_kind: Some(AppKind::Terminal),
            },
            TileContent::Stub {
                kind: AppKind::Terminal,
                label: "Terminal".into(),
            },
        ),
    ];
    WindowGroup::new(42, tabs, 1, WindowWidth::Normal)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "renderer.width/height are small fixed test-canvas dimensions, exactly \
              representable in f32"
)]
fn render_group(renderer: &mut OffscreenRenderer, group: &mut WindowGroup) -> Vec<u8> {
    let (w, h) = (renderer.width, renderer.height);
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut tr = TextRenderer::new();

    let root = group.build_layout(&mut taffy, &theme, &mut node_map);
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
    group.apply_layout(&taffy, root, Point::ORIGIN);

    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    group.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    // Dark slate base so overpainted accents / chrome read as distinct
    // from the desktop background.
    let bg = Color::from_rgb8(0x0f, 0x17, 0x2a);
    renderer.render(&scene, bg)
}

/// Cheap 64-bit FNV-1a over the full RGBA buffer — not a golden-image
/// comparison, just a fingerprint good enough to detect any pixel-level
/// divergence between two renders.
fn pixel_hash(buf: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in buf {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn maybe_write_png(rgba: &[u8], w: u32, h: u32, name: &str) {
    let Ok(dir) = std::env::var("QUICK_UI_SCREENSHOT_DIR") else {
        return;
    };
    let path = std::path::Path::new(&dir).join(format!("{name}.png"));
    std::fs::create_dir_all(&dir).ok();
    let file = std::fs::File::create(&path).expect("create screenshot png");
    let mut bw = std::io::BufWriter::new(file);
    let mut enc = png::Encoder::new(&mut bw, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().expect("png header");
    writer.write_image_data(rgba).expect("png write");
    eprintln!("wrote {} to {}", name, path.display());
}

/// If any two states hash the same, either a paint branch (focus ring,
/// maximized floating controls, tab-close path) regressed, or a state
/// change silently stopped affecting output.
#[test]
fn window_group_visual_journey_distinguishes_all_states() {
    let (w, h) = (620u32, 200u32);
    let Some(mut renderer) = try_new_renderer(w, h) else {
        eprintln!("offscreen: no wgpu adapter available — skipping window-group journey");
        return;
    };

    let mut group = make_window_group();
    let fresh = render_group(&mut renderer, &mut group);
    let fresh_hash = pixel_hash(&fresh);
    maybe_write_png(&fresh, w, h, "window_group_fresh");

    // Should paint the indigo ring/glow pair around the whole card.
    group.search_active = true;
    let search = render_group(&mut renderer, &mut group);
    let search_hash = pixel_hash(&search);
    maybe_write_png(&search, w, h, "window_group_search");
    assert_ne!(
        fresh_hash, search_hash,
        "search_active must change the rendered output (focus ring + glow)",
    );

    // search_active stays on — both can coexist. Adds the 2px outline +
    // floating title pill + Restore + × pills.
    group.maximized = true;
    let maximized = render_group(&mut renderer, &mut group);
    let maximized_hash = pixel_hash(&maximized);
    maybe_write_png(&maximized, w, h, "window_group_maximized");
    assert_ne!(
        fresh_hash, maximized_hash,
        "maximized must change the rendered output (outline + floating pills)",
    );
    assert_ne!(
        search_hash, maximized_hash,
        "maximized on top of search_active must paint additional chrome",
    );

    // Turn overlays back off and remove the active tab — exercises the
    // close path's layout-spring start and the `remove_tab` branch.
    group.maximized = false;
    group.search_active = false;
    let detached = group.detach_tab(1);
    assert!(detached.is_some(), "detach_tab(1) must succeed");
    let post_close = render_group(&mut renderer, &mut group);
    let post_close_hash = pixel_hash(&post_close);
    maybe_write_png(&post_close, w, h, "window_group_post_close");
    assert_ne!(
        fresh_hash, post_close_hash,
        "removing a tab must change the rendered output (one fewer pill)",
    );
}

/// This is the invariant the journey test relies on — without it, its
/// pixel-hash comparisons are meaningless. Compared with
/// [`OffscreenRenderer::pixels_differing_beyond_aa`], not byte equality:
/// see the `ui::offscreen` module docs for why GPU rasterisation isn't
/// bit-identical between runs.
#[test]
fn window_group_fresh_render_is_deterministic() {
    let (w, h) = (400u32, 120u32);
    let Some(mut renderer) = try_new_renderer(w, h) else {
        eprintln!("offscreen: no wgpu adapter available — skipping determinism check");
        return;
    };
    let mut g1 = make_window_group();
    let mut g2 = make_window_group();
    let a = render_group(&mut renderer, &mut g1);
    let b = render_group(&mut renderer, &mut g2);
    assert_eq!(
        OffscreenRenderer::pixels_differing_beyond_aa(&a, &b),
        0,
        "two fresh WindowGroup renders must produce the same image",
    );
}
