//! End-to-end proof that an external Wayland client's pixels reach the
//! screen inside its tab.
//!
//! Everything below drives the *production* chain — there is no bespoke
//! render path for the test:
//!
//! ```text
//!   tab_binding::adopt_toplevel        (what XdgShellHandler::new_toplevel calls)
//!     → foreign::reconcile_foreign_tabs (what the native loop calls each tick)
//!     → ChromeState::bump               (revision → strip resync)
//!     → FrameBuilder::relayout          (splices the tab, publishes its rect)
//!     → shell::to_ui_tile               (Foreign → ForeignSurfaceWidget)
//!     → ForeignFrameStore               (what CompositorHandler::commit fills)
//!     → FrameBuilder::paint             (the same scene the compositor submits)
//!     → OffscreenRenderer               (rasterises that scene)
//! ```
//!
//! The assertions are on rasterised pixels and on the size the compositor
//! would put in an `xdg_toplevel.configure`, not on intermediate
//! bookkeeping.
//!
//! Set `QOS_SHOT_DIR` to also drop PNGs of every rendered frame for
//! eyeballing.

use shell::compositor::foreign::{self, store};
use shell::compositor::frame::FrameBuilder;
use shell::compositor::tab_binding;
use shell::shell::state::WorkspaceTree;
use ui::offscreen::OffscreenRenderer;
use vello::peniko::Color;

const W: u32 = 1600;
const H: u32 = 900;

/// Surface ids are the process-wide key into the frame store, so each
/// test picks its own to stay independent under parallel execution.
fn shot(name: &str, renderer: &mut OffscreenRenderer, scene: &vello::Scene) -> Vec<u8> {
    let rgba = renderer.render(scene, Color::BLACK);
    if let Ok(dir) = std::env::var("QOS_SHOT_DIR") {
        let path = std::path::Path::new(&dir).join(format!("{name}.png"));
        renderer.render_png(scene, Color::BLACK, &path);
        eprintln!("wrote {}", path.display());
    }
    rgba
}

/// One production frame: the per-frame state pump (`sync` + `update`) and
/// then layout + paint, in the order `compositor::native::run` does it.
///
/// The pump is not optional here. Adopting a client focuses its new group,
/// and it is `update` that pans the strip to bring the focused group into
/// view. A strip whose existing window is full-width places the new group
/// past the right edge, so without the pump the tile under test is laid out
/// off the render surface and every pixel assertion samples nothing.
fn frame(fb: &mut FrameBuilder) -> vello::Scene {
    let mut doc = automerge::AutoCommit::new();
    let root = automerge::ROOT;
    fb.pump_apps(&mut doc, &root);
    fb.paint_settled()
}

/// Paint until the widget tree stops asking for animation frames.
///
/// Splicing a tab into a live group plays the tile's spawn fade, which
/// legitimately keeps `needs_animation_frame()` true for a couple of
/// hundred milliseconds. Tests that assert on the *idle* repaint signal
/// have to let that settle first, or they measure the fade instead of the
/// thing they care about.
fn settle(fb: &mut FrameBuilder) {
    // Always paint once: the first pass is what splices pending tabs into
    // the live tree and lays them out, so it must run even when nothing
    // is animating yet.
    let _ = frame(fb);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while fb.needs_animation_frame() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(8));
        let _ = frame(fb);
    }
    assert!(
        !fb.needs_animation_frame(),
        "the widget tree never settled; an animation is stuck in flight"
    );
}

/// A solid opaque RGBA image — what a client that filled its surface with
/// one colour would commit.
fn solid(width: u32, height: u32, rgb: (u8, u8, u8)) -> Vec<u8> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..(width * height) {
        out.extend_from_slice(&[rgb.0, rgb.1, rgb.2, 0xFF]);
    }
    out
}

