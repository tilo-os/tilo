//! End-to-end proof that an external Wayland client's pixels reach the
//! screen inside its tab, driving the production chain (no bespoke render
//! path for the test):
//!
//! ```text
//!   tab_binding::adopt_toplevel        (what XdgShellHandler::new_toplevel calls)
//!     → ChromeState::bump               (revision → strip resync)
//!     → FrameBuilder::relayout          (splices the tab, publishes its rect)
//!     → shell::to_ui_tile               (Foreign → ForeignSurfaceWidget)
//!     → ForeignFrameStore               (what CompositorHandler::commit fills)
//!     → FrameBuilder::paint             (the same scene the compositor submits)
//!     → OffscreenRenderer               (rasterises that scene)
//! ```
//!
//! Assertions are on rasterised pixels and the size the compositor would
//! put in an `xdg_toplevel.configure`, not intermediate bookkeeping.
//! Set `TILO_SHOT_DIR` to also drop PNGs of every rendered frame.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]

use shell::compositor::foreign::{self, ForeignSurfaceWidget, store};
use shell::compositor::frame::FrameBuilder;
use shell::compositor::tab_binding;
use shell::shell::state::WorkspaceTree;
use ui::num::{i32_of_f64, u32_of_f64};
use ui::offscreen::OffscreenRenderer;
use ui::widget::Widget;
use vello::peniko::Color;

const W: u32 = 1600;
const H: u32 = 900;

/// Surface ids are the process-wide key into the frame store, so each
/// test picks its own to stay independent under parallel execution.
fn shot(name: &str, renderer: &mut OffscreenRenderer, scene: &vello::Scene) -> Vec<u8> {
    let rgba = renderer.render(scene, Color::BLACK);
    if let Ok(dir) = std::env::var("TILO_SHOT_DIR") {
        let path = std::path::Path::new(&dir).join(format!("{name}.png"));
        renderer.render_png(scene, Color::BLACK, &path);
        eprintln!("wrote {}", path.display());
    }
    rgba
}

