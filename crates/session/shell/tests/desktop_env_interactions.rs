//! End-to-end coverage for desktop-environment window management —
//! window-group resize and reorder, strip travel, and drag-file-to-open —
//! driven through the [`ShellOverlayHost`] event/state path.
//!
//! Harness mirrors `cross_strip_drag.rs`: build a tree, hand it to
//! [`build_tilo_overlay_host_with_chrome`], run a layout pass, then drive
//! winit events and the per-frame harvest hooks used by `Widget::update`.

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
use std::collections::HashMap;

use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::chrome_state::Chrome;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree,
};
use shell::shell::topbar_chrome::TopBarChrome;
use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};

const W: f32 = 1280.0;
const H: f32 = 800.0;

macro_rules! assert_approx_eq {
    ($left:expr, $right:expr $(,)?) => {{
        let (left, right) = ($left, $right);
        assert!((left - right).abs() <= 1.0e-5, "left={left:?}, right={right:?}");
    }};
    ($left:expr, $right:expr, $($arg:tt)+) => {{
        let (left, right) = ($left, $right);
        assert!((left - right).abs() <= 1.0e-5, $($arg)+);
    }};
}

/// One workspace, one strip, one tab per group. Group ids are the
/// caller's so failure messages stay legible; the first is the strip's
/// active group.
fn single_strip_tree(group_ids: &[u64]) -> (WorkspaceTree, StripId, Vec<GroupId>) {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();

    let mk_tab = |gid: u64| Tab {
        id: gid.saturating_mul(10).saturating_add(1),
        title: format!("G{gid}"),
        tile: TileContent::Stub {
            kind: AppKind::Other("Test".into()),
            label: format!("G{gid}"),
        },
    };
    let groups: Vec<WindowGroup> = group_ids
        .iter()
        .map(|&gid| WindowGroup {
            id: gid,
            tabs: vec![mk_tab(gid)],
            active_tab: gid.saturating_mul(10).saturating_add(1),
            width: WindowWidth::Normal,
            maximized: false,
        })
        .collect();

    let strip = Strip {
        id: strip_id,
        name: "Main".into(),
        window_groups: groups,
    };
    let ws = Workspace {
        strips: vec![strip],
        // Everything else comes from the builder, so a field added to
        // `Workspace` does not break this fixture.
        ..Workspace::new(ws_id, "Test")
    };

    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, strip_id);
    let mut active_group = HashMap::new();
    active_group.insert(strip_id, group_ids[0]);

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
    (tree, strip_id, group_ids.to_vec())
}

struct Layout {
    taffy: TaffyTree<()>,
    #[allow(
        dead_code,
        reason = "the fixture retains the complete Taffy tree even when a test inspects only child geometry"
    )]
    root_node: NodeId,
    child_node: NodeId,
}

/// Usable strip width at the fixture's viewport (window minus the
/// wheel-disc slot + gutter). Group widths are fractions of this, so a
/// pixel expectation only means anything alongside it; read off the live
/// row so the test can't drift from what the shell reserves.
fn usable_strip_px(host: &mut ShellOverlayHost, strip_id: StripId) -> f64 {
    host.group_row_mut_for_tests(strip_id)
        .expect("strip GroupRow")
        .usable_at(0.0)
}

fn lay_out_host(host: &mut ShellOverlayHost) -> Layout {
    // What the compositor does before every build: name the display, so
    // window-group width fractions resolve against it rather than the
    // toolkit's fallback.
    host.set_output_geometry(&[shell::shell::overlay_host::OutputGeometry {
        x: 0.0,
        width: f64::from(W),
        height: f64::from(H),
    }]);
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

fn laid_out_topbar_height(chrome: Chrome) -> f64 {
    let tree = chrome.borrow().tree().clone();
    let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
    taffy
        .compute_layout(
            node,
            Size {
                width: AvailableSpace::Definite(W),
                height: AvailableSpace::Definite(H),
            },
        )
        .expect("compute top-bar layout");
    bar.apply_layout(&taffy, node, Point::ORIGIN);
    bar.get_rect().height()
}

fn dispatch(host: &mut ShellOverlayHost, layout: &Layout, event: &WindowEvent) {
    let mut doc = automerge::AutoCommit::new();
    let doc_root = automerge::ROOT.clone();
    let modifiers = Modifiers::default();
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
}

/// Hover a group's first tab and hold its chip-reveal curve past the end, so
/// the per-tab chips have hit rects to click.
///
/// The cluster slides out on wall time: `TabBar::advance_reveal` starts the
/// curve and returns where it started, so the rects stay `Rect::ZERO` until it
/// has run. A test that hovers and reads straight back always reads zero and
/// clicks nothing. `hold_reveal_at` is the renderer's own escape from that —
/// an offscreen frame costs more wall time than the curve lasts — and layout
/// has to run again afterwards, since `apply_layout` is what places the rects.
fn reveal_tab_chips(host: &mut ShellOverlayHost, layout: &Layout, strip_id: StripId) -> Layout {
    let pill = host
        .group_at_for_tests(strip_id, 0)
        .expect("group after layout")
        .tab_rects()
        .first()
        .copied()
        .expect("a pill")
        .center();
    cursor(host, layout, pill);
    // The curve is created by the layout pass that first sees the new hover
    // target, so it has to exist before it can be held.
    let layout = lay_out_host(host);
    let mut held = false;
    if let Some(row) = host.group_row_mut_for_tests(strip_id) {
        for g in &mut row.groups {
            held |= g
                .bar_mut_for_tests()
                .hold_reveal_at(std::time::Duration::from_secs(1));
        }
    }
    assert!(
        held,
        "hovering the pill started no reveal, so there is nothing to click",
    );
    let _ = layout;
    lay_out_host(host)
}

fn cursor(host: &mut ShellOverlayHost, layout: &Layout, p: Point) {
    dispatch(
        host,
        layout,
        &WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(p.x, p.y),
        },
    );
}

