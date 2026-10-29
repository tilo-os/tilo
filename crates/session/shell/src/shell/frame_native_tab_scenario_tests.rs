//! The frame's fullscreen and opening-window scenarios, each its own fixture
//! rather than one chain: a full screen gives a window the whole output and
//! restores its tile, an unknown fullscreen id leaves the shell standing, and
//! a restored native tab is configured before its first buffer.

use super::*;

/// Look up a widget's rect by its *last* path segment
/// (`.../TopBarSpace[0]` → `TopBarSpace`) — registry keys are full
/// paths, so an exact-key lookup on a nested widget would miss. The
/// widgets asserted on here are unique in the rendered tree, so the
/// first match is deterministic.
#[cfg(test)]
pub(crate) fn rect_named(
    reg: &HashMap<String, vello::kurbo::Rect>,
    name: &str,
) -> vello::kurbo::Rect {
    reg.iter()
        .find(|(k, _)| {
            let seg = k.rsplit('/').next().unwrap_or(k);
            let leaf = match seg.find('[') {
                Some(i) => &seg[..i],
                None => seg,
            };
            leaf == name
        })
        .map_or_else(
            || {
                panic!(
                    "widget `{name}` not found in registry; keys: {:?}",
                    reg.keys().collect::<Vec<_>>()
                )
            },
            |(_, r)| *r,
        )
}