/// One production frame: the per-frame state pump (`sync` + `update`) then
/// layout + paint, in the order `compositor::native::run` does it. The pump
/// is not optional — adopting a client focuses its new group and `update`
/// pans the strip to bring it into view, so without it the tile lands off
/// the render surface and every pixel assertion samples nothing.
fn frame(fb: &mut FrameBuilder) -> vello::Scene {
    let mut doc = automerge::AutoCommit::new();
    let root = automerge::ROOT;
    fb.pump_apps(&mut doc, &root);
    assert_eq!(
        fb.chrome().overlays,
        shell::shell::chrome_state::OverlayFlags::default(),
        "the compositing fixture must not obscure the client with a shell overlay"
    );
    assert!(
        !fb.host().palette_visible() && !fb.host().onboarding_active(),
        "the compositing fixture must not obscure the client with a palette or onboarding"
    );
    assert!(
        !shell::shell::input_policy::chrome_captures_input(fb),
        "the compositing fixture must leave the client interactive"
    );
    fb.paint_settled()
}

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "chrome's only use is producing rendered via tree_mut(); the write-guard's \
lifetime is tied to chrome's borrow, so it can't be dropped before rendered's \
last use, and the block already ends right there"
)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn foreign_buffers_are_composited_once_into_visible_tabs_only() {
    // Phase `output_presentation_buffer_reaches_a_nonblack_output_frame`.
    {
        let surface_id = 0x70_0000_0003;
        store().forget(surface_id);
        let pixels = solid(4, 4, (0x19, 0xC8, 0x5A));
        // This is the same decoded frame store that `State::commit` populates;
        // direct output paint must not depend on a WorkspaceTree widget.
        store().publish_rgba(surface_id, &pixels, 4, 4);
        assert_eq!(
            store().snapshot(surface_id).expect("captured frame").rgba,
            pixels
        );
        let mut scene = vello::Scene::new();
        foreign::paint_output_presentation_surface(
            &mut scene,
            surface_id,
            vello::kurbo::Rect::new(0.0, 0.0, 80.0, 80.0),
            &ui::Theme::dark(),
        );
        let mut renderer = OffscreenRenderer::new(80, 80);
        let rgba = renderer.render(&scene, Color::BLACK);
        let pixel = OffscreenRenderer::sample(&rgba, 80, 40, 40);
        assert!(
            pixel.0.abs_diff(0x19) <= 4
                && pixel.1.abs_diff(0xC8) <= 4
                && pixel.2.abs_diff(0x5A) <= 4,
            "direct audience output must paint the committed client frame, got {pixel:?}"
        );
        store().forget(surface_id);
    }

    // Phase `client_buffer_pixels_are_composited_into_the_tab_rect`.
    // The load-bearing test: a client's buffer bytes become pixels inside the
    // tab's rectangle in the scene the compositor actually submits.
    {
        let surface_id = 0x5EED_0001;
        store().forget(surface_id);

        let mut fb = adopt_into_frame_builder(surface_id, "org.example.client");
        // Splice, pan and spawn fade must all finish before the published tab
        // rect is the one the client will be configured to.
        settle(&mut fb);
        let scene = frame(&mut fb);
        let mut renderer = OffscreenRenderer::new(W, H);
        let before = shot("foreign-before-first-frame", &mut renderer, &scene);

        let rect = tile_rect(&mut fb, surface_id);
        assert!(
            rect.width() > 100.0 && rect.height() > 100.0,
            "the foreign tile must occupy real area; got {rect:?}"
        );
        let (cx, cy) = (u32_of_f64(rect.center().x), u32_of_f64(rect.center().y));

        let viewport = store()
            .viewport(surface_id)
            .expect("apply_layout must publish the tab rect into the frame store");
        assert_eq!(
            viewport,
            (
                u32_of_f64(rect.width().round()),
                u32_of_f64(rect.height().round())
            ),
            "the published viewport is the tab's on-screen extent"
        );

        // No frame yet: whatever is on screen, it is NOT the client's colour.
        let pre = OffscreenRenderer::sample(&before, W, cx, cy);
        assert_ne!(
            (pre.0, pre.1, pre.2),
            (0xE0, 0x18, 0xA0),
            "test fixture: the placeholder must not already be the client colour"
        );

        // `publish_rgba` is the same entry point the dmabuf import path uses
        // after decoding.
        let (bw, bh) = viewport;
        let rgba = solid(bw, bh, (0xE0, 0x18, 0xA0));
        store().publish_rgba(surface_id, &rgba, bw, bh);

        // A new buffer is edge-triggered via needs_redraw (one shot). Treating
        // a pending sequence as animation cadence pinned idle off-screen tiles
        // at the refresh rate; the native loop also wakes on frame_seq.
        let mut probe = ForeignSurfaceWidget::new(surface_id, "org.example.client".into());
        probe.place_floating(rect);
        Widget::mark_drawn(&mut probe);
        assert!(
            Widget::needs_redraw(&probe),
            "a freshly committed client frame must request a repaint"
        );
        assert!(
            !Widget::needs_animation_frame(&probe),
            "a fitting buffer must not pin animation cadence after presentation settles"
        );

        // The dissolve is 180 ms, so the very next frame is still mostly card.
        quiesce(&mut fb, surface_id);
        let scene = frame(&mut fb);
        let after = shot("foreign-with-client-frame", &mut renderer, &scene);
        // The focused foreign group can still finish its strip pan while its
        // opening card is up. Sample the rect painted by this frame, not the
        // pre-commit position captured above.
        let rect = tile_rect(&mut fb, surface_id);
        let (cx, cy) = (u32_of_f64(rect.center().x), u32_of_f64(rect.center().y));
        let px = OffscreenRenderer::sample(&after, W, cx, cy);
        // The compositor may paint transient collaboration/chrome ink at the
        // exact centre. Prove that the client owns the tile as an area, rather
        // than treating one overlaid pixel as the whole compositing contract.
        let after_pixels = &after;
        let samples: Vec<_> = [0.25, 0.5, 0.75]
            .into_iter()
            .flat_map(|fx| {
                [0.25, 0.5, 0.75].into_iter().map(move |fy| {
                    OffscreenRenderer::sample(
                        after_pixels,
                        W,
                        u32_of_f64(rect.x0 + rect.width() * fx),
                        u32_of_f64(rect.y0 + rect.height() * fy),
                    )
                })
            })
            .collect();
        let client_pixels = samples
            .iter()
            .filter(|sample| {
                sample.0.abs_diff(0xE0) <= 4
                    && sample.1.abs_diff(0x18) <= 4
                    && sample.2.abs_diff(0xA0) <= 4
            })
            .count();
        assert!(
            client_pixels >= 5,
            "the client's pixels must fill the tab; only {client_pixels}/9 interior samples matched \
             (centre rgba{px:?} at ({cx}, {cy}), samples {samples:?}, rect {rect:?})"
        );

        // A point well outside the tile keeps the shell's own chrome.
        let outside_y = u32_of_f64(rect.y0).saturating_sub(30).max(1);
        let out = OffscreenRenderer::sample(&after, W, cx, outside_y);
        assert!(
            !(out.0.abs_diff(0xE0) <= 4 && out.1.abs_diff(0x18) <= 4 && out.2.abs_diff(0xA0) <= 4),
            "client pixels must be clipped to the tab; leaked to ({cx}, {outside_y}): rgba{out:?}"
        );

        store().forget(surface_id);
    }

    // Phase `painting_does_not_reimport_the_clients_buffer`.
    // Painting is free: turning a client's buffer into RGBA is the expensive
    // part and must happen once per *commit*, never once per *paint*. A
    // dmabuf import is a GPU readback, and the compositor paints far more
    // often than a client commits, so an import hung off the paint loop
    // would turn one idle GPU client into permanent load.
    {
        let surface_id = 0x5EED_0007;
        store().forget(surface_id);

        let mut fb = adopt_into_frame_builder(surface_id, "org.example.steady");
        settle(&mut fb);
        let (bw, bh) = store().viewport(surface_id).expect("tab laid out");

        // `publish_rgba` is the publish both import routes end in.
        let before = store().import_count();
        store().publish_rgba(surface_id, &solid(bw, bh, (0x30, 0x90, 0xD0)), bw, bh);

        // That dissolve is a legitimate 180 ms animation, so "the client is idle"
        // is only answerable once it has finished.
        quiesce(&mut fb, surface_id);

        // Paint repeatedly with no further commits, as the loop does while the
        // client sits idle.
        for _ in 0..30 {
            let _ = frame(&mut fb);
        }

        assert_eq!(
            store().import_count(),
            before,
            "painting re-imported the client buffer; import must be driven by \
             the commit hook, not by the paint loop"
        );
        assert!(
            !fb.needs_animation_frame(),
            "an idle client kept asking for animation frames after its frame was drawn"
        );

        store().forget(surface_id);
    }

    // Phase `a_background_foreign_tab_neither_paints_nor_wakes_the_paint_loop`.
    // A tab that is not the active one in its group must not paint its
    // client's pixels — and, because `WindowGroup` only consults the active
    // host, must not keep the compositor's paint loop awake either.
    {
        let front = 0x5EED_0005;
        let back = 0x5EED_0006;
        store().forget(front);
        store().forget(back);

        let mut fb = FrameBuilder::new((W, H));
        // Two clients as two tabs of ONE group, `front` active. The second names
        // the group the first landed in (how a document opened from another app
        // is placed); adopting twice with default placement would give each its
        // own group, leaving nothing in the background.
        {
            let chrome = fb.chrome();
            let mut rendered = chrome.tree_mut();
            tab_binding::adopt_toplevel(&mut rendered, back, Some("org.example.back".into()));
            let group = rendered
                .desktop
                .active_strip
                .get(&rendered.desktop.active)
                .copied()
                .and_then(|strip| rendered.desktop.active_group_for(strip))
                .expect("the first adoption focuses the group it landed in");
            tab_binding::adopt_toplevel_at(
                &mut rendered,
                front,
                Some("org.example.front".into()),
                tab_binding::Placement::TabIn(group),
            );
            assert!(
                foreign::is_visible_tab(&rendered, front),
                "the tab adopted last is the group's active one"
            );
            assert!(
                !foreign::is_visible_tab(&rendered, back),
                "the tab it was adopted beside is now in the background"
            );
        }
        create_fixture_tab_records(&fb);
        fb.with_chrome(shell::shell::chrome_state::ChromeState::bump);
        settle(&mut fb);

        let rect = tile_rect(&mut fb, front);
        let (cx, cy) = (u32_of_f64(rect.center().x), u32_of_f64(rect.center().y));

        let (bw, bh) = store()
            .viewport(front)
            .expect("the visible tab publishes its rect");

        // Settle the *visible* tab first: otherwise the front tab is still a
        // placeholder painting a spinner, which correctly keeps the compositor
        // repainting for reasons unrelated to the background tab.
        store().publish_rgba(front, &solid(bw, bh, (0x40, 0x40, 0x40)), bw, bh);
        quiesce(&mut fb, front);

        let rgba = solid(bw, bh, (0x11, 0x9E, 0xE0));
        store().publish_rgba(back, &rgba, bw, bh);

        assert!(
            !fb.needs_animation_frame(),
            "a background tab's frame must not force the compositor to repaint"
        );

        let mut renderer = OffscreenRenderer::new(W, H);
        let scene = frame(&mut fb);
        let pixels = shot("foreign-background-tab", &mut renderer, &scene);
        let px = OffscreenRenderer::sample(&pixels, W, cx, cy);
        assert!(
            !(px.0.abs_diff(0x11) <= 4 && px.1.abs_diff(0x9E) <= 4 && px.2.abs_diff(0xE0) <= 4),
            "a background client's frame leaked into the active tab; got rgba{px:?}"
        );

        store().forget(front);
        store().forget(back);
    }
}