fn press(host: &mut ShellOverlayHost, layout: &Layout) {
    dispatch(
        host,
        layout,
        &WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
    );
}

fn release(host: &mut ShellOverlayHost, layout: &Layout) {
    dispatch(
        host,
        layout,
        &WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        },
    );
}

// 1. Horizontal window-group resize

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "st's only use is producing tree via tree(); the read-guard's lifetime \
              is tied to st's borrow, so it can't be dropped before tree's last use"
)]
fn resize_handle_drag_persists_new_group_widths_into_tree() {
    let (tree, strip_id, ids) = single_strip_tree(&[100, 200]);
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);

    let usable = usable_strip_px(&mut host, strip_id);
    let start_px = WindowWidth::Normal.resolve(usable);
    {
        let st = chrome.borrow();
        let tree = st.tree();
        let g = &tree.desktop.workspaces[0].strips[0].window_groups;
        assert_approx_eq!(g[0].width.resolve(usable), start_px);
        assert_approx_eq!(g[1].width.resolve(usable), start_px);
    }

    let handle_rect = host
        .group_row_mut_for_tests(strip_id)
        .expect("strip GroupRow")
        .handle_at(0)
        .expect("one inter-group resize handle for a 2-group strip")
        .get_rect();
    let cx = f64::midpoint(handle_rect.x0, handle_rect.x1);
    let cy = f64::midpoint(handle_rect.y0, handle_rect.y1);

    // Drag the handle +40 px right: left neighbour grows, right shrinks.
    cursor(&mut host, &layout, Point::new(cx, cy));
    press(&mut host, &layout);
    cursor(&mut host, &layout, Point::new(cx + 40.0, cy));
    release(&mut host, &layout);

    // The production `Widget::update` runs this harvest each frame.
    host.tick_pending_width_harvest_for_tests();

    let st = chrome.borrow();
    let tree = st.tree();
    let groups = &tree.desktop.workspaces[0].strips[0].window_groups;
    let left = groups.iter().find(|g| g.id == ids[0]).unwrap();
    let right = groups.iter().find(|g| g.id == ids[1]).unwrap();
    assert_approx_eq!(
        left.width.resolve(usable),
        start_px + 40.0,
        "left group must have grown by the +40 px drag delta in the persisted tree",
    );
    assert_approx_eq!(
        right.width.resolve(usable),
        start_px - 40.0,
        "right group must have shrunk by the same 40 px (no overlap, widths reflow)",
    );
    // What is *stored* is the share, not the pixels — the same tree on a
    // wider display gives the same proportions, not the same widths.
    assert!(
        matches!(left.width, WindowWidth::Fraction(_)),
        "a resize persists as a fraction of the strip; got {:?}",
        left.width,
    );
}

/// A drag far past a neighbour's minimum must clamp: the shrinking group
/// pins at the display's minimum and the growing one absorbs only the
/// slack, preserving the pair sum, and the clamped widths persist.
#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "st's only use is producing tree via tree(); the read-guard's lifetime \
              is tied to st's borrow, so it can't be dropped before tree's last use"
)]
fn resize_handle_drag_clamps_at_min_width_and_persists() {
    let (mut tree, strip_id, ids) = single_strip_tree(&[100, 200]);
    // Start both at Narrow. From two Normals the right group can't reach its
    // minimum at all (the left would grow past the display), so the pair sum
    // has to leave slack for the clamp to be reachable.
    for g in &mut tree.desktop.workspaces[0].strips[0].window_groups {
        g.width = WindowWidth::Narrow;
    }
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);

    let usable = usable_strip_px(&mut host, strip_id);
    let pair_sum = 2.0 * WindowWidth::Narrow.resolve(usable);
    let min = ui::widget::min_group_px(usable);
    let handle_rect = host
        .group_row_mut_for_tests(strip_id)
        .expect("strip GroupRow")
        .handle_at(0)
        .expect("one inter-group resize handle for a 2-group strip")
        .get_rect();
    let cx = f64::midpoint(handle_rect.x0, handle_rect.x1);
    let cy = f64::midpoint(handle_rect.y0, handle_rect.y1);

    // Far past the right neighbour's min: it can only shrink to the
    // display's minimum, so the left group grows by that slack, not by 5000.
    cursor(&mut host, &layout, Point::new(cx, cy));
    press(&mut host, &layout);
    cursor(&mut host, &layout, Point::new(cx + 5000.0, cy));
    release(&mut host, &layout);

    host.tick_pending_width_harvest_for_tests();

    let st = chrome.borrow();
    let tree = st.tree();
    let groups = &tree.desktop.workspaces[0].strips[0].window_groups;
    let left = groups.iter().find(|g| g.id == ids[0]).unwrap();
    let right = groups.iter().find(|g| g.id == ids[1]).unwrap();
    assert_approx_eq!(
        right.width.resolve(usable),
        min,
        "the shrinking (right) group clamps at the display's minimum width, not below",
    );
    assert_approx_eq!(
        left.width.resolve(usable),
        pair_sum - min,
        "the growing (left) group absorbs only the neighbour's actual slack",
    );
    assert_approx_eq!(
        left.width.resolve(usable) + right.width.resolve(usable),
        pair_sum,
        "pair sum is preserved under clamp — no overlap, no runaway width",
    );
}

