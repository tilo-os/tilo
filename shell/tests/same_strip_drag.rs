//! Dropping a window onto a sibling group has to survive the next frame.
//!
//! The strip on screen is a *projection* of one workspace's
//! [`WorkspaceTree`], and [`ShellOverlayHost`] rebuilds that projection
//! from the tree whenever the chrome's revision moves. So a gesture that
//! moves a window in the widget tree and nowhere else is not a move at
//! all — it is a picture of one, and it lasts exactly until the next
//! thing bumps the revision, which in a live session is constantly.
//!
//! That is what "putting stuff in window groups works visually at first
//! and then weirdly reverts" is. The cross-*strip* drag has had a
//! tree-side half since #151 ([`WorkspaceTree::move_tab_across_strips`],
//! driven from `dispatch_cross_strip_detaches`). The same-strip drag —
//! by far the more common gesture, and the one that puts two
//! applications in one window — had none: `GroupRow`'s in-row detach
//! resolver spliced the tab between its own children and published
//! nothing for the shell to mirror.
//!
//! Everything here drives the real gesture: a press on the pill, a run
//! of pointer moves, a release over the target. Nothing stages a
//! `pending_detach` by hand, because the bug is not in the resolver —
//! the resolver was always right — it is in what the rest of the system
//! is told afterwards.

use std::collections::HashMap;

use shell::shell::build_nirios_overlay_host_with_chrome;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree,
};
use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, WindowEvent};

const W: f32 = 1600.0;
const H: f32 = 900.0;

/// One strip, two groups, two tabs in the left one. The smallest shape
/// that can express "drag a window out of the group it shares and onto
/// the neighbour": one fewer tab in the source and the source group
/// empties, which is a different code path.
fn one_strip_two_groups() -> (WorkspaceTree, StripId, GroupId, GroupId, [u64; 2], u64) {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();
    let g_left: GroupId = 1001;
    let g_right: GroupId = 2001;
    let left_tabs: [u64; 2] = [11, 12];
    let right_tab: u64 = 21;

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
        name: "Solo".into(),
        window_groups: vec![
            WindowGroup {
                id: g_left,
                tabs: vec![mk_tab(left_tabs[0], "L1"), mk_tab(left_tabs[1], "L2")],
                active_tab: left_tabs[0],
                width: WindowWidth::Normal,
                maximized: false,
            },
            WindowGroup {
                id: g_right,
                tabs: vec![mk_tab(right_tab, "R1")],
                active_tab: right_tab,
                width: WindowWidth::Normal,
                maximized: false,
            },
        ],
    };
    let ws = Workspace {
        strips: vec![strip],
        ..Workspace::test_default(ws_id, "Test")
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
    (tree, strip_id, g_left, g_right, left_tabs, right_tab)
}

struct Layout {
    taffy: TaffyTree<()>,
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
        child_node: child,
    }
}

fn dispatch(host: &mut ShellOverlayHost, layout: &Layout, event: WindowEvent) {
    let mut doc = automerge::AutoCommit::new();
    let doc_root = automerge::ROOT.clone();
    let modifiers = winit::event::Modifiers::default();
    let mut handled = false;
    let mut focus: Option<NodeId> = None;
    host.handle_event(
        &event,
        1.0,
        &modifiers,
        &mut doc,
        &doc_root,
        &mut handled,
        &mut focus,
        layout.child_node,
        &layout.taffy,
    );
}

fn cursor_to(host: &mut ShellOverlayHost, layout: &Layout, p: Point) {
    dispatch(
        host,
        layout,
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(p.x, p.y),
        },
    );
}

fn button(host: &mut ShellOverlayHost, layout: &Layout, state: ElementState) {
    dispatch(
        host,
        layout,
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state,
            button: MouseButton::Left,
        },
    );
}

/// Press at `from`, travel to `to` through `steps` intermediate moves,
/// release. The moves are individual because every one of them is a
/// frame the widget tree reacts to — a teleport would skip the drag
/// entirely and never pass the press threshold.
fn drag(host: &mut ShellOverlayHost, layout: &Layout, from: Point, to: Point, steps: usize) {
    cursor_to(host, layout, from);
    button(host, layout, ElementState::Pressed);
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        cursor_to(
            host,
            layout,
            Point::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t),
        );
    }
    button(host, layout, ElementState::Released);
}

/// Where tab `tab_id`'s pill is painted right now, in screen space.
fn pill_centre(host: &mut ShellOverlayHost, strip: StripId, group_idx: usize, tab_id: u64) -> Point {
    let g = host
        .group_at_for_tests(strip, group_idx)
        .expect("group widget");
    let idx = g
        .tabs()
        .iter()
        .position(|t| t.id == tab_id)
        .expect("the dragged tab must have a pill");
    g.tab_rects()[idx].center()
}

/// Middle of group `group_idx`'s tab bar — the drop target for "put this
/// window in with that one".
fn bar_centre(host: &mut ShellOverlayHost, strip: StripId, group_idx: usize) -> Point {
    host.group_at_for_tests(strip, group_idx)
        .expect("group widget")
        .bar_rect()
        .center()
}

/// Which group each tab lives in, as the widget tree has it.
fn widget_layout(host: &ShellOverlayHost, strip: StripId) -> Vec<(u64, Vec<u64>)> {
    host.groups_per_strip()
        .into_iter()
        .find(|(s, _)| *s == strip)
        .map(|(_, gs)| gs)
        .unwrap_or_default()
}

