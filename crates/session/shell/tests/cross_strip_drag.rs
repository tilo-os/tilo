//! Per-frame wire-up coverage for cross-strip tab drag-drop.
//!
//! The cross-group case is covered by unit tests in
//! `crates/sdk/ui-core/src/widget/group_row.rs`. Cross-*strip* can't resolve from inside
//! a single `GroupRow` (which only sees its own siblings), so the broker
//! lives shell-side: [`ShellOverlayHost`] holds per-strip `GroupRow`
//! pointers and [`WorkspaceTree`] tracks every strip's identity. Without
//! that hook the source row's local resolver silently loses the "move to
//! the OTHER strip" case.

use shell::shell::state::FreshId as _;
use std::collections::HashMap;

use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree,
};
use ui::widget::Widget as _;
use vello::kurbo::Point;
use winit::event::Modifiers;

use crate::support::host::{Layout, dispatch, lay_out};

const W: f32 = 1280.0;
const H: f32 = 800.0;

/// The smallest topology that exercises cross-strip dispatch: fewer
/// strips leaves nothing to cross to, fewer tabs makes the source group
/// disappear mid-detach (a separate case).
fn two_strip_two_tab_tree() -> (WorkspaceTree, StripId, StripId, [u64; 2], [u64; 2]) {
    let ws_id = WorkspaceId::new();
    let s_a = StripId::new();
    let s_b = StripId::new();
    // Stable widget-tree ids so failure messages stay legible.
    let g_a: GroupId = 1001;
    let g_b: GroupId = 2001;
    let a_tabs: [u64; 2] = [11, 12];
    let b_tabs: [u64; 2] = [21, 22];

    let mk_tab = |id: u64, label: &str| Tab {
        id,
        title: label.into(),
        tile: TileContent::Stub {
            kind: AppKind::Other("Test".into()),
            label: label.into(),
        },
        app_title: None,
    };

    let strip_a = Strip {
        id: s_a,
        name: "StripA".into(),
        window_groups: vec![WindowGroup {
            id: g_a,
            tabs: vec![mk_tab(a_tabs[0], "A1"), mk_tab(a_tabs[1], "A2")],
            active_tab: a_tabs[0],
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    };
    let strip_b = Strip {
        id: s_b,
        name: "StripB".into(),
        window_groups: vec![WindowGroup {
            id: g_b,
            tabs: vec![mk_tab(b_tabs[0], "B1"), mk_tab(b_tabs[1], "B2")],
            active_tab: b_tabs[0],
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    };
    let ws = {
        let mut workspace = Workspace::new(ws_id, "Test");
        workspace.strips = vec![strip_a, strip_b];
        workspace
    };

    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, s_a);
    let mut active_group = HashMap::new();
    active_group.insert(s_a, g_a);
    active_group.insert(s_b, g_b);

    let mut tree = WorkspaceTree {
        desktop: shell::shell::state::DesktopTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group,
            collaborators: vec![],
            ..Default::default()
        },
        ..Default::default()
    };
    // Production trees always carry the Automerge mirror established at
    // creation/load. Keep the accepted-drop fixture faithful to that path;
    // refusal against an unavailable document is covered separately.
    tree.seed_doc_from_cache();

    (tree, s_a, s_b, a_tabs, b_tabs)
}

/// Full build/layout/apply pass mirroring `App::layout`, so every rect
/// lands in screen-space coords.
fn lay_out_host(host: &mut ShellOverlayHost) -> Layout {
    lay_out(host, W, H)
}

/// Pump a no-op `CursorMoved` so a pre-staged `pending_detach` flows
/// through `WindowGroup::handle_event` into `GroupRow`'s detach sweep.
fn pump_no_op(host: &mut ShellOverlayHost, layout: &Layout) {
    let event = ui::synthetic_input::cursor_moved(9000.0, 9000.0);
    dispatch(
        host,
        &layout.taffy,
        layout.child_node,
        Modifiers::default(),
        &event,
    );
    // This harness stops short of `Widget::update`; tests invoke the
    // cross-strip dispatch via `tick_cross_strip_dispatch_for_tests`.
}

#[test]
#[allow(
    clippy::similar_names,
    reason = "the a/b suffix pairs (strip_a_total_tabs/strip_b_total_tabs, \
tree_a_total/tree_b_total) mirror the two strips under test; renaming either \
away from the pair would hurt readability, not help it"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "tree is read from through the end of the function (tree_strip_a/b, \
tree_a_total/b_total, tree_b_has_dragged), so st can't be dropped any earlier"
)]
#[allow(
    clippy::too_many_lines,
    reason = "the test keeps the complete cross-strip gesture and both topology assertions together"
)]
fn tab_drags_cross_into_a_destination_strip_or_return_home() {
    // Phase `dragged_tab_crosses_into_destination_strip_via_shell_dispatch`.
    {
        let (tree, s_a, s_b, a_tabs, _b_tabs) = two_strip_two_tab_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);

        // Without a layout pass the resolver compares release points against
        // `Rect::ZERO` and never matches.
        let layout = lay_out_host(&mut host);

        // Pre-condition: strip-A [11, 12], strip-B [21, 22], widget tree
        // mirroring the persisted state at boot.
        let snap_pre = host.groups_per_strip();
        let snap_pre_a = snap_pre.iter().find(|(s, _)| *s == s_a).expect("strip A");
        let snap_pre_b = snap_pre.iter().find(|(s, _)| *s == s_b).expect("strip B");
        assert_eq!(snap_pre_a.1.len(), 1, "strip A starts with 1 group");
        assert_eq!(snap_pre_b.1.len(), 1, "strip B starts with 1 group");
        assert_eq!(snap_pre_a.1[0].1, vec![a_tabs[0], a_tabs[1]]);
        assert_eq!(snap_pre_b.1[0].1, vec![21, 22]);

        // Release y inside strip-B's band, so the dispatcher reads "aiming at
        // strip B" rather than "dropped in own strip".
        let dest_rect = host
            .group_row_rect_for_tests(s_b)
            .expect("strip B's GroupRow rect must be populated after apply_layout");
        let drop_y = f64::midpoint(dest_rect.y0, dest_rect.y1);
        let destination_group = host
            .group_at_for_tests(s_b, 0)
            .expect("strip B destination group");
        let drop_x = destination_group.get_rect().center().x + 1.0;
        let release = Point::new(drop_x, drop_y);

        // Same shape `TabBar::handle_event` writes when a drag releases outside
        // the bar's own rect.
        let dragged = a_tabs[0];
        {
            let g = host
                .group_at_for_tests(s_a, 0)
                .expect("strip A group 0 widget");
            g.bar_mut_for_tests().pending_detach = Some((dragged, release));
        }

        // `GroupRow::handle_event` drains the pending_detach and, since
        // `release.y` is outside strip-A's band, stashes the tab on
        // `pending_cross_strip_detach` for the shell to route.
        pump_no_op(&mut host, &layout);
        host.tick_cross_strip_dispatch_for_tests();

        let snap_post = host.groups_per_strip();
        let snap_post_a = snap_post.iter().find(|(s, _)| *s == s_a).expect("strip A");
        let snap_post_b = snap_post.iter().find(|(s, _)| *s == s_b).expect("strip B");

        let strip_a_total_tabs: usize = snap_post_a.1.iter().map(|(_, ts)| ts.len()).sum();
        assert_eq!(
            strip_a_total_tabs, 1,
            "strip A lost the dragged tab — total now 1, was 2"
        );
        assert!(
            !snap_post_a.1.iter().any(|(_, ts)| ts.contains(&dragged)),
            "dragged tab id {dragged} must no longer live on strip A"
        );

        let strip_b_total_tabs: usize = snap_post_b.1.iter().map(|(_, ts)| ts.len()).sum();
        assert_eq!(
            strip_b_total_tabs, 3,
            "strip B gained the dragged tab — total now 3, was 2"
        );
        assert_eq!(
            snap_post_b.1.len(),
            1,
            "the destination body joins its existing group instead of creating a seam group"
        );
        assert_eq!(
            snap_post_b.1[0].1,
            vec![21, 22, dragged],
            "the right half of the destination body appends at the exact shared target"
        );

        // The splice must also reach the persisted tree, or the tab pops back to
        // strip A on the next reload.
        let st = chrome.borrow();
        let tree = st.tree();
        let tree_strip_a = tree.desktop.workspaces[0]
            .strips
            .iter()
            .find(|s| s.id == s_a)
            .expect("strip A in tree");
        let tree_strip_b = tree.desktop.workspaces[0]
            .strips
            .iter()
            .find(|s| s.id == s_b)
            .expect("strip B in tree");
        let tree_a_total: usize = tree_strip_a
            .window_groups
            .iter()
            .map(|g| g.tabs.len())
            .sum();
        let tree_b_total: usize = tree_strip_b
            .window_groups
            .iter()
            .map(|g| g.tabs.len())
            .sum();
        assert_eq!(
            tree_a_total, 1,
            "persisted tree: strip A must have 1 tab after the cross-strip move"
        );
        assert_eq!(
            tree_b_total, 3,
            "persisted tree: strip B must have 3 tabs after the cross-strip move"
        );
        let tree_b_has_dragged = tree_strip_b
            .window_groups
            .iter()
            .any(|g| g.tabs.iter().any(|t| t.id == dragged));
        assert!(
            tree_b_has_dragged,
            "persisted tree: strip B must carry the dragged tab id {dragged}"
        );
        assert_eq!(
            tree_strip_b.window_groups[0]
                .tabs
                .iter()
                .map(|tab| tab.id)
                .collect::<Vec<_>>(),
            vec![21, 22, dragged],
            "persisted topology must match the live exact-index insertion"
        );
    }

    // Phase `drag_outside_every_strip_returns_tab_to_source_row`.
    {
        let (tree, s_a, _s_b, a_tabs, _b_tabs) = two_strip_two_tab_tree();
        let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let layout = lay_out_host(&mut host);

        // Far above every strip's band: the fallback splices into a new group on
        // strip A near `release.x`.
        let release = Point::new(f64::from(W) / 2.0, -100.0);
        let dragged = a_tabs[0];
        {
            let g = host
                .group_at_for_tests(s_a, 0)
                .expect("strip A group 0 widget");
            g.bar_mut_for_tests().pending_detach = Some((dragged, release));
        }
        pump_no_op(&mut host, &layout);
        host.tick_cross_strip_dispatch_for_tests();

        let snap = host.groups_per_strip();
        let strip_a_snap = snap.iter().find(|(s, _)| *s == s_a).expect("strip A");
        let total_tabs: usize = strip_a_snap.1.iter().map(|(_, ts)| ts.len()).sum();
        assert_eq!(
            total_tabs, 2,
            "strip A must still hold both tabs after a return-to-source recovery"
        );
        let dragged_present = strip_a_snap.1.iter().any(|(_, ts)| ts.contains(&dragged));
        assert!(
            dragged_present,
            "dragged tab id {dragged} must remain on strip A on the no-valid-target fallback path"
        );
    }
}