/// Build a `FrameBuilder` whose rendered tree carries the foreign tab a
/// client would have been adopted into, exactly the way the native
/// compositor loop gets there.
fn adopt_into_frame_builder(surface_id: u64, app_id: &str) -> (FrameBuilder, WorkspaceTree) {
    let mut protocol_tree = WorkspaceTree::bootstrap_demo();
    tab_binding::adopt_toplevel(&mut protocol_tree, surface_id, Some(app_id.to_string()));

    let mut fb = FrameBuilder::new((W, H));
    let mirrored = {
        let chrome = fb.chrome();
        let mut rendered = chrome.tree_mut();
        foreign::reconcile_foreign_tabs(&protocol_tree, &mut rendered)
    };
    assert!(
        mirrored,
        "the protocol-side adoption must mirror into the rendered tree"
    );
    // Focus the adopted client's group in the rendered tree.
    //
    // `adopt_toplevel` does this on the protocol side, and in the compositor
    // the two are one shared tree — but this harness keeps them separate and
    // `reconcile_foreign_tabs` mirrors tabs, not the focus pointer. Without
    // the focus the per-frame scroll reconcile has no reason to pan, and a
    // strip whose existing window is full-width leaves the new group off the
    // right edge of the output.
    {
        let chrome = fb.chrome();
        let mut rendered = chrome.tree_mut();
        let focus = rendered
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
        let (strip_id, group_id) =
            focus.expect("the mirrored tree must hold the adopted client's group");
        rendered.set_active_group(strip_id, group_id);
    }
    fb.with_chrome(|st| st.bump());
    (fb, protocol_tree)
}

/// The tile rect, resolved through the production accessor that walks the
/// live widget tree, cross-checked against the rect the widget published
/// into the frame store (the value the configure pass consumes).
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

/// The load-bearing test: a client's buffer bytes become pixels inside
/// the tab's rectangle in the scene the compositor actually submits.
#[test]
fn client_buffer_pixels_are_composited_into_the_tab_rect() {
    let surface_id = 0x5EED_0001;
    store().forget(surface_id);

    let (mut fb, _protocol) = adopt_into_frame_builder(surface_id, "org.example.client");
    // Reach a steady state first: the tab is spliced into the live tree, the
    // strip pans the newly focused group into view, and the spawn fade
    // finishes. Only then is the published tab rect the one the client will
    // actually be configured to.
    settle(&mut fb);
    let scene = frame(&mut fb);
    let mut renderer = OffscreenRenderer::new(W, H);
    let before = shot("foreign-before-first-frame", &mut renderer, &scene);

    let rect = tile_rect(&mut fb, surface_id);
    assert!(
        rect.width() > 100.0 && rect.height() > 100.0,
        "the foreign tile must occupy real area; got {rect:?}"
    );
    let (cx, cy) = (rect.center().x as u32, rect.center().y as u32);

    // The compositor learned the tab's size from the layout pass, which
    // is what it configures the client with.
    let viewport = store()
        .viewport(surface_id)
        .expect("apply_layout must publish the tab rect into the frame store");
    assert_eq!(
        viewport,
        (rect.width().round() as u32, rect.height().round() as u32),
        "the published viewport is the tab's on-screen extent"
    );

    // No frame yet: whatever is on screen, it is NOT the client's colour.
    let pre = OffscreenRenderer::sample(&before, W, cx, cy);
    assert_ne!(
        (pre.0, pre.1, pre.2),
        (0xE0, 0x18, 0xA0),
        "test fixture: the placeholder must not already be the client colour"
    );

    // The client renders a solid, unmistakable colour at exactly the
    // size it was configured to, and commits. `publish_rgba` is the same
    // entry point the dmabuf import path uses after decoding.
    let (bw, bh) = viewport;
    let rgba = solid(bw, bh, (0xE0, 0x18, 0xA0));
    store().publish_rgba(surface_id, &rgba, bw, bh);

    // The widget reports it needs a repaint — this is the signal that
    // wakes the compositor's paint loop for a new client frame.
    assert!(
        fb.needs_animation_frame(),
        "a freshly committed client frame must request a repaint"
    );

    let scene = frame(&mut fb);
    let after = shot("foreign-with-client-frame", &mut renderer, &scene);
    let px = OffscreenRenderer::sample(&after, W, cx, cy);
    assert!(
        px.0.abs_diff(0xE0) <= 4 && px.1.abs_diff(0x18) <= 4 && px.2.abs_diff(0xA0) <= 4,
        "the client's pixels must be composited at the tab centre; \
         got rgba{px:?} at ({cx}, {cy}) inside {rect:?}"
    );

    // …and only inside the tab. A point well outside the tile keeps the
    // shell's own chrome.
    let outside_y = (rect.y0 as u32).saturating_sub(30).max(1);
    let out = OffscreenRenderer::sample(&after, W, cx, outside_y);
    assert!(
        !(out.0.abs_diff(0xE0) <= 4 && out.1.abs_diff(0x18) <= 4 && out.2.abs_diff(0xA0) <= 4),
        "client pixels must be clipped to the tab; leaked to ({cx}, {outside_y}): rgba{out:?}"
    );

    store().forget(surface_id);
}