/// Which group each tab lives in, as the persisted tree has it.
fn tree_layout(chrome: &shell::shell::chrome_state::Chrome, strip: StripId) -> Vec<(u64, Vec<u64>)> {
    let st = chrome.borrow();
    let tree = st.tree();
    tree.active_workspace()
        .and_then(|ws| ws.strips.iter().find(|s| s.id == strip))
        .map(|s| {
            s.window_groups
                .iter()
                .map(|g| (g.id, g.tabs.iter().map(|t| t.id).collect()))
                .collect()
        })
        .unwrap_or_default()
}

/// The rest of the frame the drop happened on — the production
/// `Widget::update`, which is where every widget-to-tree mirror runs.
///
/// Called rather than a per-harvest test hook on purpose: a hook proves
/// the mirror works, not that anything calls it, and "the writeback
/// exists but nothing runs it" is the same bug from the user's side.
fn settle(host: &mut ShellOverlayHost) {
    let mut doc = automerge::AutoCommit::new();
    let doc_root = automerge::ROOT.clone();
    host.update(&mut doc, &doc_root);
}

/// A later frame, on which something bumped the revision and the strip
/// rows were rebuilt from the tree.
///
/// Any of a hundred ordinary things bumps it in a live session: a
/// notification arriving, a peer's presence changing, a window opening
/// on another machine. The test does it explicitly because the claim is
/// that the drop survives one, not that any particular event causes it.
fn next_frame(host: &mut ShellOverlayHost, chrome: &shell::shell::chrome_state::Chrome) {
    chrome.borrow_mut().bump();
    let _ = host.take_structure_dirty();
}

#[test]
fn a_window_dropped_on_a_sibling_group_stays_there() {
    let (tree, strip, g_left, g_right, left_tabs, right_tab) = one_strip_two_groups();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);
    let dragged = left_tabs[1];

    assert_eq!(
        widget_layout(&host, strip),
        vec![
            (g_left, vec![left_tabs[0], dragged]),
            (g_right, vec![right_tab])
        ],
        "two groups, the left one holding both windows",
    );

    let from = pill_centre(&mut host, strip, 0, dragged);
    let to = bar_centre(&mut host, strip, 1);
    drag(&mut host, &layout, from, to, 12);

    // What the person sees the instant they let go.
    assert_eq!(
        widget_layout(&host, strip),
        vec![(g_left, vec![left_tabs[0]]), (g_right, vec![right_tab, dragged])],
        "the drop moved the window onto the neighbour's bar",
    );

    settle(&mut host);

    // The frame after. This is the revert the user reported:
    // `resync_strips_from_tree` diffs every row against the tree, and a
    // tree that still holds the pre-drag arrangement puts the window
    // back where it came from.
    next_frame(&mut host, &chrome);
    assert_eq!(
        widget_layout(&host, strip),
        vec![(g_left, vec![left_tabs[0]]), (g_right, vec![right_tab, dragged])],
        "the window is still on the neighbour a frame later",
    );

    // …and why. The tree the shell rebuilds from — the one that
    // survives a restart and the one peers read — has to have been told.
    assert_eq!(
        tree_layout(&chrome, strip),
        vec![(g_left, vec![left_tabs[0]]), (g_right, vec![right_tab, dragged])],
        "the workspace tree records the move the person just made",
    );
}

#[test]
fn a_window_dragged_into_open_space_keeps_its_new_window() {
    // The other half of the same gesture: dropped in the gap rather
    // than on a bar, which splits the window out into one of its own.
    // Same failure — a new group minted widget-side that the tree never
    // hears about is a group the next resync deletes.
    let (tree, strip, g_left, g_right, left_tabs, _right_tab) = one_strip_two_groups();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);
    let dragged = left_tabs[1];

    let from = pill_centre(&mut host, strip, 0, dragged);
    // The gap between the two groups, at bar height: no sibling bar
    // contains it, so the in-row resolver takes the split path.
    let left_rect = host
        .group_at_for_tests(strip, 0)
        .expect("left group")
        .get_rect();
    let right_rect = host
        .group_at_for_tests(strip, 1)
        .expect("right group")
        .get_rect();
    let to = Point::new(
        (left_rect.x1 + right_rect.x0) / 2.0,
        bar_centre(&mut host, strip, 1).y,
    );
    drag(&mut host, &layout, from, to, 12);

    let after_drop = widget_layout(&host, strip);
    assert_eq!(
        after_drop.len(),
        3,
        "the window came out into a third group of its own: {after_drop:?}",
    );
    let new_group = after_drop
        .iter()
        .find(|(id, tabs)| *id != g_left && *id != g_right && tabs == &vec![dragged])
        .map(|(id, _)| *id)
        .unwrap_or_else(|| panic!("no new single-window group holds {dragged}: {after_drop:?}"));

    settle(&mut host);

    next_frame(&mut host, &chrome);
    assert_eq!(
        widget_layout(&host, strip),
        after_drop,
        "the new window is still there a frame later",
    );
    assert_eq!(
        tree_layout(&chrome, strip),
        after_drop,
        "the tree carries the new window under the id the strip minted, \
         group {new_group} included",
    );
}
