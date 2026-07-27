//! Cursor-targeted file-drop routing.
//!
//! `WindowEvent::DroppedFile` carries a path but NO coordinate, so the
//! compositor tracks the pointer from `CursorMoved` and routes the drop
//! to the window group under the last-known position. These tests drive
//! the REAL production path end-to-end:
//!
//!   1. Build a workspace with two side-by-side window groups in one
//!      strip and run a full layout pass so each group's on-screen rect
//!      is populated (the same rects `pick_group_at` hit-tests).
//!   2. Pump a `CursorMoved` over a chosen group (seeds `last_cursor`),
//!      then a `DroppedFile` through `ShellOverlayHost::handle_event` —
//!      the identical entry point winit calls in the live binary.
//!   3. Assert the dropped file's tab landed in the group under the
//!      cursor and NOT its neighbour, that dropping over each group in
//!      turn routes to that group (multi-window correctness), that a
//!      drop over empty desktop space is a graceful no-op, and that the
//!      drop records a per-workspace filesystem capability grant for the
//!      target app (sandbox honoured).

use std::collections::HashMap;

use shell::shell::build_nirios_overlay_host_with_chrome;
use shell::shell::chrome_state::Chrome;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{
    AppKind, Capability, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
    Workspace, WorkspaceId, WorkspaceTree,
};
use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::dpi::PhysicalPosition;
use winit::event::WindowEvent;

const W: f32 = 1280.0;
const H: f32 = 800.0;

/// One workspace, one strip, two `WindowGroup`s side by side. `Narrow`
/// width (400 px each) so both groups fit inside the 1280 px viewport
/// with distinct, non-overlapping rects — that's what lets the cursor's
/// x-coordinate tell "left window" apart from "right window".
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
        id: ws_id,
        name: "Test".into(),
        strips: vec![strip],
        roles: vec![],
        workspace_files: vec![],
        collaborators: vec![],
        grants: HashMap::new(),
        shared_surfaces: HashMap::new(),
    };

    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, strip_id);
    let mut active_group = HashMap::new();
    active_group.insert(strip_id, g_left);

    let tree = WorkspaceTree {
        workspaces: vec![ws],
        active: ws_id,
        active_strip,
        active_group,
        collaborators: vec![],
        ..Default::default()
    };

    (tree, strip_id, g_left, g_right)
}

/// Run `build_layout → compute_layout → apply_layout` so every group's
/// stored rect lands in screen-space. Mirrors `App::layout` in the run
/// loop. Without it `pick_group_at` compares the drop point against
/// `Rect::ZERO` and never matches.
struct Layout {
    taffy: TaffyTree<()>,
    root_node: NodeId,
    child_node: NodeId,
}

fn lay_out_host(host: &mut ShellOverlayHost) -> Layout {
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root_node = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(W),
                    height: Dimension::length(H),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();

    let mut tr = ui::text_render::TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root_node,
            Size {
                width: AvailableSpace::Definite(W),
                height: AvailableSpace::Definite(H),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    host.apply_layout(&taffy, child, Point::ORIGIN);
    Layout {
        taffy,
        root_node,
        child_node: child,
    }
}

/// Pump one `WindowEvent` through the host's real `handle_event`.
fn pump(host: &mut ShellOverlayHost, layout: &Layout, event: &WindowEvent) {
    let mut doc = automerge::AutoCommit::new();
    let doc_root = automerge::ROOT.clone();
    let modifiers = winit::event::Modifiers::default();
    let mut handled = false;
    let mut focus: Option<NodeId> = None;
    host.handle_event(
        event,
        1.0,
        &modifiers,
        &mut doc,
        &doc_root,
        &mut handled,
        &mut focus,
        layout.child_node,
        &layout.taffy,
    );
    let _ = layout.root_node;
}

/// Move the pointer to `p` (seeds `last_cursor`), then drop `path`.
/// This is exactly the two-event sequence winit delivers for a
/// drag-and-drop that ends over the window.
fn hover_and_drop(host: &mut ShellOverlayHost, layout: &Layout, p: Point, path: &str) {
    pump(
        host,
        layout,
        &WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(p.x, p.y),
        },
    );
    pump(
        host,
        layout,
        &WindowEvent::DroppedFile(std::path::PathBuf::from(path)),
    );
}