/// The compositor sizes the client to the tab, and re-sizes it when the
/// tab's extent changes (here: an output resize). The configure is
/// emitted once per change, never once per frame.
#[test]
fn tab_layout_drives_the_client_configure_size() {
    let surface_id = 0x5EED_0002;
    store().forget(surface_id);

    let (mut fb, _protocol) = adopt_into_frame_builder(surface_id, "org.example.sized");
    let _ = frame(&mut fb);

    let first = store()
        .take_pending_configure(surface_id, 1.0)
        .expect("the first layout must produce a configure");
    let rect = tile_rect(&mut fb, surface_id);
    assert_eq!(
        first,
        (rect.width().round() as i32, rect.height().round() as i32),
        "the configure carries the tab's laid-out size"
    );
    assert_eq!(
        store().take_pending_configure(surface_id, 1.0),
        None,
        "an unchanged layout must not re-configure the client every frame"
    );

    // Shrink the output: the tab shrinks, so the client is re-configured
    // to the new tab extent.
    fb.set_viewport((900, 600));
    let _ = frame(&mut fb);
    let resized = store()
        .take_pending_configure(surface_id, 1.0)
        .expect("a resized tab must re-configure the client");
    let rect_after = tile_rect(&mut fb, surface_id);
    assert_eq!(
        resized,
        (
            rect_after.width().round() as i32,
            rect_after.height().round() as i32
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

/// Unmap (client detached its buffer) stops painting the last frame, and
/// destroy releases the cached framebuffer entirely.
#[test]
fn unmap_stops_painting_and_destroy_releases_the_frame() {
    let surface_id = 0x5EED_0003;
    store().forget(surface_id);

    let (mut fb, _protocol) = adopt_into_frame_builder(surface_id, "org.example.lifecycle");
    settle(&mut fb);
    let (bw, bh) = store().viewport(surface_id).expect("tab laid out");
    let rgba = solid(bw, bh, (0x22, 0xC0, 0x40));
    store().publish_rgba(surface_id, &rgba, bw, bh);
    assert!(store().snapshot(surface_id).is_some());

    let mut renderer = OffscreenRenderer::new(W, H);
    let scene = frame(&mut fb);
    let mapped = shot("foreign-lifecycle-mapped", &mut renderer, &scene);
    let rect = tile_rect(&mut fb, surface_id);
    let (cx, cy) = (rect.center().x as u32, rect.center().y as u32);
    let px = OffscreenRenderer::sample(&mapped, W, cx, cy);
    assert!(
        px.0.abs_diff(0x22) <= 4 && px.1.abs_diff(0xC0) <= 4 && px.2.abs_diff(0x40) <= 4,
        "fixture: the mapped client frame is on screen; got rgba{px:?}"
    );

    // `wl_surface.attach(NULL)` → the compositor's commit hook clears the
    // frame. The tab stays (the toplevel is still alive).
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

    // Destroy → the entry (and its framebuffer) is gone.
    store().forget(surface_id);
    assert_eq!(store().frame_seq(surface_id), 0);
    assert_eq!(store().viewport(surface_id), None);
}

/// A destroyed toplevel's tab disappears from the rendered tree, and its
/// widget goes with it.
#[test]
fn destroying_the_toplevel_removes_the_tab_from_the_rendered_tree() {
    let surface_id = 0x5EED_0004;
    store().forget(surface_id);

    let (mut fb, mut protocol) = adopt_into_frame_builder(surface_id, "org.example.transient");
    let _ = frame(&mut fb);
    assert!(
        fb.foreign_surface_rect(surface_id).is_some(),
        "the adopted client has a live tile"
    );

    // Protocol side tears the toplevel down.
    assert!(tab_binding::close_tab_for_destroyed_surface(
        &mut protocol,
        surface_id
    ));
    let mirrored = {
        let chrome = fb.chrome();
        let mut rendered = chrome.tree_mut();
        foreign::reconcile_foreign_tabs(&protocol, &mut rendered)
    };
    assert!(mirrored, "the teardown must mirror across");
    fb.with_chrome(|st| st.bump());
    let _ = frame(&mut fb);

    assert!(
        fb.foreign_surface_rect(surface_id).is_none(),
        "a destroyed toplevel must not leave its tile behind"
    );
    store().forget(surface_id);
}

/// Painting is free: the expensive part of a foreign tile is turning the
/// client's buffer into RGBA, and that must happen once per *commit*,
/// never once per *paint*.
///
/// A dmabuf import is a GPU readback (or a full-frame CPU map). The
/// compositor paints far more often than a client commits, so an import
/// hung off the paint loop would turn a single idle GPU client into
/// permanent load — the exact regression this asserts against, on the
/// production `FrameBuilder::paint` path.
#[test]
fn painting_does_not_reimport_the_clients_buffer() {
    let surface_id = 0x5EED_0007;
    store().forget(surface_id);

    let (mut fb, _protocol) = adopt_into_frame_builder(surface_id, "org.example.steady");
    settle(&mut fb);
    let (bw, bh) = store().viewport(surface_id).expect("tab laid out");

    // One commit's worth of pixels lands in the store. `publish_rgba` is
    // the same publish both import routes end in.
    let before = store().import_count();
    store().publish_rgba(surface_id, &solid(bw, bh, (0x30, 0x90, 0xD0)), bw, bh);

    // Now paint repeatedly with no further commits, the way the
    // compositor's loop does while the client sits idle.
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

/// A tab that is not the active one in its group must not paint its
/// client's pixels — and, because `WindowGroup` only consults the active
/// host, must not keep the compositor's paint loop awake either.
#[test]
fn a_background_foreign_tab_neither_paints_nor_wakes_the_paint_loop() {
    let front = 0x5EED_0005;
    let back = 0x5EED_0006;
    store().forget(front);
    store().forget(back);

    let mut fb = FrameBuilder::new((W, H));
    // Two clients as two tabs of ONE group, `front` the active one.
    //
    // Both go in through the production splice; the second names the
    // group the first landed in, which is the placement a document
    // opened from another app asks for. Adopting twice with the default
    // placement would *not* build this case: a launch is a new task, so
    // it gets its own window group beside the focused one, and each
    // client would then be the active tab of a group of its own with
    // nothing in the background at all.
    {
        let chrome = fb.chrome();
        let mut rendered = chrome.tree_mut();
        tab_binding::adopt_toplevel(&mut rendered, back, Some("org.example.back".into()));
        let group = rendered
            .active_strip
            .get(&rendered.active)
            .copied()
            .and_then(|strip| rendered.active_group_for(strip))
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
    fb.with_chrome(|st| st.bump());
    settle(&mut fb);

    let rect = tile_rect(&mut fb, front);
    let (cx, cy) = (rect.center().x as u32, rect.center().y as u32);

    // Only the background client commits a frame.
    let (bw, bh) = store()
        .viewport(front)
        .expect("the visible tab publishes its rect");
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