// 2. Whole-group reorder

fn group_grab_point(host: &mut ShellOverlayHost, strip_id: StripId, group_idx: usize) -> Point {
    let g = host
        .group_at_for_tests(strip_id, group_idx)
        .expect("group widget");
    let bar = g.bar_rect();
    let last = g.tab_rects().last().copied().unwrap_or(bar);
    Point::new(
        f64::midpoint(last.x1, bar.x1),
        f64::midpoint(bar.y0, bar.y1),
    )
}

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "st's only use is producing tree via tree(); the read-guard's lifetime \
              is tied to st's borrow, so it can't be dropped before tree's last use"
)]
fn group_drag_reorders_strip_window_groups_in_tree() {
    let (tree, strip_id, _ids) = single_strip_tree(&[100, 200, 300]);
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let state_dir = tempfile::tempdir().expect("state directory");
    let state_path = state_dir.path().join("state.json");
    {
        let st = chrome.borrow_mut();
        let mut tree = st.tree_mut();
        tree.persist_path = Some(state_path.clone());
        tree.seed_doc_from_cache();
    }
    let layout = lay_out_host(&mut host);

    {
        let st = chrome.borrow();
        let tree = st.tree();
        let order: Vec<u64> = tree.desktop.workspaces[0].strips[0]
            .window_groups
            .iter()
            .map(|g| g.id)
            .collect();
        assert_eq!(order, vec![100, 200, 300]);
    }

    // Drag group 100's tab-bar background past group 300's centre.
    let grab = group_grab_point(&mut host, strip_id, 0);
    let g300_center_x = host
        .group_at_for_tests(strip_id, 2)
        .unwrap()
        .get_rect()
        .center()
        .x;
    cursor(&mut host, &layout, grab);
    press(&mut host, &layout);
    cursor(&mut host, &layout, Point::new(g300_center_x + 60.0, grab.y));
    release(&mut host, &layout);

    host.tick_pending_group_reorder_harvest_for_tests();

    let st = chrome.borrow();
    let tree = st.tree();
    let order: Vec<u64> = tree.desktop.workspaces[0].strips[0]
        .window_groups
        .iter()
        .map(|g| g.id)
        .collect();
    assert_eq!(
        order,
        vec![200, 300, 100],
        "the dragged group must move to the dropped slot in the persisted tree; \
         neighbours shift to fill the vacated slot",
    );
    drop(tree);
    drop(st);

    // The one-shot must be consumed: a second frame may not author a
    // duplicate reorder transaction or alter the already-persisted order.
    let heads_after_first = chrome.borrow_mut().tree_mut().current_heads();
    host.tick_pending_group_reorder_harvest_for_tests();
    assert_eq!(
        chrome.borrow_mut().tree_mut().current_heads(),
        heads_after_first,
        "one pointer drag commits exactly one durable reorder"
    );
    let restarted = shell::shell::persist::load_tree_from(&state_path)
        .expect("pointer reorder writes a reconstructable layout");
    assert_eq!(
        restarted.desktop.workspaces[0].strips[0]
            .window_groups
            .iter()
            .map(|group| group.id)
            .collect::<Vec<_>>(),
        vec![200, 300, 100],
        "restart retains the pointer-drag order"
    );
}

