//! Cursor-targeted file-drop routing.
//!
//! `WindowEvent::DroppedFile` carries a path but NO coordinate, so the
//! compositor tracks the pointer from `CursorMoved` and routes the drop
//! to the window group under the last-known position. The test builds two
//! side-by-side groups with laid-out rects, then sends `CursorMoved` +
//! `DroppedFile` through
//! `ShellOverlayHost::handle_event` — winit's own entry point in the
//! live binary.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::shell::state::FreshId as _;
use shell::shell::state::WorkspaceAuthority as _;
use std::collections::HashMap;

use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::chrome_state::Chrome;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{
    AppKind, Capability, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
    Workspace, WorkspaceId, WorkspaceTree,
};
use ui::widget::Widget as _;
use vello::kurbo::Point;
use winit::event::{Modifiers, WindowEvent};

use crate::support::host::{Layout, dispatch, lay_out};

const W: f32 = 1280.0;
const H: f32 = 800.0;

/// Two `WindowGroup`s at `Narrow` width (400px each) so both fit inside
/// the 1280px viewport with distinct, non-overlapping rects, letting the
/// cursor's x-coordinate tell them apart.
fn two_group_one_strip_tree() -> (WorkspaceTree, StripId, GroupId, GroupId) {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();
    let g_left: GroupId = 1001;
    let g_right: GroupId = 2001;

    let mk_tab = |id: u64, label: &str| Tab {
        id,
        title: label.into(),
        tile: TileContent::Stub {
            kind: AppKind::Other("Test".into()),
            label: label.into(),
        },
    };

    let strip = Strip {
        id: strip_id,
        name: "Main".into(),
        window_groups: vec![
            WindowGroup {
                id: g_left,
                tabs: vec![mk_tab(11, "L1")],
                active_tab: 11,
                width: WindowWidth::Narrow,
                maximized: false,
            },
            WindowGroup {
                id: g_right,
                tabs: vec![mk_tab(21, "R1")],
                active_tab: 21,
                width: WindowWidth::Narrow,
                maximized: false,
            },
        ],
    };
    let ws = Workspace {
        strips: vec![strip],
        // Everything else, including the workspace's durable collaboration room,
        // comes from the builder, so a field added to `Workspace` does not break
        // this fixture.
        ..Workspace::new(ws_id, "Test")
    };

    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, strip_id);
    let mut active_group = HashMap::new();
    active_group.insert(strip_id, g_left);

    let tree = WorkspaceTree {
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

    (tree, strip_id, g_left, g_right)
}

/// `build_layout -> compute_layout -> apply_layout`, mirroring
/// `App::layout`. Without it `pick_group_at` compares against
/// `Rect::ZERO` and never matches.
fn lay_out_host(host: &mut ShellOverlayHost) -> Layout {
    lay_out(host, W, H)
}

fn pump(host: &mut ShellOverlayHost, layout: &Layout, event: &WindowEvent) {
    dispatch(
        host,
        &layout.taffy,
        layout.child_node,
        Modifiers::default(),
        event,
    );
}

/// Move the pointer to `p` (seeds `last_cursor`), then drop `path` —
/// exactly the two-event sequence winit delivers for a drag-and-drop
/// ending over the window.
fn hover_and_drop(host: &mut ShellOverlayHost, layout: &Layout, p: Point, path: &str) {
    pump(host, layout, &ui::synthetic_input::cursor_moved(p.x, p.y));
    pump(
        host,
        layout,
        &WindowEvent::DroppedFile(std::path::PathBuf::from(path)),
    );
}

fn tab_count(chrome: &Chrome, group_id: GroupId) -> usize {
    let st = chrome.borrow();
    let tree = st.tree();
    tree.desktop.workspaces[0]
        .strips
        .iter()
        .flat_map(|s| s.window_groups.iter())
        .find(|g| g.id == group_id)
        .map_or_else(
            || panic!("group {group_id} must exist in the persisted tree"),
            |g| g.tabs.len(),
        )
}

fn group_center(host: &mut ShellOverlayHost, strip_id: StripId, group_idx: usize) -> Point {
    host.group_at_for_tests(strip_id, group_idx)
        .expect("group must exist after layout")
        .get_rect()
        .center()
}

