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
use shell::shell::tab_binding::Placement;
use shell::shell::topbar_chrome::{TOPBAR_HEIGHT_PX, TopBarChrome};
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use ui::synthetic_input::PointerAction;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::event::{Modifiers, MouseButton, WindowEvent};

use crate::support::host;

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
            focus_history: Vec::new(),
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
    child_node: NodeId,
    theme: Theme,
    input_scale: f64,
}

/// Usable strip width at the fixture's viewport (window minus the 32 px
/// leading and trailing edge gutters). Group widths are fractions of this, so a
/// pixel expectation only means anything alongside it; read off the live
/// row so the test can't drift from what the shell reserves.
fn usable_strip_px(host: &mut ShellOverlayHost, strip_id: StripId) -> f64 {
    host.group_row_mut_for_tests(strip_id)
        .expect("strip GroupRow")
        .usable_at(0.0)
}

fn lay_out_host(host: &mut ShellOverlayHost) -> Layout {
    lay_out_host_with_theme(host, Theme::dark(), 1.0)
}

fn lay_out_host_with_theme(host: &mut ShellOverlayHost, theme: Theme, input_scale: f64) -> Layout {
    // What the compositor does before every build: name the display, so
    // window-group width fractions resolve against it rather than the
    // toolkit's fallback.
    host.set_output_geometry(&[shell::shell::overlay_host::OutputGeometry::unscaled(
        0.0,
        0.0,
        f64::from(W),
        f64::from(H),
    )]);
    let host::Layout { taffy, child_node } = host::lay_out_with_theme(host, W, H, &theme);
    Layout {
        taffy,
        child_node,
        theme,
        input_scale,
    }
}