/// A complete durable desktop interaction uses three entry points: opening a
/// file from the shell, dragging a window group, and closing the opened tab.
/// The final revision/load checks make sure none of those live-widget steps
/// can be undone by the next reconciliation or a process restart.
#[test]
fn open_reorder_close_then_revision_reconstructs_the_same_layout() {
    let (tree, strip_id, _ids) = single_strip_tree(&[100, 200, 300]);
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let state_dir = tempfile::tempdir().expect("state directory");
    let state_path = state_dir.path().join("state.json");
    {
        let state = chrome.borrow_mut();
        let mut tree = state.tree_mut();
        tree.persist_path = Some(state_path.clone());
        tree.seed_doc_from_cache();
    }

    let opened = chrome
        .borrow_mut()
        .handle_file_drop(std::path::Path::new("/tmp/layout-session.ts"))
        .expect("file drop opens an editor tab");
    chrome.borrow_mut().bump();
    assert!(
        host.take_structure_dirty(),
        "the opened tab reaches the live row"
    );
    let layout = lay_out_host(&mut host);

    // Drag the first group to the end through the pointer path.
    let grab = group_grab_point(&mut host, strip_id, 0);
    let last_center_x = host
        .group_at_for_tests(strip_id, 2)
        .expect("last group")
        .get_rect()
        .center()
        .x;
    cursor(&mut host, &layout, grab);
    press(&mut host, &layout);
    cursor(&mut host, &layout, Point::new(last_center_x + 60.0, grab.y));
    release(&mut host, &layout);
    host.tick_pending_group_reorder_harvest_for_tests();

    assert!(
        host.close_tab_in_group(100, Some(opened)),
        "the shared durable close path removes the file-opened tab"
    );
    let _ = host.take_structure_dirty();
    chrome.borrow_mut().bump();
    let _ = host.take_structure_dirty();

    {
        let state = chrome.borrow();
        let tree = state.tree();
        let strip = &tree.desktop.workspaces[0].strips[0];
        assert_eq!(
            strip
                .window_groups
                .iter()
                .map(|group| group.id)
                .collect::<Vec<_>>(),
            vec![200, 300, 100],
            "pointer reorder remains after the open/close lifecycle"
        );
        assert!(
            strip.window_groups[2]
                .tabs
                .iter()
                .all(|tab| tab.id != opened),
            "the opened tab is absent from durable state before reconstruction"
        );
        drop(tree);
    }

    let restarted = shell::shell::persist::load_tree_from(&state_path)
        .expect("the combined interaction writes a reconstructable layout");
    let strip = &restarted.desktop.workspaces[0].strips[0];
    assert_eq!(
        strip
            .window_groups
            .iter()
            .map(|group| group.id)
            .collect::<Vec<_>>(),
        vec![200, 300, 100]
    );
    assert!(
        strip.window_groups[2]
            .tabs
            .iter()
            .all(|tab| tab.id != opened)
    );
}

/// The magnifier chip in a tab's window decoration must open the search
/// bar *scoped to that app* — the same thing Ctrl+K does.
#[test]
fn tab_search_chip_opens_palette_scoped_to_that_app() {
    use shell::shell::chrome_state::SearchScope;

    let (tree, strip_id, _ids) = single_strip_tree(&[100]);
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);

    assert!(!host.palette_visible(), "palette starts closed");

    // Hover the pill so the per-tab chips get hit rects, then click search.
    let layout = reveal_tab_chips(&mut host, &layout, strip_id);
    let chip = host
        .group_at_for_tests(strip_id, 0)
        .expect("group after layout")
        .bar_for_tests()
        .search_rects()
        .first()
        .copied()
        .filter(|r| r.area() > 0.0)
        .expect("search chip rect once its reveal has run");
    cursor(&mut host, &layout, chip.center());
    press(&mut host, &layout);
    release(&mut host, &layout);

    host.tick_pending_search_open_for_tests();

    assert!(
        host.palette_visible(),
        "the decoration's search chip must open the search bar",
    );
    assert_eq!(
        chrome.borrow().search_scope,
        SearchScope::ActiveApp,
        "and it must be scoped to the app, not global",
    );
}

/// Clicking a tab's Expand chip maximizes it inside the desktop work area:
/// the top bar stays available while the sidebar wheel gets out of the way.
#[test]
fn expand_maximizes_beneath_the_top_bar() {
    let (tree, strip_id, _ids) = single_strip_tree(&[100, 200]);
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);

    assert!(
        !chrome.borrow().is_fullscreen(),
        "nothing holds the output at boot",
    );

    // Route through the event path used by the compositor.
    let layout = reveal_tab_chips(&mut host, &layout, strip_id);
    let chip = {
        let g = host
            .group_at_for_tests(strip_id, 0)
            .expect("group after layout");
        let rects = g.bar_for_tests().max_rects();
        assert!(
            rects.first().is_some_and(|r| r.area() > 0.0),
            "the active tab must expose an Expand chip once its reveal has run"
        );
        rects[0].center()
    };
    cursor(&mut host, &layout, chip);
    press(&mut host, &layout);
    release(&mut host, &layout);

    host.tick_pending_maximize_harvest_for_tests();

    assert!(
        host.group_at_for_tests(strip_id, 0).unwrap().maximized,
        "the Expand click must flip the group's maximized flag",
    );
    assert!(
        chrome.borrow().is_fullscreen(),
        "the promoted group must still suppress the sidebar wheel",
    );

    // The sidebar wheel must now be zero-area so nothing paints over the
    // fullscreen window.
    let layout2 = lay_out_host(&mut host);
    let sidebar = host
        .sidebar_rect_for_tests()
        .expect("sidebar node exists even when suppressed");
    assert_approx_eq!(
        sidebar.area(),
        0.0,
        "the radial workspace switcher must be collapsed to zero area while a \
         window is expanded; got {sidebar:?}",
    );
    let topbar_height = laid_out_topbar_height(chrome);
    assert!(
        topbar_height > 10.0,
        "maximize must preserve the OS-owned top bar; got height {topbar_height}",
    );
    let _ = &layout2;
}