#[test]
#[serial_test::serial(process_env)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn native_tabs_configure_before_first_buffer_and_fullscreen_restores() {
    fn restored_native_tab_gets_a_configure_rect_before_its_first_buffer() {
        use crate::shell::state::{Tab, TileContent};
        use crate::shell::tab_binding::{self, Placement};

        const SURFACE: u64 = 0xfedc_0011;
        const APP: &str = "dev.tilo.calculator";
        crate::shell::launcher::clear_pending_placements();
        let store = crate::shell::foreign::store();
        store.forget(SURFACE);
        let mut tree = WorkspaceTree::bootstrap_default();
        let room = tree.desktop.workspaces[0].room();
        let tab = tab_binding::place_tab(
            &mut tree,
            Tab {
                id: crate::shell::state::next_id(),
                title: "Calculator".into(),
                tile: TileContent::LocalApp {
                    app_id: APP.into(),
                    label: "Calculator".into(),
                    // Restore has already dispatched the app; do not start
                    // a real process from this frame-builder regression.
                    note: crate::shell::peer_windows::OPENING.into(),
                },
                app_title: None,
            },
            Placement::NewGroup,
        );
        let mut fb = FrameBuilder::with_tree(tree, (1280, 800));
        let mut doc = AutoCommit::new();
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        assert!(fb.foreign_surface_rect(SURFACE).is_none());
        // A real restored LocalApp notice has already appeared on screen.
        // Its host visibility latch must not hide the new app's readiness.
        let _ = fb.paint();
        crate::shell::launcher::set_pending_placement(APP, Placement::IntoTab(tab));
        {
            let chrome = fb.chrome();
            let mut tree = chrome.tree_mut();
            assert_eq!(
                tab_binding::adopt_toplevel_in_room(&mut tree, SURFACE, Some(APP.into()), room),
                Some(tab),
            );
        }
        store.note_adopted(SURFACE);
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        assert_eq!(store.frame_seq(SURFACE), 0);
        let rect = fb
            .foreign_surface_rect(SURFACE)
            .expect("restored tab must publish its configure rect before the client can draw");
        assert!(rect.width() > 0.0 && rect.height() > 0.0);
        let (width, height) = store
            .take_pending_configure(SURFACE)
            .expect("layout must make the initial client configure available without input");
        assert!(
            fb.needs_animation_frame() || fb.next_wake_deadline().is_some(),
            "the opening window must keep progressing"
        );
        assert!(!store.is_presented(SURFACE));
        assert!(
            fb.any_tile_arriving(),
            "the visible notice must become the window's arrival, not an unresponsive-app error"
        );
        let _ = fb.paint();

        let width = u32::try_from(width).expect("positive configured width");
        let height = u32::try_from(height).expect("positive configured height");
        let bytes = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
            .and_then(|bytes| usize::try_from(bytes).ok())
            .expect("bounded test viewport");
        store.publish_rgba(SURFACE, &vec![255; bytes], width, height);
        let revision = fb.chrome_revision();
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        let _ = fb.paint();
        assert!(
            store.is_presented(SURFACE),
            "the first sized buffer must replace the opening presentation"
        );
        assert_eq!(
            fb.chrome_revision(),
            revision,
            "presentation must not need an unrelated tree mutation"
        );
        store.forget(SURFACE);
        crate::shell::launcher::clear_pending_placements();
    }

    /// A group promoted to fullscreen absorbs the whole output (top bar
    /// collapses, the strip's rect rises and spans the width), and clearing
    /// it restores the exact prior layout — the compositor-scene half of the
    /// fullscreen contract, driven through the
    /// `FrameBuilder::set_fullscreen` → `ChromeState` → overlay-host path.
    fn set_fullscreen_promotes_group_and_restores_on_exit() {
        let mut fb = FrameBuilder::new((1600, 900));
        let _ = fb.paint_settled();

        // A group that actually exists in the rendered tree.
        let gid = {
            let chrome = fb.chrome();
            let tree = chrome.tree();
            tree.desktop
                .active_workspace()
                .expect("bootstrap tree has an active workspace")
                .strips[0]
                .window_groups[0]
                .id
        };

        let before = fb.widget_registry();
        let topbar_before = rect_named(&before, "TopBarSpace");
        let row_before = rect_named(&before, "GroupRow");
        assert!(
            topbar_before.height() > 10.0,
            "top bar visible before fullscreen"
        );

        // Enter fullscreen on that group.
        fb.set_fullscreen(Some(gid));
        assert_eq!(fb.fullscreen_group(), Some(gid));
        let _ = fb.paint_settled();
        assert!(fb.chrome().is_fullscreen());

        let during = fb.widget_registry();
        let topbar_fs = rect_named(&during, "TopBarSpace");
        let row_fs = rect_named(&during, "GroupRow");
        assert!(
            topbar_fs.height() <= 0.5,
            "top bar collapses under fullscreen; got height {}",
            topbar_fs.height(),
        );
        assert!(
            row_fs.y0 < row_before.y0 - 20.0,
            "the strip rises to the top once the bar is gone; before y0 {} during {}",
            row_before.y0,
            row_fs.y0,
        );
        assert!(
            row_fs.width() >= fb.viewport().0 - 1.0,
            "fullscreen strip spans the output width; got {} vs {}",
            row_fs.width(),
            fb.viewport().0,
        );

        // Exit — the layout restores exactly.
        fb.set_fullscreen(None);
        assert_eq!(fb.fullscreen_group(), None);
        let _ = fb.paint_settled();
        assert!(!fb.chrome().is_fullscreen());

        let after = fb.widget_registry();
        let topbar_after = rect_named(&after, "TopBarSpace");
        let row_after = rect_named(&after, "GroupRow");
        assert!(
            (topbar_after.height() - topbar_before.height()).abs() < 0.5,
            "top bar height restores: before {} after {}",
            topbar_before.height(),
            topbar_after.height(),
        );
        assert!(
            (row_after.y0 - row_before.y0).abs() < 0.5
                && (row_after.x0 - row_before.x0).abs() < 0.5
                && (row_after.width() - row_before.width()).abs() < 0.5,
            "strip row rect restores exactly: before {row_before:?} after {row_after:?}",
        );
    }

    /// A fullscreen id the rendered tree does NOT host must be a safe
    /// no-op — must not blank the shell by hiding every strip (the live
    /// client tree and rendered tree aren't yet unified).
    fn set_fullscreen_with_unknown_group_does_not_blank_shell() {
        let mut fb = FrameBuilder::new((1600, 900));
        let _ = fb.paint_settled();
        let before = fb.widget_registry();
        let row_before = rect_named(&before, "GroupRow");

        // An id that cannot exist in the tree.
        fb.set_fullscreen(Some(u64::MAX));
        let _ = fb.paint_settled();

        let during = fb.widget_registry();
        let row_during = rect_named(&during, "GroupRow");
        let topbar_during = rect_named(&during, "TopBarSpace");
        assert!(
            row_during.width() > 0.0 && row_during.height() > 0.0,
            "an unknown fullscreen id must not collapse the shell; got {row_during:?}",
        );
        // Geometry matches the normal (non-fullscreen) layout — no promotion,
        // no chrome suppression.
        assert!((row_during.width() - row_before.width()).abs() < 0.5);
        assert!(
            topbar_during.height() > 10.0,
            "top bar stays visible for an unknown fullscreen id",
        );
    }

    /// Expand keeps the top bar.
    fn an_expanded_window_keeps_the_top_bar() {
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_demo();
        let first = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
        first.maximized = true;
        let group = first.id;
        let mut fb = FrameBuilder::with_tree(tree, (1600, 900));
        let _ = fb.paint_settled();
        fb.with_chrome(|chrome| chrome.set_maximized_group(Some(group)));
        let _ = fb.paint_settled();
        assert!(
            rect_named(&fb.widget_registry(), "TopBarSpace").height() > 10.0,
            "Expand keeps the top bar"
        );
        fb.with_chrome(|chrome| chrome.set_maximized_group(None));
        let _ = fb.paint_settled();
        assert!(
            rect_named(&fb.widget_registry(), "TopBarSpace").height() > 10.0,
            "restoring keeps the top bar"
        );
    }

    /// Super+F on a Tilo window covers the whole output. Hosted apps
    /// are xdg clients; chrome fullscreen still wins so W07 sees
    /// `fullscreen_group`.
    fn super_f_on_a_tilo_group_covers_the_output() {
        let mut fb = FrameBuilder::new((1600, 900));
        let _ = fb.paint_settled();
        assert!(fb.host_mut().toggle_focused_group_fullscreen());
        // The native loop writes `set_fullscreen(None)` every tick so
        // client xdg stays an overlay. Super+F must survive that wipe.
        fb.set_fullscreen(None);
        assert!(
            fb.fullscreen_group().is_some(),
            "compositor set_fullscreen(None) must not clear Super+F"
        );
        let _ = fb.paint_settled();
        assert!(fb.chrome().is_fullscreen());
        let during = fb.widget_registry();
        let row = rect_named(&during, "GroupRow");
        assert!(
            row.width() >= fb.viewport().0 - 1.0,
            "Super+F must cover the output; row width {} vs output {}",
            row.width(),
            fb.viewport().0,
        );
        assert!(
            rect_named(&during, "TopBarSpace").height() <= 0.5,
            "Super+F hides the top bar"
        );
        assert!(fb.host_mut().toggle_focused_group_fullscreen());
        let _ = fb.paint_settled();
        assert!(!fb.chrome().is_fullscreen());
    }

    /// Super+F on a foreign window of a desktop partitioned across two
    /// outputs gives it that whole output, not its strip band under the
    /// hidden top bar, and leaving fullscreen restores its tile.
    fn super_f_foreign_window_covers_its_output_in_a_partitioned_desktop() {
        use crate::shell::output_scene::OutputGeometry;
        use crate::shell::tab_binding::{self, Placement};

        const SURFACE: u64 = 0xfedc_0012;
        const APP: &str = "tilo.test.fullscreen-partition";
        let store = crate::shell::foreign::store();
        store.forget(SURFACE);
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_default();
        let room = tree.desktop.workspaces[0].room();
        crate::shell::launcher::set_pending_placement(APP, Placement::NewGroup);
        assert!(
            tab_binding::adopt_toplevel_in_room(&mut tree, SURFACE, Some(APP.into()), room)
                .is_some()
        );
        store.note_adopted(SURFACE);
        // Frames on a pinned clock that advances a frame interval each,
        // so the strip's springs run to rest in simulated time: every
        // rect compared below is a settled layout, not a frame of motion.
        let mut at = ui::time::now();
        let settle = |fb: &mut FrameBuilder, at: &mut ui::time::Instant| {
            for _ in 0..120 {
                *at += ui::time::Duration::from_millis(16);
                ui::time::testing::with_monotonic_time(*at, || {
                    let _ = fb.paint_settled();
                });
            }
        };
        let mut fb = FrameBuilder::with_tree(tree, (2944, 1080));
        fb.set_output_geometry(vec![
            OutputGeometry::unscaled(0.0, 0.0, 1920.0, 1080.0).with_id(0),
            OutputGeometry::unscaled(1920.0, 0.0, 1024.0, 640.0).with_id(1),
        ]);
        settle(&mut fb, &mut at);
        let before = fb
            .foreign_surface_rect(SURFACE)
            .expect("tiled foreign window");
        assert!(before.y0 > 0.0);
        assert!(fb.host_mut().toggle_focused_group_fullscreen());
        fb.set_fullscreen(None);
        settle(&mut fb, &mut at);
        let fullscreen = fb
            .foreign_surface_rect(SURFACE)
            .expect("fullscreen foreign window");
        assert!(
            fullscreen.y0.abs() < 0.5 && (fullscreen.height() - 1080.0).abs() < 0.5,
            "Super+F must give the client the whole output height: {fullscreen:?}"
        );
        assert!(
            fullscreen.x0.abs() < 0.5 && (fullscreen.width() - 1920.0).abs() < 0.5,
            "Super+F must give the client the whole output width: {fullscreen:?}"
        );
        assert!(fb.host_mut().toggle_focused_group_fullscreen());
        settle(&mut fb, &mut at);
        let restored = fb
            .foreign_surface_rect(SURFACE)
            .expect("restored foreign window");
        assert_eq!(restored, before, "leaving fullscreen restores the tile");
        store.forget(SURFACE);
        crate::shell::launcher::clear_pending_placements();
    }

    restored_native_tab_gets_a_configure_rect_before_its_first_buffer();
    set_fullscreen_promotes_group_and_restores_on_exit();
    set_fullscreen_with_unknown_group_does_not_blank_shell();
    super_f_on_a_tilo_group_covers_the_output();
    an_expanded_window_keeps_the_top_bar();
    super_f_foreign_window_covers_its_output_in_a_partitioned_desktop();
}