/// Closing the final window must preserve the empty desktop on every produced
/// frame, including the first frame before any closing animation has settled.
/// This checks scene production and retained rasterisation, not physical KMS.
#[test]
fn last_window_close_keeps_every_produced_frame_nonblack() {
    let surface_id = 0x70_0000_0031;
    store().forget(surface_id);
    let mut tree = WorkspaceTree::bootstrap_default();
    tab_binding::adopt_toplevel(
        &mut tree,
        surface_id,
        Some("org.example.last-window".into()),
    );
    let workspace = tree.desktop.active;
    let mut fb = FrameBuilder::with_tree(tree, (W, H));
    create_fixture_tab_records(&fb);
    settle(&mut fb);
    let rect = tile_rect(&mut fb, surface_id);
    let pixels = solid(
        u32_of_f64(rect.width()),
        u32_of_f64(rect.height()),
        (0xE0, 0x18, 0xA0),
    );
    store().publish_rgba(
        surface_id,
        &pixels,
        u32_of_f64(rect.width()),
        u32_of_f64(rect.height()),
    );
    quiesce(&mut fb, surface_id);

    let mut retained = OffscreenRenderer::new(W, H);
    let mut oracle = OffscreenRenderer::new(W, H);
    fb.relayout();
    let before = fb.paint_with_damage();
    let before_pixels = retained.render_resilient(&before.scene, Color::BLACK);
    let backdrop = OffscreenRenderer::sample(&before_pixels, W, W - 8, H - 8);
    assert!(backdrop.0 > 0 || backdrop.1 > 0 || backdrop.2 > 0);
    assert_eq!(backdrop.3, 255);
    fb.with_chrome(|chrome| {
        shell::shell::window_actions::close_active_tab(chrome).expect("ordinary close lifecycle");
    });
    let mut doc = automerge::AutoCommit::new();
    for frame_index in 0..16 {
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        let painted = fb.paint_with_damage();
        {
            let chrome = fb.chrome();
            let tree = chrome.tree();
            assert_eq!(tree.desktop.active, workspace, "close must retain Home");
            assert_eq!(tree.desktop.workspaces.len(), 1);
            assert!(
                tree.desktop.workspaces[0]
                    .strips
                    .iter()
                    .all(|strip| strip.window_groups.is_empty()),
                "close must not fabricate a replacement window"
            );
        }
        assert!(fb.foreign_surface_rect(surface_id).is_none());
        if let Some(regions) = &painted.damage_regions {
            let rectangles = regions
                .iter()
                .map(|r| {
                    vello::kurbo::Rect::new(
                        f64::from(r.x),
                        f64::from(r.y),
                        f64::from(r.x.saturating_add(r.width)),
                        f64::from(r.y.saturating_add(r.height)),
                    )
                })
                .collect::<Vec<_>>();
            retained.render_damage_regions_gpu_wait(&painted.scene, Color::BLACK, &rectangles);
        } else {
            retained.render_gpu_wait(&painted.scene, Color::BLACK);
        }
        let actual = retained.readback_current();
        let expected = oracle.render_resilient(&painted.scene, Color::BLACK);
        assert!(
            actual
                .iter()
                .zip(&expected)
                .all(|(a, b)| a.abs_diff(*b) <= 2),
            "retained frame {frame_index} must match its exact full-render oracle"
        );
        assert_eq!(
            OffscreenRenderer::sample(&actual, W, W - 8, H - 8),
            backdrop,
            "frame {frame_index} must preserve the painted desktop backdrop"
        );
        assert!(
            actual
                .chunks_exact(4)
                .filter(|pixel| pixel[0] > 0 || pixel[1] > 0 || pixel[2] > 0)
                .count()
                > usize::try_from(W * H / 2).expect("viewport fits usize"),
            "frame {frame_index} must never become a black output"
        );
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    store().forget(surface_id);
}

/// Paint until the widget tree reaches a steady state.
///
/// Splicing a tab plays the spawn fade, which legitimately keeps
/// `needs_animation_frame()` true for a couple hundred ms. A still-opening
/// tile paints a spinner and never reports idle while it does (by design),
/// so "steady" means "nothing animating, or only a tab still starting" —
/// a test needing to tell those apart must get its client to present first.
fn settle(fb: &mut FrameBuilder) {
    // Always paint once: the first pass is what splices pending tabs into
    // the live tree and lays them out, so it must run even when nothing
    // is animating yet.
    let _ = frame(fb);
    let started = std::time::Instant::now();
    let minimum = started
        .checked_add(std::time::Duration::from_millis(500))
        .unwrap_or(started);
    let deadline = tilo_test_env::hang_deadline();
    let settled = |fb: &mut FrameBuilder| !fb.needs_animation_frame() || fb.any_tile_opening();
    while (std::time::Instant::now() < minimum || !settled(fb))
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(8));
        let _ = frame(fb);
    }
    assert!(
        settled(fb),
        "the widget tree never settled within the hang bound; an animation is stuck in flight"
    );
}