/// A lone window group has no *inter*-group handle, so its only resize
/// affordance is the grip on its own right edge — dragging it must widen
/// the group on screen and in the persisted tree.
#[test]
fn group_right_edge_drag_resizes_on_screen_and_persists() {
    let (tree, strip_id, _ids) = single_strip_tree(&[100]);
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let mut layout = lay_out_host(&mut host);

    let usable = usable_strip_px(&mut host, strip_id);
    let width_in_tree = |chrome: &shell::shell::chrome_state::Chrome| -> f64 {
        let st = chrome.borrow();
        let tree = st.tree();
        tree.desktop.workspaces[0].strips[0].window_groups[0]
            .width
            .resolve(usable)
    };
    let boot_px = WindowWidth::Normal.resolve(usable);
    assert_approx_eq!(
        width_in_tree(&chrome),
        boot_px,
        "Normal is two thirds of the usable strip at boot",
    );

    let rect = host
        .group_at_for_tests(strip_id, 0)
        .expect("group after layout")
        .get_rect();
    let on_screen_before = rect.width();
    // Grab 2 px inside the right-edge grip and drag +120 px.
    let grab = Point::new(rect.x1 - 2.0, rect.center().y);
    cursor(&mut host, &layout, grab);
    press(&mut host, &layout);
    cursor(&mut host, &layout, Point::new(grab.x + 120.0, grab.y));
    release(&mut host, &layout);

    // The width change must report structure-dirty so the tree is rebuilt;
    // re-laying out is what the production frame loop does next.
    layout = lay_out_host(&mut host);
    let on_screen_after = host
        .group_at_for_tests(strip_id, 0)
        .unwrap()
        .get_rect()
        .width();
    assert!(
        on_screen_after > on_screen_before,
        "the group must actually get wider on screen: {on_screen_before} -> {on_screen_after}",
    );
    assert_approx_eq!(
        on_screen_after,
        on_screen_before + 120.0,
        "and by exactly the dragged distance",
    );

    host.tick_pending_width_harvest_for_tests();
    assert_approx_eq!(
        width_in_tree(&chrome),
        boot_px + 120.0,
        "the new width must persist into the tree so it survives a restart",
    );
    let _ = &layout;
}

/// A group focused in the same frame it appeared must still pan into
/// view — `adopt_toplevel` creates and focuses a group before it has a
/// rect.
#[test]
fn a_group_focused_before_it_has_a_rect_is_still_scrolled_into_view() {
    // One full-width group fills the strip, so a second starts past the
    // right edge — the geometry a newly launched app lands in.
    let (mut tree, strip_id, _ids) = single_strip_tree(&[100]);
    tree.desktop.workspaces[0].strips[0].window_groups[0].width = WindowWidth::Wide;
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);
    let _ = &layout;

    // Splice in a second group and focus it the way adoption does, before
    // any layout pass has given it a rect.
    let new_group = {
        let st = chrome.borrow();
        let mut t = st.tree_mut();
        let id = 9_100_u64;
        let tab_id = 9_101_u64;
        t.desktop.workspaces[0].strips[0]
            .window_groups
            .push(shell::shell::state::WindowGroup {
                id,
                tabs: vec![Tab {
                    id: tab_id,
                    title: "Launched".into(),
                    tile: TileContent::Stub {
                        kind: AppKind::Other("Launched".into()),
                        label: "Launched".into(),
                    },
                }],
                active_tab: tab_id,
                width: WindowWidth::Normal,
                maximized: false,
            });
        t.desktop.set_active_group(strip_id, id);
        id
    };
    chrome.borrow_mut().bump();

    // Frame one, in production order: resync splices the group into the live
    // row, then the scroll reconcile runs — still with no rect.
    let _ = Widget::take_structure_dirty(&mut host);
    host.tick_scroll_active_into_view_for_tests();
    let _layout = lay_out_host(&mut host);
    // Frame two: the group now has a rect, so the pan must happen.
    host.tick_scroll_active_into_view_for_tests();
    // The pan is eased on the snap curve, not teleported — advance the
    // per-frame tick past the curve's duration so the motion settles before
    // the position is read.
    host.group_row_mut_for_tests(strip_id)
        .expect("strip GroupRow")
        .tick_snap_at(std::time::Instant::now() + std::time::Duration::from_millis(400));
    let scrolled = host
        .group_row_mut_for_tests(strip_id)
        .expect("strip GroupRow")
        .scroll_x();
    assert!(
        scrolled > 0.0,
        "the strip must pan toward the newly focused group; scroll_x is {scrolled}",
    );

    let _layout = lay_out_host(&mut host);
    let row_rect = host.group_row_rect_for_tests(strip_id).unwrap();
    let idx = host
        .group_row_mut_for_tests(strip_id)
        .unwrap()
        .group_ids()
        .iter()
        .position(|&g| g == new_group)
        .expect("the new group is in the row");
    let rect = host.group_at_for_tests(strip_id, idx).unwrap().get_rect();
    assert!(
        rect.x0 < row_rect.x1,
        "the newly focused group must be on the output, not past its right \
         edge (group x0 {} vs row x1 {})",
        rect.x0,
        row_rect.x1,
    );
}