fn laid_out_topbar_height(chrome: Chrome) -> f64 {
    let tree = chrome.borrow().tree().clone();
    let mut bar = TopBarChrome::from_tree(&tree.desktop)
        .with_chrome(shell::shell::topbar_chrome::native_source(chrome));
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

fn dispatch_with_modifiers(
    host: &mut ShellOverlayHost,
    layout: &Layout,
    event: &WindowEvent,
    modifiers: Modifiers,
) {
    ui::testing::Dispatch::new()
        .scale_factor(layout.input_scale)
        .modifiers(modifiers)
        .layout(&layout.taffy, layout.child_node)
        .send(host, event);
}

fn dispatch(host: &mut ShellOverlayHost, layout: &Layout, event: &WindowEvent) {
    dispatch_with_modifiers(host, layout, event, Modifiers::default());
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
    // A pill opens once the pointer has rested on it for the hover dwell.
    let dwelt = host
        .group_at_for_tests(strip_id, 0)
        .expect("hovered group")
        .bar_mut_for_tests()
        .elapse_hover_dwell();
    assert!(dwelt, "resting on the pill opened nothing to reveal");
    // The curve is created by the layout pass that first sees the new hover
    // target, so it has to exist before it can be held.
    let _layout = lay_out_host_with_theme(host, layout.theme.clone(), layout.input_scale);
    // The next layout starts the reveal curve. Hold that curve to its end
    // before placing the chip rects.
    let layout = lay_out_host_with_theme(host, layout.theme.clone(), layout.input_scale);
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
    lay_out_host_with_theme(host, layout.theme.clone(), layout.input_scale)
}

fn cursor(host: &mut ShellOverlayHost, layout: &Layout, p: Point) {
    dispatch(
        host,
        layout,
        &PointerAction::MoveTo(p).event(layout.input_scale),
    );
}

fn press(host: &mut ShellOverlayHost, layout: &Layout) {
    dispatch(
        host,
        layout,
        &PointerAction::Press(MouseButton::Left).event(layout.input_scale),
    );
}

fn release(host: &mut ShellOverlayHost, layout: &Layout) {
    dispatch(
        host,
        layout,
        &PointerAction::Release(MouseButton::Left).event(layout.input_scale),
    );
}

/// Paint explicitly before inspecting targets; inspection itself never prepares them.
fn paint_group(host: &mut ShellOverlayHost, layout: &Layout, strip_id: StripId) {
    host.group_at_for_tests(strip_id, 0)
        .expect("group to paint")
        .draw(
            &mut vello::Scene::new(),
            vello::kurbo::Affine::IDENTITY,
            &mut ui::text_render::TextRenderer::new(),
            &layout.theme,
            &ui::presence::PresenceCtx::default(),
        );
}

fn press_with_modifiers(host: &mut ShellOverlayHost, layout: &Layout, modifiers: Modifiers) {
    dispatch_with_modifiers(host, layout, &ui::synthetic_input::mouse_press(), modifiers);
}

#[test]
fn painted_window_controls_survive_cancel_and_scale_changes() {
    for theme in [Theme::dark(), Theme::light().with_text_scale(2.0)] {
        let (tree, strip_id, _) = single_strip_tree(&[1]);
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let layout = lay_out_host_with_theme(&mut host, theme.clone(), 1.5);
        let layout = reveal_tab_chips(&mut host, &layout, strip_id);
        let maximize = host
            .active_window_bounds()
            .unwrap()
            .maximize
            .expect("revealed Expand");
        cursor(&mut host, &layout, maximize.center());
        press(&mut host, &layout);
        release(&mut host, &layout);
        host.tick_pending_maximize_harvest_for_tests();
        let layout = lay_out_host_with_theme(&mut host, theme.clone(), 1.5);
        let unpainted = host.active_window_bounds().expect("active group");
        assert!(
            unpainted.restore.is_none(),
            "inspection must not paint or reveal"
        );
        assert_eq!(host.active_window_bounds(), Some(unpainted));
        cursor(
            &mut host,
            &layout,
            Point::new(unpainted.group.center().x, unpainted.group.y0 + 1.0),
        );
        // A rest in the reveal band shows the bar once the dwell runs out.
        assert!(
            host.group_at_for_tests(strip_id, 0)
                .unwrap()
                .elapse_maximized_chrome_intent(),
            "a rest in the band reveals the bar"
        );
        paint_group(&mut host, &layout, strip_id);
        let bounds = host.active_window_bounds().expect("painted active group");
        let restore = bounds.restore.expect("painted Restore");
        let client = bounds.client.expect("active client");
        assert!(
            client.contains(restore.center()),
            "the bar stands over the client's top edge"
        );
        assert_eq!(host.active_window_control_rects().unwrap().1, restore);
        cursor(&mut host, &layout, restore.center());
        press(&mut host, &layout);
        assert!(host.group_at_for_tests(strip_id, 0).unwrap().maximized);
        dispatch(
            &mut host,
            &layout,
            &PointerAction::Leave.event(layout.input_scale),
        );
        release(&mut host, &layout);
        host.tick_pending_maximize_harvest_for_tests();
        assert!(host.group_at_for_tests(strip_id, 0).unwrap().maximized);

        // The pointer left the output, which hid the bar: reach for it again.
        host.group_at_for_tests(strip_id, 0)
            .unwrap()
            .set_maximized_chrome_shown_for_tests(true);
        paint_group(&mut host, &layout, strip_id);
        cursor(&mut host, &layout, restore.center());
        paint_group(&mut host, &layout, strip_id);
        press(&mut host, &layout);
        let scaled = lay_out_host_with_theme(&mut host, theme.with_text_scale(1.3), 2.0);
        paint_group(&mut host, &scaled, strip_id);
        let target = host
            .active_window_bounds()
            .unwrap()
            .restore
            .expect("new painted Restore");
        cursor(&mut host, &scaled, target.center());
        release(&mut host, &scaled);
        host.tick_pending_maximize_harvest_for_tests();
        assert!(
            host.group_at_for_tests(strip_id, 0).unwrap().maximized,
            "a scale change must cancel the old held gesture"
        );

        host.group_at_for_tests(strip_id, 0)
            .unwrap()
            .set_maximized_chrome_shown_for_tests(true);
        paint_group(&mut host, &scaled, strip_id);
        cursor(&mut host, &scaled, target.center());
        press(&mut host, &scaled);
        assert!(host.group_at_for_tests(strip_id, 0).unwrap().maximized);
        release(&mut host, &scaled);
        host.tick_pending_maximize_harvest_for_tests();
        assert!(!host.group_at_for_tests(strip_id, 0).unwrap().maximized);
        assert!(!chrome.borrow().is_fullscreen());
    }
}

// 1. Horizontal window-group resize

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "st's only use is producing tree via tree(); the read-guard's lifetime \
is tied to st's borrow, so it can't be dropped before tree's last use"
)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn resize_handles_persist_clamped_redistributed_group_widths() {
    // Phase `resize_handle_drag_persists_new_group_widths_into_tree`.
    {
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

        // Drag the handle +40 px right: the left group grows into the infinite
        // strip; the neighbour keeps its own width.
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
            start_px,
            "ordinary resize must not squeeze the adjacent group",
        );
        // What is *stored* is the share, not the pixels — the same tree on a
        // wider display gives the same proportions, not the same widths.
        assert!(
            matches!(left.width, WindowWidth::Fraction(_)),
            "a resize persists as a fraction of the strip; got {:?}",
            left.width,
        );
    }

    // Phase `shift_resize_handle_redistributes_width_between_neighbours`.
    {
        use winit::keyboard::ModifiersState;

        let (tree, strip_id, ids) = single_strip_tree(&[100, 200]);
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let layout = lay_out_host(&mut host);
        let usable = usable_strip_px(&mut host, strip_id);
        let start_px = WindowWidth::Normal.resolve(usable);
        let handle_rect = host
            .group_row_mut_for_tests(strip_id)
            .expect("strip GroupRow")
            .handle_at(0)
            .expect("inter-group handle")
            .get_rect();
        let centre = handle_rect.center();

        cursor(&mut host, &layout, centre);
        press_with_modifiers(&mut host, &layout, Modifiers::from(ModifiersState::SHIFT));
        cursor(&mut host, &layout, Point::new(centre.x + 40.0, centre.y));
        release(&mut host, &layout);
        host.tick_pending_width_harvest_for_tests();

        let (left_px, right_px) = {
            let st = chrome.borrow();
            let groups = &st.tree().desktop.workspaces[0].strips[0].window_groups;
            let width_of = |id| {
                groups
                    .iter()
                    .find(|g| g.id == id)
                    .unwrap()
                    .width
                    .resolve(usable)
            };
            (width_of(ids[0]), width_of(ids[1]))
        };
        assert_approx_eq!(left_px, start_px + 40.0);
        assert_approx_eq!(right_px, start_px - 40.0);
        assert_approx_eq!(
            left_px + right_px,
            start_px * 2.0,
            "Shift-resize deliberately preserves the adjacent pair's total width",
        );
    }

    // Phase `a_drag_far_past_the_display_has_no_ceiling`.
    // A normal drag hundreds of displays wide widens the resized group that
    // far, without changing its neighbour, and the width persists: there is
    // no widest window anyone can feel.
    {
        let (mut tree, strip_id, ids) = single_strip_tree(&[100, 200]);
        for g in &mut tree.desktop.workspaces[0].strips[0].window_groups {
            g.width = WindowWidth::Narrow;
        }
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let layout = lay_out_host(&mut host);

        let usable = usable_strip_px(&mut host, strip_id);
        let start = WindowWidth::Narrow.resolve(usable);
        let handle_rect = host
            .group_row_mut_for_tests(strip_id)
            .expect("strip GroupRow")
            .handle_at(0)
            .expect("one seam for a 2-group strip")
            .get_rect();
        let cx = f64::midpoint(handle_rect.x0, handle_rect.x1);
        let cy = f64::midpoint(handle_rect.y0, handle_rect.y1);

        cursor(&mut host, &layout, Point::new(cx, cy));
        press(&mut host, &layout);
        cursor(&mut host, &layout, Point::new(cx + 200_000.0, cy));
        release(&mut host, &layout);

        host.tick_pending_width_harvest_for_tests();

        let st = chrome.borrow();
        let tree = st.tree();
        let groups = &tree.desktop.workspaces[0].strips[0].window_groups;
        let left = groups.iter().find(|g| g.id == ids[0]).unwrap();
        let right = groups.iter().find(|g| g.id == ids[1]).unwrap();
        assert_approx_eq!(
            right.width.resolve(usable),
            start,
            "ordinary resizing must not squeeze the right neighbour",
        );
        assert!(
            (left.width.resolve(usable) - (start + 200_000.0)).abs() <= 1.0,
            "the window follows the drag all the way: {} for {start} + 200000",
            left.width.resolve(usable),
        );
    }

    // Phase `group_right_edge_drag_resizes_on_screen_and_persists`.
    // A lone window group has no *inter*-group handle, so its only resize
    // affordance is the grip on its own right edge — dragging it must widen
    // the group on screen and in the persisted tree.
    {
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
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn group_and_tab_drags_reorder_the_tree_and_reconstruct_after_close() {
    // Phase `group_drag_reorders_strip_window_groups_in_tree`.
    {
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

    // Phase `tab_drag_reorders_tabs_within_group_in_tree`.
    // Dragging a tab pill to a new slot inside its own group must reorder
    // the persisted `tabs` list — the whole wire from `TabBar` release
    // through `pending_tab_reorder` to
    // the desktop controller's committed tab reorder.
    {
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

    // Phase `open_reorder_close_then_revision_reconstructs_the_same_layout`.
    // A complete durable desktop interaction uses three entry points: opening a
    // file from the shell, dragging a window group, and closing the opened tab.
    // The final revision/load checks make sure none of those live-widget steps
    // can be undone by the next reconciliation or a process restart.
    {
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
            .handle_file_drop_at_placement(
                std::path::Path::new("/tmp/layout-session.ts"),
                Placement::TabIn(100),
            )
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
}

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "st's only use is producing tree via tree(); the read-guard's lifetime \
is tied to st's borrow, so it can't be dropped before tree's last use"
)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn drops_and_the_tab_search_chip_open_the_right_app() {
    // Phase `dropping_a_code_file_opens_the_editor_in_the_active_workspace`.
    {
        let (tree, _strip_id, _ids) = single_strip_tree(&[100]);
        let (_host, chrome) = build_tilo_overlay_host_with_chrome(&tree);

        let tabs_before = {
            let st = chrome.borrow();
            let tree = st.tree();
            tree.desktop.workspaces[0].strips[0].window_groups[0]
                .tabs
                .len()
        };

        // Drop a `.ts` file onto the group, as a `WindowEvent::DroppedFile` does.
        let tab_id = chrome
            .borrow_mut()
            .handle_file_drop_at_placement(
                std::path::Path::new("/some/where/component.ts"),
                Placement::TabIn(100),
            )
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

    // Phase `dropping_a_folder_opens_files_app`.
    {
        // An extensionless path that isn't a real dir classifies as a plain file
        // → Files, as does a real folder: a non-code drop never resolves to the
        // Editor.
        let (tree, _strip_id, _ids) = single_strip_tree(&[100]);
        let (_host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let tmp = std::env::temp_dir().join("tilo-deskenv-drop-folder");
        let _ = std::fs::create_dir_all(&tmp);

        let tab_id = chrome
            .borrow_mut()
            .handle_file_drop_at_placement(&tmp, Placement::TabIn(100))
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

    // Phase `tab_search_chip_opens_palette_scoped_to_that_app`.
    // The window bar's search control, right after the window's place, must
    // open the search bar *scoped to that app* — the same thing Ctrl+K does —
    // when the app has nothing to search in place.
    {
        use shell::shell::chrome_state::SearchScope;

        let (tree, strip_id, _ids) = single_strip_tree(&[100]);
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let layout = lay_out_host(&mut host);

        assert!(!host.palette_visible(), "palette starts closed");

        let group = host
            .group_at_for_tests(strip_id, 0)
            .expect("group after layout");
        assert!(
            group
                .bar_for_tests()
                .search_rects()
                .iter()
                .all(|r| r.area() <= 0.0),
            "the window carries one search control, not one per tab"
        );
        let chip = Some(group.window_search().anchor())
            .filter(|r| r.area() > 0.0)
            .expect("the window's search control is placed after its title");
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
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn expand_restore_and_workspace_switch_change_what_is_rendered() {
    // Phase `expand_maximizes_into_the_work_area`.
    // Clicking a tab's Expand chip maximizes it inside the desktop work area:
    // the top bar stays, and the group spans the full width.
    {
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
            "the promoted group holds the work area",
        );

        let layout2 = lay_out_host(&mut host);
        let topbar_height = laid_out_topbar_height(chrome.clone());
        assert_approx_eq!(
            topbar_height,
            f64::from(TOPBAR_HEIGHT_PX),
            "maximize must preserve the full OS-owned top bar",
        );
        let group = host
            .group_at_for_tests(strip_id, 0)
            .expect("maximized group after layout");
        let maximized = group.get_rect();
        assert_approx_eq!(
            maximized.y0,
            topbar_height,
            "maximized content must directly abut the OS top bar; topbar_bottom={topbar_height}, \
             group={maximized:?}, blank_gap={}",
            maximized.y0 - topbar_height,
        );
        assert_approx_eq!(
            maximized.x0,
            0.0,
            "an expanded group owns the left edge: no leading edge gutter is left"
        );
        assert_approx_eq!(
            maximized.x1,
            f64::from(W),
            "maximized group must reach the right edge"
        );
        assert_approx_eq!(
            maximized.y1,
            f64::from(H),
            "maximize must reach the work area's bottom edge; got {maximized:?}",
        );
        let app = group
            .active_host_rect_for_tests()
            .expect("maximized group's active app");
        // The client fills the whole window: its bar hides at rest and
        // appears over the top edge when reached for, taking no room.
        assert_eq!(
            app, maximized,
            "the maximized client must fill the window: no lane above it",
        );
        assert_approx_eq!(
            group.bar_for_tests().get_rect().height(),
            0.0,
            "the hidden tab strip must reserve no height",
        );

        // At rest, with the pointer in the app, no bar is painted.
        cursor(&mut host, &layout2, maximized.center());
        paint_group(&mut host, &layout2, strip_id);
        assert!(
            host.active_window_bounds()
                .expect("painted active group")
                .restore
                .is_none(),
            "the bar hides while the pointer is in the app",
        );
        // A rest in the reveal band at the window's top, through the host's
        // pointer route at the group's actual, nonzero origin, shows it once
        // the dwell runs out. Painting must produce a usable Restore target.
        cursor(
            &mut host,
            &layout2,
            Point::new(maximized.center().x, maximized.y0 + 1.0),
        );
        cursor(
            &mut host,
            &layout2,
            Point::new(maximized.center().x + 1.0, maximized.y0 + 2.0),
        );
        assert!(
            host.group_at_for_tests(strip_id, 0)
                .expect("maximized group")
                .elapse_maximized_chrome_intent(),
            "a rest in the band reveals the bar",
        );
        paint_group(&mut host, &layout2, strip_id);
        let bounds = host.active_window_bounds().expect("painted active group");
        let restore = bounds.restore.expect("painted Restore target");
        assert_eq!(bounds.client, Some(app));
        assert!(
            restore.area() > 0.0,
            "Restore must reveal below the OS top bar"
        );
        assert!(
            restore.y0 >= maximized.y0 + ui::widget::top_edge_reveal::REVEAL_BAND_PX,
            "Restore stands below the reveal band, never under the resting pointer",
        );
        assert!(
            restore.x0 >= maximized.x0 && restore.x1 <= maximized.x1,
            "Restore must stay inside the maximized group",
        );
        cursor(&mut host, &layout2, restore.center());
        press(&mut host, &layout2);
        release(&mut host, &layout2);
        host.tick_pending_maximize_harvest_for_tests();
        assert!(
            !chrome.borrow().is_fullscreen(),
            "the painted Restore target must work"
        );
        assert!(!host.group_at_for_tests(strip_id, 0).unwrap().maximized);
    }

    // Phase `the_leading_gutter_is_dropped_while_expanded_and_returns_on_restore`.
    // An expanded group owns the left edge; restoring it lays the group out
    // behind the leading edge gutter again on the first frame.
    {
        let (tree, strip_id, _ids) = single_strip_tree(&[100]);
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let restored_layout = lay_out_host(&mut host);

        // Expand through the same one-shot toggle the Expand chip publishes,
        // dispatching once so `WindowGroup` drains it into the harvest path.
        host.group_at_for_tests(strip_id, 0)
            .expect("group after layout")
            .publish_maximize_intent_for_tests();
        cursor(
            &mut host,
            &restored_layout,
            Point::new(f64::from(W) - 1.0, f64::from(H) - 1.0),
        );
        host.tick_pending_maximize_harvest_for_tests();
        let maximized_layout = lay_out_host(&mut host);
        assert!(chrome.borrow().is_fullscreen());
        assert_approx_eq!(
            host.group_at_for_tests(strip_id, 0)
                .expect("maximized group")
                .get_rect()
                .x0,
            0.0,
            "an expanded group reaches the left edge",
        );

        // Queue the same one-shot toggle the Restore button publishes, then
        // dispatch once so `WindowGroup` drains it into the shell harvest path.
        host.group_at_for_tests(strip_id, 0)
            .expect("maximized group")
            .publish_maximize_intent_for_tests();
        cursor(
            &mut host,
            &maximized_layout,
            Point::new(f64::from(W) - 1.0, f64::from(H) - 1.0),
        );
        host.tick_pending_maximize_harvest_for_tests();

        assert!(!chrome.borrow().is_fullscreen());
        let _restored_layout = lay_out_host(&mut host);
        let restored = host
            .group_at_for_tests(strip_id, 0)
            .expect("restored group")
            .get_rect();
        assert!(
            restored.x0 > 0.0,
            "the leading edge gutter must return on the first restored layout; got {restored:?}",
        );
    }

    // Phase `a_client_fullscreen_covers_the_whole_output`.
    // Full screen, unlike maximize, hides every piece of chrome.
    {
        let (tree, strip_id, ids) = single_strip_tree(&[100, 200]);
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let _ = lay_out_host(&mut host);
        chrome.borrow_mut().set_fullscreen_group(Some(ids[0]));
        let _ = lay_out_host(&mut host);
        let group = host
            .group_at_for_tests(strip_id, 0)
            .expect("fullscreen group")
            .get_rect();
        assert_approx_eq!(group.x0, 0.0, "a fullscreen group reaches the left edge");
        assert_approx_eq!(
            group.x1,
            f64::from(W),
            "a fullscreen group reaches the right edge"
        );
    }

    // Phase `switching_workspace_swaps_the_rendered_strips`.
    {
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
            focus_history: Vec::new(),
        };
        let group_400 = WindowGroup {
            id: 400,
            tabs: vec![mk_tab(4001, "B-third")],
            active_tab: 4001,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
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
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn focus_scrolls_groups_into_view_but_the_wheel_does_not_pan() {
    // Phase `a_group_focused_before_it_has_a_rect_is_still_scrolled_into_view`.
    // A group focused in the same frame it appeared must still pan into
    // view — `adopt_toplevel` creates and focuses a group before it has a
    // rect.
    {
        // One group as wide as the strip's usable width fills it, so a second
        // starts past the right edge — the geometry a newly launched app
        // lands in. (`Wide` is half of it; it filled the strip only while a
        // workspace wheel took a quarter of the display beside it.)
        let (mut tree, strip_id, _ids) = single_strip_tree(&[100]);
        tree.desktop.workspaces[0].strips[0].window_groups[0].width = WindowWidth::Fraction(2000);
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
            t.desktop.workspaces[0].strips[0].window_groups.push(
                shell::shell::state::WindowGroup {
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
                    focus_history: Vec::new(),
                },
            );
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

    // Phase `focusing_offscreen_group_scrolls_it_into_view`.
    {
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

    // Phase `wheel_over_strip_does_not_move_it`.
    {
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
            &ui::synthetic_input::mouse_wheel(0.0, -3.0),
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

    // Phase `streamed_three_finger_pan_moves_the_live_strip_and_settles_overpull`.
    {
        let (tree, strip_id, _ids) = single_strip_tree(&[100, 200, 300, 400]);
        let active_group = tree.desktop.active_group_for(strip_id);
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let _layout = lay_out_host(&mut host);
        let t0 = std::time::Instant::now();
        let at = |ms: u64| t0 + std::time::Duration::from_millis(ms);

        assert!(host.update_active_strip_pan(320.0, true, at(0)));
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
        assert!(host.update_active_strip_pan(legal_end + 500.0, false, at(500)));
        let held = host.group_row_mut_for_tests(strip_id).unwrap().scroll_x();
        assert!(
            held > legal_end && held < legal_end + 500.0,
            "overpull must keep moving with resistance: legal={legal_end}, held={held}"
        );

        // Lifted after the fingers came to rest: nothing is thrown.
        let released = at(700);
        assert!(host.end_active_strip_pan(released, false));
        host.group_row_mut_for_tests(strip_id)
            .unwrap()
            .tick_snap_at(released + std::time::Duration::from_millis(600));
        assert_approx_eq!(
            host.group_row_mut_for_tests(strip_id).unwrap().scroll_x(),
            legal_end,
            "release must settle the live strip to its legal trailing rest",
        );
    }
}

/// The live host laid out on a virtual frame clock while input arrives at
/// its own rate: what the strip does frame by frame.
struct PacedStrip {
    host: ShellOverlayHost,
    strip: StripId,
    t0: std::time::Instant,
}

impl PacedStrip {
    fn new(groups: &[u64]) -> Self {
        let (tree, strip, _ids) = single_strip_tree(groups);
        let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let _ = lay_out_host(&mut host);
        Self {
            host,
            strip,
            t0: std::time::Instant::now()
                .checked_add(std::time::Duration::from_secs(1))
                .expect("a second from now"),
        }
    }

    fn at(&self, ms: f64) -> std::time::Instant {
        self.t0
            .checked_add(std::time::Duration::from_secs_f64(ms / 1000.0))
            .expect("within the test's span")
    }

    /// Lay the desktop out for the frame due at `ms`; how far the strip's
    /// first window has moved left of where it started.
    fn frame(&mut self, ms: f64) -> f64 {
        let at = self.at(ms);
        let host = &mut self.host;
        ui::time::testing::with_monotonic_time(at, || {
            // What the host's update runs before every frame: the focus a
            // step wrote reaches the tree, and focus-follow frames it.
            host.flush_active_tab_writeback();
            host.tick_scroll_active_into_view_for_tests();
            let _ = lay_out_host(host);
        });
        -self
            .host
            .group_at_for_tests(self.strip, 0)
            .expect("first window")
            .get_rect()
            .x0
    }
}

#[test]
fn strip_motion_is_paced_by_the_frame_clock() {
    // Phase `a_steady_three_finger_swipe_moves_the_strip_evenly_every_frame`.
    // A 125 Hz touchpad into a 90 Hz display: showing the newest report moved
    // the strip one report on some frames and two on others.
    {
        let mut paced = PacedStrip::new(&[100, 200, 300, 400, 500, 600, 700, 800]);
        let (speed, report_dt, frame_dt) = (1200.0, 8.0, 1000.0 / 90.0);
        let mut reports = 0_u32;
        let mut previous = paced.frame(0.0);
        let mut steps = Vec::new();
        for frame in 0..36_u32 {
            let frame_ms = frame_dt * (f64::from(frame) + 0.5);
            // Every report the touchpad made before this frame.
            loop {
                let report_ms = report_dt * f64::from(reports);
                if report_ms > frame_ms {
                    break;
                }
                let travel = speed * (report_ms + report_dt) / 1000.0;
                let at = paced.at(report_ms);
                let _ = paced.host.update_active_strip_pan(travel, reports == 0, at);
                reports += 1;
            }
            let moved = paced.frame(frame_ms);
            if frame_ms > 30.0 {
                steps.push(moved - previous);
            }
            previous = moved;
        }
        drop(paced);
        let expected = speed / 90.0;
        eprintln!("three-finger swipe, 125 Hz into 90 Hz, steps per frame: {steps:?}");
        assert!(steps.len() > 30, "{steps:?}");
        assert!(
            steps
                .iter()
                .all(|step| (step - expected).abs() <= expected * 0.02),
            "every frame moves the strip the fingers' {expected:.2} px: {steps:?}"
        );
    }

    // Phase `a_spun_alt_shift_wheel_reveals_without_stalls_or_surges`. The
    // compositor turns each notch into one window of focus
    // (`step_focus_along_strip`), and every step retargets the reveal.
    {
        let mut paced = PacedStrip::new(&[100, 200, 300, 400, 500, 600, 700, 800, 900, 1000]);
        let (notch_dt, frame_dt) = (70.0, 1000.0 / 90.0);
        let mut previous = paced.frame(0.0);
        let mut notches = 0_u32;
        let mut last_notch = 0.0;
        let mut steps = Vec::new();
        for frame in 1..135_u32 {
            let ms = frame_dt * f64::from(frame);
            // Every notch the wheel clicked before this frame.
            loop {
                let notch_ms = frame_dt + notch_dt * f64::from(notches);
                if notches >= 9 || notch_ms > ms {
                    break;
                }
                ui::time::testing::with_monotonic_time(paced.at(notch_ms), || {
                    assert!(
                        paced.host.step_focus_along_strip(1),
                        "notch {notches} moves focus"
                    );
                });
                notches += 1;
                last_notch = notch_ms;
            }
            let moved = paced.frame(ms);
            steps.push((ms, moved - previous));
            previous = moved;
        }
        drop(paced);
        eprintln!("spun wheel, a notch every 70 ms at 90 Hz, steps per frame: {steps:?}");
        assert!(
            steps.iter().map(|(_, step)| step).sum::<f64>() > 600.0,
            "nine notches carry the strip several windows: {steps:?}"
        );
        // From the third notch on, once the reveal runs window to window
        // (the first notches reveal only the rest of a cut window), to the
        // last notch.
        let third_notch = frame_dt + 2.0 * notch_dt;
        let spinning: Vec<f64> = steps
            .iter()
            .skip_while(|(at, _)| *at <= third_notch)
            .take_while(|(at, _)| *at <= last_notch)
            .map(|&(_, step)| step)
            .collect();
        assert!(spinning.len() >= 10, "the spin moves the strip: {steps:?}");
        let max = spinning.iter().copied().fold(0.0_f64, f64::max);
        let min = spinning.iter().copied().fold(f64::INFINITY, f64::min);
        assert!(
            min >= max / 3.0,
            "no frame of a spin stalls next to a surging one: {spinning:?}"
        );
        let surge = spinning
            .windows(2)
            .map(|w| w[1] - w[0])
            .fold(0.0_f64, f64::max);
        let mean = spinning.iter().sum::<f64>()
            / f64::from(u32::try_from(spinning.len()).expect("a handful of frames"));
        assert!(
            surge <= mean * 0.6,
            "no notch makes the strip lurch forward: {spinning:?}"
        );
    }
}

#[test]
fn pointer_focus_keeps_native_card_in_place_but_keyboard_focus_reveals_it() {
    let (tree, strip, ids) = single_strip_tree(&[100, 200]);
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);
    let row = host.group_row_rect_for_tests(strip).unwrap();
    let clipped = host.group_at_for_tests(strip, 1).unwrap().get_rect();
    // The second card straddles the row's right edge: it starts inside the
    // visible row and ends past it.
    assert!(clipped.x0 < row.x1, "the card starts inside the row");
    assert!(
        clipped.x1 > row.x1,
        "the card is clipped by the row's right edge"
    );
    let before = host.group_row_mut_for_tests(strip).unwrap().scroll_x();
    cursor(
        &mut host,
        &layout,
        Point::new(clipped.x0 + 20.0, clipped.center().y),
    );
    press(&mut host, &layout);
    release(&mut host, &layout);
    // Drive the actual native per-frame focus marker and intent harvest.
    let doc = automerge::AutoCommit::new();
    host.update(&doc, &automerge::ROOT);
    host.tick_scroll_active_into_view_for_tests();
    host.group_row_mut_for_tests(strip)
        .unwrap()
        .tick_snap_at(std::time::Instant::now() + std::time::Duration::from_secs(1));
    assert_eq!(
        chrome.borrow().tree().desktop.active_group_for(strip),
        Some(ids[1])
    );
    assert_eq!(
        host.group_row_mut_for_tests(strip).unwrap().scroll_x(),
        before
    );
    assert!(host.group_at_for_tests(strip, 1).unwrap().focused());
    chrome
        .borrow()
        .tree_mut()
        .desktop
        .set_active_group(strip, ids[0]);
    host.tick_scroll_active_into_view_for_tests();
    chrome
        .borrow()
        .tree_mut()
        .desktop
        .set_active_group(strip, ids[1]);
    host.tick_scroll_active_into_view_for_tests();
    host.group_row_mut_for_tests(strip)
        .unwrap()
        .tick_snap_at(std::time::Instant::now() + std::time::Duration::from_secs(1));
    let _layout = lay_out_host(&mut host);
    assert!(host.group_at_for_tests(strip, 1).unwrap().get_rect().x1 <= row.x1 + 1.0);
}

// 3. Horizontal strip scroll

// 4. Drag-file-to-open

// ---------------------------------------------------------------------------
// Resizing from every window edge, first and last included. The model is one
// rule, `ui::widget::edge_resize`: every window owns both its edges, the seam
// between two is split down the middle except that the focused window owns
// all of it, a plain drag changes only the grabbed window (its opposite edge
// holding still), and Shift+drag is zero-sum between the two at the seam.
// ---------------------------------------------------------------------------

/// Three narrow groups, all on screen, the `focused`-th the one in use.
fn narrow_strip(focused: usize) -> (WorkspaceTree, StripId, Vec<GroupId>) {
    let (mut tree, strip_id, ids) = single_strip_tree(&[100, 200, 300]);
    for g in &mut tree.desktop.workspaces[0].strips[0].window_groups {
        g.width = WindowWidth::Narrow;
    }
    tree.desktop.active_group.insert(strip_id, ids[focused]);
    (tree, strip_id, ids)
}

fn on_screen_rects(host: &mut ShellOverlayHost, strip_id: StripId) -> Vec<vello::kurbo::Rect> {
    (0..3).map(|i| group_rect(host, strip_id, i)).collect()
}

fn widths_of(rects: &[vello::kurbo::Rect]) -> Vec<f64> {
    rects.iter().map(|r| r.width()).collect()
}

/// What one drag did: the windows before it, while the pointer was still
/// down at its end (after the frame loop's layout), and after the release.
struct EdgeDragOutcome {
    before: Vec<vello::kurbo::Rect>,
    held: Vec<vello::kurbo::Rect>,
    after: Vec<f64>,
    /// The persisted widths, in pixels, before and after.
    stored_before: Vec<f64>,
    stored_after: Vec<f64>,
}

/// Press at `from` with window `focused` in use, drag `dx` along the real
/// input path, release, and report what moved.
fn drag_from(
    focused: usize,
    from: impl Fn(&mut ShellOverlayHost, StripId) -> Point,
    dx: f64,
    shift: bool,
) -> EdgeDragOutcome {
    use winit::keyboard::ModifiersState;
    let (tree, strip_id, ids) = narrow_strip(focused);
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);
    assert!(
        host.group_at_for_tests(strip_id, focused)
            .unwrap()
            .focused(),
        "precondition: window {focused} is the one in use"
    );
    let usable = usable_strip_px(&mut host, strip_id);
    let stored = |chrome: &shell::shell::chrome_state::Chrome| -> Vec<f64> {
        let st = chrome.borrow();
        let groups = &st.tree().desktop.workspaces[0].strips[0].window_groups;
        ids.iter()
            .map(|id| {
                groups
                    .iter()
                    .find(|g| g.id == *id)
                    .unwrap()
                    .width
                    .resolve(usable)
            })
            .collect()
    };
    let stored_before = stored(&chrome);
    let before = on_screen_rects(&mut host, strip_id);
    let at = from(&mut host, strip_id);
    let mods = if shift {
        Modifiers::from(ModifiersState::SHIFT)
    } else {
        Modifiers::default()
    };
    cursor(&mut host, &layout, at);
    press_with_modifiers(&mut host, &layout, mods);
    cursor(&mut host, &layout, Point::new(at.x + dx, at.y));
    let layout = lay_out_host(&mut host);
    let held = on_screen_rects(&mut host, strip_id);
    release(&mut host, &layout);
    let _ = lay_out_host(&mut host);
    let after = widths_of(&on_screen_rects(&mut host, strip_id));
    host.tick_pending_width_harvest_for_tests();
    EdgeDragOutcome {
        before,
        held,
        after,
        stored_before,
        stored_after: stored(&chrome),
    }
}

fn group_rect(host: &mut ShellOverlayHost, strip_id: StripId, i: usize) -> vello::kurbo::Rect {
    host.group_at_for_tests(strip_id, i).unwrap().get_rect()
}

fn assert_widths(before: &[f64], after: &[f64], delta: [f64; 3], what: &str) {
    for i in 0..3 {
        assert!(
            (after[i] - (before[i] + delta[i])).abs() <= 1.5,
            "{what}: group {i} went {} -> {}, expected {:+} (before {before:?}, after {after:?})",
            before[i],
            after[i],
            delta[i],
        );
    }
}

/// Every window's width changed by `delta` on screen and in the persisted
/// tree, and nothing else.
fn assert_resized(outcome: &EdgeDragOutcome, delta: [f64; 3], what: &str) {
    let before = widths_of(&outcome.before);
    assert_widths(&before, &widths_of(&outcome.held), delta, what);
    assert_widths(&before, &outcome.after, delta, what);
    assert_widths(&outcome.stored_before, &outcome.stored_after, delta, what);
}

fn grip(
    group: usize,
    edge: ui::widget::WindowEdge,
) -> impl Fn(&mut ShellOverlayHost, StripId) -> Point {
    move |h, s| {
        let r = group_rect(h, s, group);
        match edge {
            ui::widget::WindowEdge::Leading => Point::new(r.x0 + 2.0, r.center().y),
            ui::widget::WindowEdge::Trailing => Point::new(r.x1 - 2.0, r.center().y),
        }
    }
}

const LEADING: ui::widget::WindowEdge = ui::widget::WindowEdge::Leading;
const TRAILING: ui::widget::WindowEdge = ui::widget::WindowEdge::Trailing;

// USER REQUIREMENT (2026-10-18): "if i increase the width of the thing i
// currently have open, it just pushes the apps next to it further away, it
// does not need to resize them. i can resize as i want, it doesnt change the
// actual size of any other app."
#[test]
fn the_focused_windows_left_edge_is_the_seam_and_resizes_only_that_window() {
    // Files (the middle window, in use) grabbed on its left edge where it
    // meets the terminal before it, even on the terminal's side of the seam,
    // and dragged left: Files grows to the left, the terminal slides left at
    // its own width, and Files' right edge holds still.
    for from in [grip(0, TRAILING), grip(1, LEADING)] {
        let outcome = drag_from(1, from, -40.0, false);
        assert_resized(&outcome, [0.0, 40.0, 0.0], "Files' left edge, dragged left");
        let (before, held) = (&outcome.before, &outcome.held);
        assert!(
            (held[1].x1 - before[1].x1).abs() <= 1.0,
            "Files' right edge holds still: {before:?} -> {held:?}"
        );
        assert!(
            (held[0].x1 - (before[0].x1 - 40.0)).abs() <= 1.0,
            "the terminal is pushed left: {before:?} -> {held:?}"
        );
        assert!(
            (held[2].x0 - before[2].x0).abs() <= 1.0,
            "the window after Files does not move"
        );
    }
}

// USER REQUIREMENT (2026-10-18): "if i increase the width of the thing i
// currently have open, it just pushes the apps next to it further away, it
// does not need to resize them. i can resize as i want, it doesnt change the
// actual size of any other app."
#[test]
fn middle_window_edges_resize_only_it_and_zero_sum_with_shift() {
    // The middle window is the one in use, so both its seams are its edges.
    let o = drag_from(1, grip(1, LEADING), 40.0, false);
    assert_resized(&o, [0.0, -40.0, 0.0], "middle window left edge");
    let o = drag_from(1, grip(1, LEADING), 40.0, true);
    assert_resized(&o, [40.0, -40.0, 0.0], "middle window left edge, shift");
    let o = drag_from(1, grip(1, TRAILING), 40.0, false);
    assert_resized(&o, [0.0, 40.0, 0.0], "middle window right edge");
    assert!(
        (o.held[1].x0 - o.before[1].x0).abs() <= 1.0,
        "its left edge holds still"
    );
    let o = drag_from(1, grip(1, TRAILING), 40.0, true);
    assert_resized(&o, [0.0, 40.0, -40.0], "middle window right edge, shift");
}

// USER REQUIREMENT (2026-10-18): the window in use wins the seam; elsewhere
// "i can resize as i want, it doesnt change the actual size of any other app."
#[test]
fn focus_wins_the_seam_and_an_unfocused_seam_is_split_down_the_middle() {
    // The first window in use owns its whole seam with the second, so the
    // second window's half of it still resizes the first.
    let o = drag_from(0, grip(1, LEADING), 40.0, false);
    assert_resized(&o, [40.0, 0.0, 0.0], "focused first window's seam");
    // The seam between the second and third, neither in use: each half is
    // its own window's edge.
    let o = drag_from(0, grip(1, TRAILING), 30.0, false);
    assert_resized(&o, [0.0, 30.0, 0.0], "unfocused seam, left half");
    let o = drag_from(0, grip(2, LEADING), -30.0, false);
    assert_resized(&o, [0.0, 0.0, 30.0], "unfocused seam, right half");
}

#[test]
fn hovering_a_seam_shows_the_edge_of_the_window_that_will_change() {
    let (tree, strip_id, _ids) = narrow_strip(1);
    let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);
    let at = grip(0, TRAILING)(&mut host, strip_id);
    cursor(&mut host, &layout, at);
    let highlight = |host: &mut ShellOverlayHost, i| {
        host.group_at_for_tests(strip_id, i)
            .unwrap()
            .edge_highlight()
    };
    assert_eq!(
        highlight(&mut host, 1),
        Some(LEADING),
        "the focused window's edge lights, though the pointer is on the other side"
    );
    assert_eq!(highlight(&mut host, 0), None, "the terminal's does not");
    assert_eq!(highlight(&mut host, 2), None);
    let mid = group_rect(&mut host, strip_id, 1).center();
    cursor(&mut host, &layout, mid);
    assert!((0..3).all(|i| highlight(&mut host, i).is_none()));
}

#[test]
fn first_window_right_edge_resizes_and_pushes_neighbours() {
    // Inside the painted grip, 2 px from the edge.
    let o = drag_from(0, grip(0, TRAILING), 40.0, false);
    assert_resized(&o, [40.0, 0.0, 0.0], "first window, right grip");
    // On the seam between the first and second window.
    let o = drag_from(
        0,
        |h, s| {
            let r = group_rect(h, s, 0);
            Point::new(r.x1 + 4.0, r.center().y)
        },
        40.0,
        false,
    );
    assert_resized(&o, [40.0, 0.0, 0.0], "first window, seam");
}

#[test]
fn last_window_right_edge_resizes() {
    // The last window ends 16 px short of the output edge, and the pointer
    // cannot leave the output, so the drag stays within it.
    let o = drag_from(2, grip(2, TRAILING), 12.0, false);
    assert_resized(&o, [0.0, 0.0, 12.0], "last window, right grip");
}

#[test]
fn first_window_left_edge_can_be_grabbed_and_resizes_it() {
    // Dragging the left edge towards the window's centre narrows it, away
    // from the centre widens it, and its right edge holds still. Neighbours
    // keep their widths.
    let o = drag_from(0, grip(0, LEADING), 40.0, false);
    assert_resized(&o, [-40.0, 0.0, 0.0], "first window, left edge inward");
    assert!((o.held[0].x1 - o.before[0].x1).abs() <= 1.0);
    let o = drag_from(0, grip(0, LEADING), -40.0, false);
    assert_resized(&o, [40.0, 0.0, 0.0], "first window, left edge outward");
    assert!((o.held[0].x1 - o.before[0].x1).abs() <= 1.0);
}

#[test]
fn every_window_edge_has_the_same_grab_zone() {
    let (tree, strip_id, _ids) = narrow_strip(0);
    let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let _layout = lay_out_host(&mut host);
    let zone = ui::widget::RESIZE_HANDLE_PX;
    for i in 0..3 {
        let r = group_rect(&mut host, strip_id, i);
        let g = host.group_at_for_tests(strip_id, i).unwrap();
        let lead = g.leading_resize_grab_rect().expect("leading grip");
        let trail = g.resize_grab_rect().expect("trailing grip");
        assert_approx_eq!(lead.width(), zone, "group {i} leading zone");
        assert_approx_eq!(trail.width(), zone, "group {i} trailing zone");
        assert_approx_eq!(lead.x0, r.x0);
        assert_approx_eq!(trail.x1, r.x1);
    }
    // The seam hit zone is exactly the two neighbouring grips plus the gap.
    let gap = group_rect(&mut host, strip_id, 1).x0 - group_rect(&mut host, strip_id, 0).x1;
    let seam = host
        .group_row_mut_for_tests(strip_id)
        .unwrap()
        .handle_at(0)
        .unwrap()
        .get_rect();
    assert_approx_eq!(seam.width(), 2.0 * zone + gap, "seam zone");
}