/// Paint until nothing is animating at all, once every tab has drawn.
/// Stricter than [`settle`], usable only after a client's buffer is in
/// the store: handing a placeholder to its window is a 180 ms cross-fade,
/// so the frame right after a commit is mostly still the card — sampling
/// there measures the dissolve, not the client's pixels.
fn quiesce(fb: &mut FrameBuilder, surface_id: u64) {
    let deadline = tilo_test_env::hang_deadline();
    // First wait for this exact client to latch as presented. A configure or
    // strip-layout change can delay that edge; measuring the dissolve from
    // publication would then spend the entire allowance on the opening card.
    while !store().is_presented(surface_id) && std::time::Instant::now() < deadline {
        let _ = fb.any_tile_opening();
        let _ = frame(fb);
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
    assert!(
        store().is_presented(surface_id),
        "the committed foreign surface never became presented"
    );
    // Now paint past the host's 180ms handover. Global animation demand is
    // intentionally not the exit condition: another shell group may still be
    // opening and is unrelated to this surface's dissolve.
    let handover_end = std::time::Instant::now()
        .checked_add(std::time::Duration::from_millis(220))
        .unwrap_or_else(std::time::Instant::now);
    while std::time::Instant::now() < handover_end {
        let _ = frame(fb);
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
}

/// A solid opaque RGBA image, as a client filling its surface would commit.
fn solid(width: u32, height: u32, rgb: (u8, u8, u8)) -> Vec<u8> {
    let pixel_count = width.saturating_mul(height);
    let byte_len = usize::try_from(pixel_count.saturating_mul(4)).unwrap_or(usize::MAX);
    let mut out = Vec::with_capacity(byte_len);
    for _ in 0..pixel_count {
        out.extend_from_slice(&[rgb.0, rgb.1, rgb.2, 0xFF]);
    }
    out
}

/// These tests mint the rendered tabs directly, so they also own creating
/// their documents. Presentation only opens existing records; it must never
/// infer permission to create one from an absent document.
fn create_fixture_tab_records(fb: &FrameBuilder) {
    let (records, tabs) = {
        let chrome = fb.chrome();
        let rendered = chrome.tree();
        let tabs = rendered
            .desktop
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| tab.id)
            .collect::<Vec<_>>();
        (chrome.records().clone(), tabs)
    };
    for tab in tabs {
        let checkout = shell::shell::tab_documents::scratch_tab_record(tab);
        shell::shell::tab_documents::create_scratch_tab_record(&records, tab)
            .expect("the fixture must create each rendered tab's document");
        assert_eq!(
            records.read(&checkout.id, ui::record::identity),
            Some(Some(checkout)),
            "the rendered tab must have its own existing document before painting"
        );
    }
}

/// A `FrameBuilder` whose tree carries the foreign tab, adopted the way the
/// compositor adopts a new toplevel.
fn adopt_into_frame_builder(surface_id: u64, app_id: &str) -> FrameBuilder {
    let mut fb = FrameBuilder::new((W, H));
    {
        let chrome = fb.chrome();
        let mut tree = chrome.tree_mut();
        tab_binding::adopt_toplevel(&mut tree, surface_id, Some(app_id.to_string()));
    }
    // Focus the adopted client's group: without focus the scroll reconcile
    // never pans the group into view.
    {
        let chrome = fb.chrome();
        let mut rendered = chrome.tree_mut();
        let focus = rendered
            .desktop
            .workspaces
            .iter()
            .flat_map(|w| w.strips.iter())
            .flat_map(|st| st.window_groups.iter().map(move |g| (st.id, g)))
            .find(|(_, g)| {
                g.tabs.iter().any(|t| {
                    matches!(
                        &t.tile,
                        shell::shell::state::TileContent::Foreign { surface_id: s, .. }
                            if *s == surface_id
                    )
                })
            })
            .map(|(strip_id, g)| (strip_id, g.id));
        let (strip_id, group_id) = focus.expect("the tree must hold the adopted client's group");
        rendered.desktop.set_active_group(strip_id, group_id);
    }
    create_fixture_tab_records(&fb);
    fb.with_chrome(shell::shell::chrome_state::ChromeState::bump);
    fb
}

/// The tile rect from the production accessor, cross-checked against the
/// rect the widget published into the frame store (what configure reads).
fn tile_rect(fb: &mut FrameBuilder, surface_id: u64) -> vello::kurbo::Rect {
    let rect = fb
        .foreign_surface_rect(surface_id)
        .unwrap_or_else(|| panic!("no visible tile hosts surface {surface_id}"));
    assert_eq!(
        store().placement(surface_id),
        Some(rect),
        "the rect the widget publishes for the configure pass must be the \
         rect it is actually laid out into"
    );
    rect
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn foreign_toplevel_lifecycle_drives_configure_unmap_and_tab_removal() {
    // Phase `tab_layout_drives_the_client_configure_size`.
    // The compositor sizes the client to the tab and re-sizes on an output
    // resize — once per change, never once per frame.
    {
        let surface_id = 0x5EED_0002;
        store().forget(surface_id);

        let mut fb = adopt_into_frame_builder(surface_id, "org.example.sized");
        let _ = frame(&mut fb);

        let first = store()
            .take_pending_configure(surface_id)
            .expect("the first layout must produce a configure");
        let rect = tile_rect(&mut fb, surface_id);
        assert_eq!(
            first,
            (
                i32_of_f64(rect.width().round()),
                i32_of_f64(rect.height().round())
            ),
            "the configure carries the tab's laid-out size"
        );
        assert_eq!(
            store().take_pending_configure(surface_id),
            None,
            "an unchanged layout must not re-configure the client every frame"
        );

        // Shrinking the output shrinks the tab, so the client is re-configured.
        fb.set_viewport((900, 600));
        let _ = frame(&mut fb);
        let resized = store()
            .take_pending_configure(surface_id)
            .expect("a resized tab must re-configure the client");
        let rect_after = tile_rect(&mut fb, surface_id);
        assert_eq!(
            resized,
            (
                i32_of_f64(rect_after.width().round()),
                i32_of_f64(rect_after.height().round())
            ),
            "the re-configure carries the tab's new laid-out size"
        );
        assert!(
            resized.1 < first.1,
            "a shorter output must ask the client for a shorter surface; \
             {first:?} -> {resized:?}"
        );

        store().forget(surface_id);
    }

    // Phase `unmap_stops_painting_and_destroy_releases_the_frame`.
    // Unmap stops painting the last frame; destroy releases the cached
    // framebuffer.
    {
        let surface_id = 0x5EED_0003;
        store().forget(surface_id);

        let mut fb = adopt_into_frame_builder(surface_id, "org.example.lifecycle");
        settle(&mut fb);
        let (bw, bh) = store().viewport(surface_id).expect("tab laid out");
        let rgba = solid(bw, bh, (0x22, 0xC0, 0x40));
        store().publish_rgba(surface_id, &rgba, bw, bh);
        assert!(store().snapshot(surface_id).is_some());

        // Sample after the 180 ms dissolve, or the fixture is still mostly card.
        quiesce(&mut fb, surface_id);
        let mut renderer = OffscreenRenderer::new(W, H);
        let scene = frame(&mut fb);
        let mapped = shot("foreign-lifecycle-mapped", &mut renderer, &scene);
        let rect = tile_rect(&mut fb, surface_id);
        let (cx, cy) = (u32_of_f64(rect.center().x), u32_of_f64(rect.center().y));
        let px = OffscreenRenderer::sample(&mapped, W, cx, cy);
        assert!(
            px.0.abs_diff(0x22) <= 4 && px.1.abs_diff(0xC0) <= 4 && px.2.abs_diff(0x40) <= 4,
            "fixture: the mapped client frame is on screen; got rgba{px:?}"
        );

        // `wl_surface.attach(NULL)` clears the frame; the tab stays, since the
        // toplevel is still alive.
        store().clear_frame(surface_id);
        assert!(
            store().snapshot(surface_id).is_none(),
            "an unmapped surface must not keep painting its stale frame"
        );
        let scene = frame(&mut fb);
        let unmapped = shot("foreign-lifecycle-unmapped", &mut renderer, &scene);
        let px = OffscreenRenderer::sample(&unmapped, W, cx, cy);
        assert!(
            !(px.0.abs_diff(0x22) <= 4 && px.1.abs_diff(0xC0) <= 4 && px.2.abs_diff(0x40) <= 4),
            "the stale frame is still on screen after unmap; got rgba{px:?}"
        );

        store().forget(surface_id);
        assert_eq!(store().frame_seq(surface_id), 0);
        assert_eq!(store().viewport(surface_id), None);
    }

    // Phase `destroying_the_toplevel_removes_the_tab_from_the_rendered_tree`.
    {
        let surface_id = 0x5EED_0004;
        store().forget(surface_id);

        let mut fb = adopt_into_frame_builder(surface_id, "org.example.transient");
        let _ = frame(&mut fb);
        assert!(
            fb.foreign_surface_rect(surface_id).is_some(),
            "the adopted client has a live tile"
        );

        let closed = {
            let chrome = fb.chrome();
            let mut tree = chrome.tree_mut();
            tab_binding::close_tab_for_destroyed_surface(&mut tree, surface_id)
        };
        assert!(closed, "the teardown must close the tab");
        fb.with_chrome(shell::shell::chrome_state::ChromeState::bump);
        let _ = frame(&mut fb);

        assert!(
            fb.foreign_surface_rect(surface_id).is_none(),
            "a destroyed toplevel must not leave its tile behind"
        );
        store().forget(surface_id);
    }
}