/// Dragging a tab pill to a new slot inside its own group must reorder
/// the persisted `tabs` list — the whole wire from `TabBar` release
/// through `pending_tab_reorder` to
/// `WorkspaceTree::reorder_tab_within_group`.
#[test]
fn tab_drag_reorders_tabs_within_group_in_tree() {
    // Three tabs so there are gaps to drop into.
    let (mut tree, strip_id, _ids) = single_strip_tree(&[100]);
    {
        let g = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
        g.tabs = vec![
            Tab {
                id: 1,
                title: "A".into(),
                tile: TileContent::Stub {
                    kind: AppKind::Other("T".into()),
                    label: "A".into(),
                },
            },
            Tab {
                id: 2,
                title: "B".into(),
                tile: TileContent::Stub {
                    kind: AppKind::Other("T".into()),
                    label: "B".into(),
                },
            },
            Tab {
                id: 3,
                title: "C".into(),
                tile: TileContent::Stub {
                    kind: AppKind::Other("T".into()),
                    label: "C".into(),
                },
            },
        ];
        g.active_tab = 1;
    }
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);

    let tab_order = |chrome: &shell::shell::chrome_state::Chrome| -> Vec<u64> {
        let st = chrome.borrow();
        let tree = st.tree();
        tree.desktop.workspaces[0].strips[0].window_groups[0]
            .tabs
            .iter()
            .map(|t| t.id)
            .collect()
    };
    assert_eq!(tab_order(&chrome), vec![1, 2, 3], "persisted order at boot");

    // Drop pill A past pill C's midpoint → [B, C, A].
    let (grab, target) = {
        let g = host
            .group_at_for_tests(strip_id, 0)
            .expect("group after layout");
        let rects = g.tab_rects();
        assert!(rects.len() >= 3, "three pills must be laid out");
        // Grab the pill's label area, NOT its centre: the per-tab close /
        // maximize / search chips sit there and consume the press before a drag
        // can start.
        (
            Point::new(rects[0].x0 + 8.0, rects[0].center().y),
            Point::new(rects[2].center().x + 10.0, rects[2].center().y),
        )
    };
    cursor(&mut host, &layout, grab);
    press(&mut host, &layout);
    cursor(&mut host, &layout, Point::new(grab.x + 20.0, grab.y));
    cursor(&mut host, &layout, target);
    release(&mut host, &layout);

    // The widget's own pill order must move too — the persisted write is a
    // mirror of it, not an independent path.
    {
        let g = host.group_at_for_tests(strip_id, 0).unwrap();
        let widget_order: Vec<u64> = g.tabs().iter().map(|t| t.id).collect();
        assert_eq!(widget_order, vec![2, 3, 1], "pill order after the drop");
    }

    host.tick_pending_tab_reorder_harvest_for_tests();

    assert_eq!(
        tab_order(&chrome),
        vec![2, 3, 1],
        "the dragged tab must move to the dropped slot in the persisted tree",
    );
}

// 3. Horizontal strip scroll