#[test]
fn file_drops_route_to_the_group_they_land_on_or_do_nothing() {
    // Phase `drop_over_left_group_routes_to_left_group_only`.
    {
        let (tree, strip_id, g_left, g_right) = two_group_one_strip_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let layout = lay_out_host(&mut host);

        let left_before = tab_count(&chrome, g_left);
        let right_before = tab_count(&chrome, g_right);
        let center = group_center(&mut host, strip_id, 0);

        hover_and_drop(&mut host, &layout, center, "/drop/component.ts");

        assert_eq!(
            tab_count(&chrome, g_left),
            left_before + 1,
            "the group under the cursor must gain the dropped file's tab"
        );
        assert_eq!(
            tab_count(&chrome, g_right),
            right_before,
            "the neighbouring group must NOT receive the drop"
        );
    }

    // Phase `drop_over_right_group_routes_to_right_group_only`.
    {
        let (tree, strip_id, g_left, g_right) = two_group_one_strip_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let layout = lay_out_host(&mut host);

        let left_before = tab_count(&chrome, g_left);
        let right_before = tab_count(&chrome, g_right);
        let center = group_center(&mut host, strip_id, 1);

        hover_and_drop(&mut host, &layout, center, "/drop/component.ts");

        assert_eq!(
            tab_count(&chrome, g_right),
            right_before + 1,
            "the group under the cursor must gain the dropped file's tab"
        );
        assert_eq!(
            tab_count(&chrome, g_left),
            left_before,
            "the neighbouring group must NOT receive the drop"
        );
    }

    // Phase `two_drops_route_to_the_two_distinct_groups_they_land_on`.
    {
        let (tree, strip_id, g_left, g_right) = two_group_one_strip_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let layout = lay_out_host(&mut host);

        let left_before = tab_count(&chrome, g_left);
        let right_before = tab_count(&chrome, g_right);

        // An extension no app claims, as everywhere else in this file. A claimed
        // type takes the document-open path instead of the placeholder-tab one,
        // and that path needs a file that exists — which is a different test than
        // this one, whose subject is which group a drop lands on.
        let left_center = group_center(&mut host, strip_id, 0);
        hover_and_drop(&mut host, &layout, left_center, "/drop/left.ts");

        let right_center = group_center(&mut host, strip_id, 1);
        hover_and_drop(&mut host, &layout, right_center, "/drop/right.ts");

        assert_eq!(
            tab_count(&chrome, g_left),
            left_before + 1,
            "the first drop must stay on the left group"
        );
        assert_eq!(
            tab_count(&chrome, g_right),
            right_before + 1,
            "the second drop must land on the right group, not the left"
        );
    }

    // Phase `drop_over_empty_desktop_space_is_a_graceful_noop`.
    {
        let (tree, _strip_id, g_left, g_right) = two_group_one_strip_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let layout = lay_out_host(&mut host);

        let left_before = tab_count(&chrome, g_left);
        let right_before = tab_count(&chrome, g_right);

        // (5, 5) is the top-left desktop corner, above/left of every group rect.
        hover_and_drop(&mut host, &layout, Point::new(5.0, 5.0), "/drop/stray.ts");

        assert_eq!(
            tab_count(&chrome, g_left),
            left_before,
            "an empty-area drop must not add a tab to any group"
        );
        assert_eq!(
            tab_count(&chrome, g_right),
            right_before,
            "an empty-area drop must not add a tab to any group"
        );
    }
}

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the retained tree borrow is used for adjacent tab-order assertions; the retained tree borrow is used for the complete seam topology assertion"
)]
fn file_drops_insert_before_a_tab_or_open_a_group_at_a_seam() {
    // Phase `drop_on_left_side_of_first_tab_inserts_before_it`.
    {
        let (tree, strip_id, g_left, _g_right) = two_group_one_strip_tree();
        let original_first = tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].id;
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let layout = lay_out_host(&mut host);
        let first_tab = host
            .group_at_for_tests(strip_id, 0)
            .and_then(|group| group.tab_rects().first().copied())
            .expect("first tab has geometry");

        hover_and_drop(
            &mut host,
            &layout,
            Point::new(first_tab.x0 + 1.0, first_tab.center().y),
            "/drop/before.ts",
        );

        let state = chrome.borrow();
        let tree = state.tree();
        let group = tree.desktop.workspaces[0].strips[0]
            .window_groups
            .iter()
            .find(|group| group.id == g_left)
            .unwrap();
        assert_eq!(group.tabs[0].title, "before.ts");
        assert_eq!(group.tabs[1].id, original_first);
    }

    // Phase `drop_between_groups_creates_a_group_at_that_seam`.
    {
        let (tree, strip_id, g_left, g_right) = two_group_one_strip_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let layout = lay_out_host(&mut host);
        let left = host
            .group_at_for_tests(strip_id, 0)
            .expect("left group")
            .get_rect();
        let right = host
            .group_at_for_tests(strip_id, 1)
            .expect("right group")
            .get_rect();

        hover_and_drop(
            &mut host,
            &layout,
            Point::new(f64::midpoint(left.x1, right.x0), left.center().y),
            "/drop/between.unknown",
        );

        let state = chrome.borrow();
        let tree = state.tree();
        let groups = &tree.desktop.workspaces[0].strips[0].window_groups;
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].id, g_left);
        assert_eq!(groups[1].tabs[0].title, "between.unknown");
        assert_eq!(groups[2].id, g_right);
    }
}

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "st's only use is producing tree via tree(), read through to the end of the \
function via ws/view/editor_caps; nothing to drop earlier; st's only use is producing tree via tree(), read through to the end of the \
function via view/editor_caps/count; nothing to drop earlier"
)]
fn file_drops_record_one_filesystem_grant_for_the_target_app() {
    // Phase `drop_records_filesystem_grant_for_target_app_on_the_workspace`.
    {
        let (tree, strip_id, _g_left, _g_right) = two_group_one_strip_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let layout = lay_out_host(&mut host);

        let dropped = "/drop/notes.ts"; // .ts → Editor app
        let center = group_center(&mut host, strip_id, 0);
        hover_and_drop(&mut host, &layout, center, dropped);

        let st = chrome.borrow();
        let tree = st.tree();
        let ws = &tree.desktop.workspaces[0];
        // The drop must record a per-app filesystem capability on the target
        // workspace — the same grants map the portal/permission-prompt path
        // writes into — keyed by the built-in Editor's canonical app-id (what
        // it advertises via `xdg_toplevel::set_app_id`), not the display label.
        let editor_key = AppKind::Other("Editor".into()).app_id();
        assert_eq!(
            editor_key, "org.tilo.Editor",
            "the Editor app-id must be a real reverse-DNS app-id, not the \
             display label 'Editor'"
        );
        // `grants()` derives the per-app view from the capability list, so it is
        // bound before borrowing out of it.
        let view = ws.app_grants_snapshot();
        let editor_caps = view
            .get(&editor_key)
            .unwrap_or_else(|| panic!("a .ts drop must record a grant under {editor_key}"));
        let granted_path = editor_caps.iter().any(|cap| {
            matches!(
                cap,
                Capability::Filesystem { path, .. } if path == std::path::Path::new(dropped)
            )
        });
        assert!(
            granted_path,
            "the dropped path must appear as a Filesystem capability; got {editor_caps:?}"
        );
    }

    // Phase `repeated_drop_of_same_file_does_not_duplicate_the_grant`.
    // The second drop must see the path is already granted and skip the
    // insert rather than stacking a duplicate capability.
    {
        let (tree, strip_id, _g_left, _g_right) = two_group_one_strip_tree();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let layout = lay_out_host(&mut host);

        let dropped = "/drop/dup.ts";
        let center = group_center(&mut host, strip_id, 0);
        hover_and_drop(&mut host, &layout, center, dropped);
        // Re-fetch the centre: the first drop added a tab, so the group must be
        // re-laid-out before the second drop.
        let layout = lay_out_host(&mut host);
        let center = group_center(&mut host, strip_id, 0);
        hover_and_drop(&mut host, &layout, center, dropped);

        let st = chrome.borrow();
        let tree = st.tree();
        let editor_key = AppKind::Other("Editor".into()).app_id();
        let view = tree.desktop.workspaces[0].app_grants_snapshot();
        let editor_caps = view.get(&editor_key).expect("grant present");
        let count = editor_caps
            .iter()
            .filter(|cap| {
                matches!(
                    cap,
                    Capability::Filesystem { path, .. } if path == std::path::Path::new(dropped)
                )
            })
            .count();
        assert_eq!(
            count, 1,
            "dropping the same file twice must record exactly one grant, got {count}"
        );
    }
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use shell::shell::state::WorkspaceExt as _;