/// Tab count of the persisted-tree group with id `group_id`.
fn tab_count(chrome: &Chrome, group_id: GroupId) -> usize {
    let st = chrome.borrow();
    let tree = st.tree();
    tree.workspaces[0]
        .strips
        .iter()
        .flat_map(|s| s.window_groups.iter())
        .find(|g| g.id == group_id)
        .map(|g| g.tabs.len())
        .unwrap_or_else(|| panic!("group {group_id} must exist in the persisted tree"))
}

/// Centre point of the laid-out group at `group_idx` in `strip_id`.
fn group_center(host: &mut ShellOverlayHost, strip_id: StripId, group_idx: usize) -> Point {
    host.group_at_for_tests(strip_id, group_idx)
        .expect("group must exist after layout")
        .get_rect()
        .center()
}

#[test]
fn drop_over_left_group_routes_to_left_group_only() {
    let (tree, strip_id, g_left, g_right) = two_group_one_strip_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
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

#[test]
fn drop_over_right_group_routes_to_right_group_only() {
    let (tree, strip_id, g_left, g_right) = two_group_one_strip_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
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

/// The load-bearing multi-window guarantee: two drops at two different
/// positions in the SAME session must each land on their own group.
#[test]
fn two_drops_route_to_the_two_distinct_groups_they_land_on() {
    let (tree, strip_id, g_left, g_right) = two_group_one_strip_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);

    let left_before = tab_count(&chrome, g_left);
    let right_before = tab_count(&chrome, g_right);

    let left_center = group_center(&mut host, strip_id, 0);
    hover_and_drop(&mut host, &layout, left_center, "/drop/left.md");

    let right_center = group_center(&mut host, strip_id, 1);
    hover_and_drop(&mut host, &layout, right_center, "/drop/right.md");

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

#[test]
fn drop_over_empty_desktop_space_is_a_graceful_noop() {
    let (tree, _strip_id, g_left, g_right) = two_group_one_strip_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);

    let left_before = tab_count(&chrome, g_left);
    let right_before = tab_count(&chrome, g_right);

    // (5, 5) is the top-left desktop corner — above/left of every group
    // rect. No panic, and no window receives a stray tab.
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

#[test]
fn drop_records_filesystem_grant_for_target_app_on_the_workspace() {
    let (tree, strip_id, _g_left, _g_right) = two_group_one_strip_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);

    let dropped = "/drop/notes.ts"; // .ts → Editor app
    let center = group_center(&mut host, strip_id, 0);
    hover_and_drop(&mut host, &layout, center, dropped);

    let st = chrome.borrow();
    let tree = st.tree();
    let ws = &tree.workspaces[0];
    // The dropped file must be recorded as a per-app filesystem
    // capability on the target workspace — the same grants map the
    // portal / permission-prompt path writes into. The key is the
    // built-in Editor's canonical app-id (the same value it would
    // advertise via `xdg_toplevel::set_app_id`), NOT the display label.
    let editor_key = AppKind::Other("Editor".into()).app_id();
    assert_eq!(
        editor_key, "org.nirios.Editor",
        "the Editor app-id must be a real reverse-DNS app-id, not the \
         display label 'Editor'"
    );
    let editor_caps = ws
        .grants
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

/// Idempotency: dropping the same file twice onto the same app must not
/// stack duplicate filesystem capabilities — the second drop sees the
/// path is already granted and skips the grant insert.
#[test]
fn repeated_drop_of_same_file_does_not_duplicate_the_grant() {
    let (tree, strip_id, _g_left, _g_right) = two_group_one_strip_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);

    let dropped = "/drop/dup.ts";
    let center = group_center(&mut host, strip_id, 0);
    hover_and_drop(&mut host, &layout, center, dropped);
    // Re-fetch the centre — the first drop added a tab, so re-layout the
    // group before the second drop.
    let layout = lay_out_host(&mut host);
    let center = group_center(&mut host, strip_id, 0);
    hover_and_drop(&mut host, &layout, center, dropped);

    let st = chrome.borrow();
    let tree = st.tree();
    let editor_key = AppKind::Other("Editor".into()).app_id();
    let editor_caps = tree.workspaces[0]
        .grants
        .get(&editor_key)
        .expect("grant present");
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