#[test]
fn switching_workspace_swaps_the_rendered_strips() {
    // Two workspaces, A (boot) and B. The rendered strip must follow
    // `tree.active`, so every window gesture acts on the workspace the user
    // is looking at rather than the boot workspace forever.
    let (mut tree, strip_a, _) = single_strip_tree(&[100, 200]);
    let ws_b_id = WorkspaceId::new();
    let strip_b = StripId::new();
    let mk_tab = |tid: u64, label: &str| Tab {
        id: tid,
        title: label.into(),
        tile: TileContent::Stub {
            kind: AppKind::Other(label.into()),
            label: label.into(),
        },
    };
    let group_300 = WindowGroup {
        id: 300,
        tabs: vec![mk_tab(3001, "B-first"), mk_tab(3002, "B-second")],
        active_tab: 3001,
        width: WindowWidth::Normal,
        maximized: false,
    };
    let group_400 = WindowGroup {
        id: 400,
        tabs: vec![mk_tab(4001, "B-third")],
        active_tab: 4001,
        width: WindowWidth::Normal,
        maximized: false,
    };
    let ws_b = Workspace {
        strips: vec![Strip {
            id: strip_b,
            name: "Main".into(),
            window_groups: vec![group_300, group_400],
        }],
        ..Workspace::new(ws_b_id, "Second")
    };
    tree.desktop.workspaces.push(ws_b);
    tree.desktop.active_strip.insert(ws_b_id, strip_b);

    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let _layout = lay_out_host(&mut host);
    assert!(host.group_row_mut_for_tests(strip_a).is_some());
    assert!(
        host.group_row_mut_for_tests(strip_b).is_none(),
        "precondition: only the active workspace's strips are rendered"
    );

    // Switch the way every switch path does (tree mutation + chrome bump),
    // then run the per-frame reconcile.
    {
        let st = chrome.borrow();
        st.tree_mut().set_active(ws_b_id);
    }
    chrome.borrow_mut().bump();
    assert!(
        host.tick_workspace_content_for_tests(),
        "an active-workspace change must rebuild the content"
    );
    let _layout = lay_out_host(&mut host);

    let row_b = host
        .group_row_mut_for_tests(strip_b)
        .expect("workspace B's strip must now be the rendered one");
    assert_eq!(row_b.group_ids(), vec![300, 400]);
    assert!(
        host.group_row_mut_for_tests(strip_a).is_none(),
        "the old workspace's rows must be gone"
    );
    assert!(
        host.focus_window_widget(strip_b, 300, 3002),
        "focus_window_widget must reach workspace B's rendered rows"
    );
    // And switching back re-renders A without leaking B's rows.
    {
        let st = chrome.borrow();
        let home = st.tree().desktop.workspaces[0].id;
        st.tree_mut().set_active(home);
    }
    chrome.borrow_mut().bump();
    assert!(host.tick_workspace_content_for_tests());
    let _layout = lay_out_host(&mut host);
    assert!(host.group_row_mut_for_tests(strip_a).is_some());
    assert!(host.group_row_mut_for_tests(strip_b).is_none());
}

#[test]
fn wheel_over_strip_does_not_move_it() {
    // Hard boundary: a bare wheel never moves the strip — scrolling is for
    // content inside a window. The view travels only through focus changes
    // and the Alt-modified wheel gestures, which route through
    // `client_input`, not this widget path.
    let (tree, strip_id, _ids) = single_strip_tree(&[100, 200, 300, 400]);
    let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);

    let row_rect = host
        .group_row_rect_for_tests(strip_id)
        .expect("row rect after layout");
    let before = host.group_row_mut_for_tests(strip_id).unwrap().scroll_x();

    cursor(
        &mut host,
        &layout,
        Point::new(
            f64::midpoint(row_rect.x0, row_rect.x1),
            f64::midpoint(row_rect.y0, row_rect.y1),
        ),
    );
    dispatch(
        &mut host,
        &layout,
        &WindowEvent::MouseWheel {
            device_id: unsafe { std::mem::zeroed() },
            delta: MouseScrollDelta::LineDelta(0.0, -3.0),
            phase: winit::event::TouchPhase::Moved,
        },
    );
    // Give any wrongly-started eased travel every chance to show itself
    // before reading the position.
    host.group_row_mut_for_tests(strip_id)
        .unwrap()
        .tick_snap_at(std::time::Instant::now() + std::time::Duration::from_millis(400));

    let after = host.group_row_mut_for_tests(strip_id).unwrap().scroll_x();
    assert_approx_eq!(
        after,
        before,
        "a bare wheel over the strip must not move it",
    );
}

#[test]
fn streamed_three_finger_pan_moves_the_live_strip_and_settles_overpull() {
    let (tree, strip_id, _ids) = single_strip_tree(&[100, 200, 300, 400]);
    let active_group = tree.desktop.active_group_for(strip_id);
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let _layout = lay_out_host(&mut host);

    assert!(host.update_active_strip_pan(320.0, true));
    assert_approx_eq!(
        host.group_row_mut_for_tests(strip_id).unwrap().scroll_x(),
        320.0,
        "the live row must track total three-finger travel 1:1",
    );
    assert_eq!(
        chrome.borrow().tree().desktop.active_group_for(strip_id),
        active_group,
        "panning the canvas must not silently change focused app",
    );

    let legal_end = host
        .group_row_mut_for_tests(strip_id)
        .unwrap()
        .pan_max_scroll_x();
    assert!(host.update_active_strip_pan(legal_end + 500.0, false));
    let held = host.group_row_mut_for_tests(strip_id).unwrap().scroll_x();
    assert!(
        held > legal_end && held < legal_end + 500.0,
        "overpull must keep moving with resistance: legal={legal_end}, held={held}"
    );

    let released = std::time::Instant::now();
    assert!(host.end_active_strip_pan(released));
    host.group_row_mut_for_tests(strip_id)
        .unwrap()
        .tick_snap_at(released + std::time::Duration::from_millis(220));
    assert_approx_eq!(
        host.group_row_mut_for_tests(strip_id).unwrap().scroll_x(),
        legal_end,
        "release must settle the live strip to its legal trailing rest",
    );
}

