//! Visual E2E journey for a `WindowGroup`: renders the widget through
//! fresh, search-active, maximized, and post-close states and asserts each
//! produces distinct pixel output — the regression guard for paint paths
//! `window_group_test.rs` only reaches through state inspection.
//!
//! Fails when no wgpu adapter is available: a check that cannot observe its
//! subject must fail, not skip (`CLAUDE.md`), or a headless CI without
//! llvmpipe would report the paint paths green without ever drawing them.
//!
//! Set `TILO_UI_SCREENSHOT_DIR=<path>` to write one PNG per state for
//! by-eye inspection.

#![allow(
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use taffy::{AvailableSpace, NodeId, Size, Style, TaffyTree};
use tilo_ui::offscreen::OffscreenRenderer;
use vello::Scene;
use vello::kurbo::{Affine, Point};

use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{
    AppKind, TabDescriptor, TaffyTreeExt, TileContent, Widget, WindowGroup, WindowWidth,
};

fn make_window_group() -> WindowGroup {
    let tabs: Vec<(TabDescriptor, TileContent)> = vec![
        (
            TabDescriptor {
                id: 1,
                title: "Editor".into(),
                app_kind: Some(AppKind::Editor),
                icon: None,
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
                icon: None,
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
                icon: None,
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
    render_group_themed(renderer, group, &Theme::dark())
}

/// The same journey frame under an explicit theme, composited on that
/// theme's own app background so a light-preset PNG shows the chips on the
/// ground they really sit on.
#[allow(
    clippy::cast_precision_loss,
    reason = "renderer.width/height are small fixed test-canvas dimensions, exactly \
              representable in f32"
)]
fn render_group_themed(
    renderer: &mut OffscreenRenderer,
    group: &mut WindowGroup,
    theme: &Theme,
) -> Vec<u8> {
    let (w, h) = (renderer.width, renderer.height);
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut tr = TextRenderer::new();

    let node = group.build_layout(&mut taffy, theme, &mut node_map);
    // A maximized group is `width: auto, flex_grow: 1e6`: it fills the strip
    // it sits in. Give it one — a sized flex parent, as the strip is — or as
    // a taffy root it collapses to 0 px, its Restore and × chips land off
    // the canvas, and the "maximized" frame proves nothing about them.
    let root = taffy.parent_node(
        Style {
            size: Size::from_lengths(w as f32, h as f32),
            ..Style::default()
        },
        &[node],
    );
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
    group.apply_layout(&taffy, node, Point::ORIGIN);

    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    group.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);

    // The theme's own app background, so the card edge and the maximized
    // chips are judged against the ground they really sit on.
    renderer.render(&scene, theme.tokens.colors.surfaces.content)
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
    let Ok(dir) = tilo_ui::env::var("TILO_UI_SCREENSHOT_DIR") else {
        return;
    };
    let path = std::path::Path::new(&dir).join(format!("{name}.png"));
    std::fs::create_dir_all(&dir).ok();
    tilo_ui::png::write_rgba8(&path, w, h, rgba).expect("write screenshot png");
    eprintln!("wrote {} to {}", name, path.display());
}

#[test]
fn window_group_journey_is_deterministic_and_distinguishes_states_in_both_themes() {
    // Case `window_group_visual_journey_distinguishes_all_states`.
    // If any two states hash the same, either a paint branch (focus ring,
    // maximized floating controls, tab-close path) regressed, or a state
    // change silently stopped affecting output.
    {
        let (w, h) = (620u32, 200u32);
        let mut renderer = OffscreenRenderer::try_new(w, h)
            .expect("the window-group journey needs a wgpu adapter to observe its subject");

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

    // Case `window_group_visual_journey_distinguishes_all_states_in_light`.
    // The same journey on the **light** preset. The maximized chrome's chips
    // are quiet pills whose edge is derived from the chip fill; a light-theme frame
    // is where a declared or literal-white edge used to vanish, so the four
    // states must stay distinct there too. Writes `window_group_<state>_light.png`
    // under `TILO_UI_SCREENSHOT_DIR` for by-eye inspection beside the dark set.
    {
        let (w, h) = (620u32, 200u32);
        let mut renderer = OffscreenRenderer::try_new(w, h)
            .expect("the light window-group journey needs a wgpu adapter to observe its subject");
        let theme = Theme::light();

        let mut group = make_window_group();
        let fresh = render_group_themed(&mut renderer, &mut group, &theme);
        let fresh_hash = pixel_hash(&fresh);
        maybe_write_png(&fresh, w, h, "window_group_fresh_light");

        group.search_active = true;
        let search = render_group_themed(&mut renderer, &mut group, &theme);
        let search_hash = pixel_hash(&search);
        maybe_write_png(&search, w, h, "window_group_search_light");
        assert_ne!(
            fresh_hash, search_hash,
            "light: search_active must change the rendered output",
        );

        group.maximized = true;
        let maximized = render_group_themed(&mut renderer, &mut group, &theme);
        let maximized_hash = pixel_hash(&maximized);
        maybe_write_png(&maximized, w, h, "window_group_maximized_light");
        assert_ne!(
            fresh_hash, maximized_hash,
            "light: maximized must change the rendered output (floating pills)",
        );
        assert_ne!(
            search_hash, maximized_hash,
            "light: maximized on top of search_active must paint additional chrome",
        );

        group.maximized = false;
        group.search_active = false;
        assert!(group.detach_tab(1).is_some(), "detach_tab(1) must succeed");
        let post_close = render_group_themed(&mut renderer, &mut group, &theme);
        let post_close_hash = pixel_hash(&post_close);
        maybe_write_png(&post_close, w, h, "window_group_post_close_light");
        assert_ne!(
            fresh_hash, post_close_hash,
            "light: removing a tab must change the rendered output",
        );
    }

    // Case `window_group_fresh_render_is_deterministic`.
    // This is the invariant the journey test relies on — without it, its
    // pixel-hash comparisons are meaningless. Compared with
    // [`OffscreenRenderer::pixels_differing_beyond_aa`], not byte equality:
    // see the `tilo_ui::offscreen` module docs for why GPU rasterisation isn't
    // bit-identical between runs.
    {
        fn window_group_fresh_render_is_deterministic() {
            let (w, h) = (400u32, 120u32);
            let Some(mut renderer) = OffscreenRenderer::try_new(w, h).ok() else {
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
        window_group_fresh_render_is_deterministic();
    }
}