#[test]
fn focusing_offscreen_group_scrolls_it_into_view() {
    // Four Normal (600 px) groups overflow the 1280 px viewport, so the last
    // starts off the right edge at scroll_x = 0.
    let (tree, strip_id, ids) = single_strip_tree(&[100, 200, 300, 400]);
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let _layout = lay_out_host(&mut host);

    let last_id = ids[3];
    let row_rect = host.group_row_rect_for_tests(strip_id).unwrap();

    let before = host.group_row_mut_for_tests(strip_id).unwrap().scroll_x();
    assert_approx_eq!(before, 0.0, "strip starts scrolled home");
    let last_rect_before = host.group_at_for_tests(strip_id, 3).unwrap().get_rect();
    assert!(
        last_rect_before.x1 > row_rect.x1,
        "precondition: the last group must hang off the strip's right edge \
         (group x1 {} vs row x1 {})",
        last_rect_before.x1,
        row_rect.x1,
    );

    // Focus the off-screen group, then run the focus-follow reconcile.
    {
        let st = chrome.borrow();
        let mut tree = st.tree_mut();
        tree.desktop.set_active_group(strip_id, last_id);
    }
    host.tick_scroll_active_into_view_for_tests();
    // The pan is eased on the snap curve, not teleported — advance the
    // per-frame tick past the curve's duration so the motion settles before
    // the position is read.
    host.group_row_mut_for_tests(strip_id)
        .unwrap()
        .tick_snap_at(std::time::Instant::now() + std::time::Duration::from_millis(400));

    let after = host.group_row_mut_for_tests(strip_id).unwrap().scroll_x();
    assert!(
        after > before,
        "focusing an off-screen group must pan the strip toward it \
         (scroll_x was {before}, now {after})",
    );

    // Re-layout so the rects reflect the new scroll offset.
    let _layout = lay_out_host(&mut host);
    let row_rect = host.group_row_rect_for_tests(strip_id).unwrap();
    let last_rect_after = host.group_at_for_tests(strip_id, 3).unwrap().get_rect();
    assert!(
        last_rect_after.x1 <= row_rect.x1 + 1.0,
        "after scroll-into-view the focused group's right edge must be inside \
         the strip (group x1 {} vs row x1 {})",
        last_rect_after.x1,
        row_rect.x1,
    );
}

// 4. Drag-file-to-open

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "st's only use is producing tree via tree(); the read-guard's lifetime \
              is tied to st's borrow, so it can't be dropped before tree's last use"
)]
fn dropping_a_code_file_opens_the_editor_in_the_active_workspace() {
    let (tree, _strip_id, _ids) = single_strip_tree(&[100]);
    let (_host, chrome) = build_tilo_overlay_host_with_chrome(&tree);

    let tabs_before = {
        let st = chrome.borrow();
        let tree = st.tree();
        tree.desktop.workspaces[0].strips[0].window_groups[0]
            .tabs
            .len()
    };

    // Drop a `.ts` file through the `WindowEvent::DroppedFile` entry.
    let tab_id = chrome
        .borrow_mut()
        .handle_file_drop(std::path::Path::new("/some/where/component.ts"))
        .expect("a file drop must resolve to a launch intent");

    let st = chrome.borrow();
    let tree = st.tree();
    let group = &tree.desktop.workspaces[0].strips[0].window_groups[0];
    assert_eq!(
        group.tabs.len(),
        tabs_before + 1,
        "the drop must splice one new tab into the active workspace",
    );
    let new_tab = group.tabs.iter().find(|t| t.id == tab_id).expect("new tab");
    assert_eq!(new_tab.title, "component.ts");
    match &new_tab.tile {
        TileContent::AppPlaceholder { kind, .. } => assert_eq!(
            kind,
            &AppKind::Other("Editor".into()),
            "a .ts file's MIME must resolve to the Editor app",
        ),
        other => panic!("expected an app-launch placeholder tile, got {other:?}"),
    }
}

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "st's only use is producing tree via tree(); the read-guard's lifetime \
              is tied to st's borrow, so it can't be dropped before tree's last use"
)]
fn dropping_a_folder_opens_files_app() {
    // An extensionless path that isn't a real dir classifies as a plain file
    // → Files, as does a real folder: a non-code drop never resolves to the
    // Editor.
    let (tree, _strip_id, _ids) = single_strip_tree(&[100]);
    let (_host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let tmp = std::env::temp_dir().join("tilo-deskenv-drop-folder");
    let _ = std::fs::create_dir_all(&tmp);

    let tab_id = chrome
        .borrow_mut()
        .handle_file_drop(&tmp)
        .expect("folder drop resolves");

    let st = chrome.borrow();
    let tree = st.tree();
    let group = &tree.desktop.workspaces[0].strips[0].window_groups[0];
    let new_tab = group.tabs.iter().find(|t| t.id == tab_id).unwrap();
    match &new_tab.tile {
        TileContent::AppPlaceholder { kind, .. } => assert_eq!(
            kind,
            &AppKind::FileExplorer,
            "a folder drop must open the Files app",
        ),
        other => panic!("expected an app-launch placeholder tile, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&tmp);
}
