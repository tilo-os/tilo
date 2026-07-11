//! End-to-end coverage for the desktop-environment window-management
//! interactions, driven through the real [`ShellOverlayHost`] event/state
//! path (not tautological helpers):
//!
//!   1. Horizontal window-group resize — drag an inter-group resize handle;
//!      neighbours reflow and the new widths persist into the
//!      [`WorkspaceTree`].
//!   2. Whole-group reorder — grab a group's tab-bar background and drop it
//!      at a new slot; the strip's `window_groups` order in the tree changes
//!      to the dropped position.
//!   3. Horizontal strip scroll — a wheel over the strip pans `scroll_x`, and
//!      focusing an off-screen group scrolls it into view.
//!   4. Drag-file-to-open — dropping a file resolves its MIME to an app and
//!      splices the launch intent into the active workspace on the real
//!      `handle_file_drop` path.
//!
//! Harness shape mirrors `cross_strip_drag.rs`: build a tree, hand it to
//! [`build_nirios_overlay_host_with_chrome`] so the shell captures the
//! per-strip `GroupRow` pointers, run a real layout pass so every rect lands
//! in screen space, then drive real winit events + the per-frame harvest
//! hooks the production `Widget::update` runs.

use std::collections::HashMap;

use shell::shell::build_nirios_overlay_host_with_chrome;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree,
};
use ui::theme::Theme;
use ui::widget::Widget;
use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use vello::kurbo::Point;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};

const W: f32 = 1280.0;
const H: f32 = 800.0;

/// One workspace, one strip, `group_ids.len()` groups (one tab each). Group
/// ids are the caller's so failure messages stay legible; the first group's
/// id is also recorded as the strip's active group.
fn single_strip_tree(group_ids: &[u64]) -> (WorkspaceTree, StripId, Vec<GroupId>) {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();

    let mk_tab = |gid: u64| Tab {
        id: gid * 10 + 1,
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
            active_tab: gid * 10 + 1,
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
    active_group.insert(strip_id, group_ids[0]);

    let tree = WorkspaceTree {
        workspaces: vec![ws],
        active: ws_id,
        active_strip,
        active_group,
        collaborators: vec![],
        ..Default::default()
    };
    (tree, strip_id, group_ids.to_vec())
}

struct Layout {
    taffy: TaffyTree<()>,
    #[allow(dead_code)]
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

/// Dispatch a single winit event through the whole host, exactly as the run
/// loop does.
fn dispatch(host: &mut ShellOverlayHost, layout: &Layout, event: WindowEvent) {
    let mut doc = automerge::AutoCommit::new();
    let doc_root = automerge::ROOT.clone();
    let modifiers = Modifiers::default();
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

fn cursor(host: &mut ShellOverlayHost, layout: &Layout, p: Point) {
    dispatch(
        host,
        layout,
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(p.x, p.y),
        },
    );
}

fn press(host: &mut ShellOverlayHost, layout: &Layout) {
    dispatch(
        host,
        layout,
        WindowEvent::MouseInput {
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
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        },
    );
}

// -----------------------------------------------------------------------
// 1. Horizontal window-group resize
// -----------------------------------------------------------------------

#[test]
fn resize_handle_drag_persists_new_group_widths_into_tree() {
    let (tree, strip_id, ids) = single_strip_tree(&[100, 200]);
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);

    // Both groups start at Normal = 600 px in the persisted tree.
    {
        let st = chrome.borrow();
        let tree = st.tree();
        let g = &tree.workspaces[0].strips[0].window_groups;
        assert_eq!(g[0].width.pixels(), 600);
        assert_eq!(g[1].width.pixels(), 600);
    }

    // Resolve the inter-group resize handle's centre from live geometry.
    let handle_rect = host
        .group_row_mut_for_tests(strip_id)
        .expect("strip GroupRow")
        .handle_at(0)
        .expect("one inter-group resize handle for a 2-group strip")
        .get_rect();
    let cx = (handle_rect.x0 + handle_rect.x1) / 2.0;
    let cy = (handle_rect.y0 + handle_rect.y1) / 2.0;

    // Drag the handle +40 px to the right: left neighbour grows, right shrinks.
    cursor(&mut host, &layout, Point::new(cx, cy));
    press(&mut host, &layout);
    cursor(&mut host, &layout, Point::new(cx + 40.0, cy));
    release(&mut host, &layout);

    // The production `Widget::update` runs this harvest each frame.
    host.tick_pending_width_harvest_for_tests();

    let st = chrome.borrow();
    let tree = st.tree();
    let groups = &tree.workspaces[0].strips[0].window_groups;
    let left = groups.iter().find(|g| g.id == ids[0]).unwrap();
    let right = groups.iter().find(|g| g.id == ids[1]).unwrap();
    assert_eq!(
        left.width.pixels(),
        640,
        "left group must have grown by the +40 px drag delta in the persisted tree",
    );
    assert_eq!(
        right.width.pixels(),
        560,
        "right group must have shrunk by the same 40 px (no overlap, widths reflow)",
    );
}

// -----------------------------------------------------------------------
// 2. Whole-group reorder
// -----------------------------------------------------------------------

fn group_grab_point(host: &mut ShellOverlayHost, strip_id: StripId, group_idx: usize) -> Point {
    let g = host
        .group_at_for_tests(strip_id, group_idx)
        .expect("group widget");
    let bar = g.bar_rect();
    let last = g.tab_rects().last().copied().unwrap_or(bar);
    Point::new((last.x1 + bar.x1) / 2.0, (bar.y0 + bar.y1) / 2.0)
}

#[test]
fn group_drag_reorders_strip_window_groups_in_tree() {
    let (tree, strip_id, _ids) = single_strip_tree(&[100, 200, 300]);
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);

    // Persisted order at boot: [100, 200, 300].
    {
        let st = chrome.borrow();
        let tree = st.tree();
        let order: Vec<u64> = tree.workspaces[0].strips[0]
            .window_groups
            .iter()
            .map(|g| g.id)
            .collect();
        assert_eq!(order, vec![100, 200, 300]);
    }

    // Grab group 100's tab-bar background and drag it past group 300's
    // centre so it lands last.
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

    // Production runs this harvest each frame inside `Widget::update`.
    host.tick_pending_group_reorder_harvest_for_tests();

    let st = chrome.borrow();
    let tree = st.tree();
    let order: Vec<u64> = tree.workspaces[0].strips[0]
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
}

// -----------------------------------------------------------------------
// 3. Horizontal strip scroll
// -----------------------------------------------------------------------

#[test]
fn wheel_over_strip_pans_scroll_offset() {
    let (tree, strip_id, _ids) = single_strip_tree(&[100, 200, 300, 400]);
    let (mut host, _chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);

    let row_rect = host
        .group_row_rect_for_tests(strip_id)
        .expect("row rect after layout");
    let before = host
        .group_row_mut_for_tests(strip_id)
        .unwrap()
        .scroll_x();

    // Cursor over the strip, then a downward wheel — Niri-style, the
    // vertical wheel pans the strip horizontally to the right.
    cursor(
        &mut host,
        &layout,
        Point::new((row_rect.x0 + row_rect.x1) / 2.0, (row_rect.y0 + row_rect.y1) / 2.0),
    );
    dispatch(
        &mut host,
        &layout,
        WindowEvent::MouseWheel {
            device_id: unsafe { std::mem::zeroed() },
            delta: MouseScrollDelta::LineDelta(0.0, -3.0),
            phase: winit::event::TouchPhase::Moved,
        },
    );

    let after = host
        .group_row_mut_for_tests(strip_id)
        .unwrap()
        .scroll_x();
    assert!(
        after > before,
        "a wheel over the strip must increase its horizontal scroll offset \
         (was {before}, now {after})",
    );
}

#[test]
fn focusing_offscreen_group_scrolls_it_into_view() {
    // Four Normal (600 px) groups overflow the 1280 px viewport, so the
    // last group starts off the right edge at scroll_x = 0.
    let (tree, strip_id, ids) = single_strip_tree(&[100, 200, 300, 400]);
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let layout = lay_out_host(&mut host);

    let last_id = ids[3];
    let row_rect = host.group_row_rect_for_tests(strip_id).unwrap();

    // Pre-condition: the last group is off the right edge, scroll is home.
    let before = host.group_row_mut_for_tests(strip_id).unwrap().scroll_x();
    assert_eq!(before, 0.0, "strip starts scrolled home");
    let last_rect_before = host
        .group_at_for_tests(strip_id, 3)
        .unwrap()
        .get_rect();
    assert!(
        last_rect_before.x1 > row_rect.x1,
        "precondition: the last group must hang off the strip's right edge \
         (group x1 {} vs row x1 {})",
        last_rect_before.x1,
        row_rect.x1,
    );

    // Focus the off-screen group in the persisted tree, then run the
    // focus-follow reconcile the production `Widget::update` runs.
    {
        let st = chrome.borrow();
        let mut tree = st.tree_mut();
        tree.set_active_group(strip_id, last_id);
    }
    host.tick_scroll_active_into_view_for_tests();

    let after = host.group_row_mut_for_tests(strip_id).unwrap().scroll_x();
    assert!(
        after > before,
        "focusing an off-screen group must pan the strip toward it \
         (scroll_x was {before}, now {after})",
    );

    // Re-layout so the group rects reflect the new scroll offset, then
    // assert the focused group is now fully within the strip viewport.
    let _layout = lay_out_host(&mut host);
    let row_rect = host.group_row_rect_for_tests(strip_id).unwrap();
    let last_rect_after = host
        .group_at_for_tests(strip_id, 3)
        .unwrap()
        .get_rect();
    assert!(
        last_rect_after.x1 <= row_rect.x1 + 1.0,
        "after scroll-into-view the focused group's right edge must be inside \
         the strip (group x1 {} vs row x1 {})",
        last_rect_after.x1,
        row_rect.x1,
    );
}

// -----------------------------------------------------------------------
// 4. Drag-file-to-open
// -----------------------------------------------------------------------

#[test]
fn dropping_a_code_file_opens_the_editor_in_the_active_workspace() {
    let (tree, _strip_id, _ids) = single_strip_tree(&[100]);
    let (_host, chrome) = build_nirios_overlay_host_with_chrome(&tree);

    let tabs_before = {
        let st = chrome.borrow();
        let tree = st.tree();
        tree.workspaces[0].strips[0].window_groups[0].tabs.len()
    };

    // Drop a `.ts` file — the real `WindowEvent::DroppedFile` entry point.
    let tab_id = chrome
        .borrow_mut()
        .handle_file_drop(std::path::Path::new("/some/where/component.ts"))
        .expect("a file drop must resolve to a launch intent");

    let st = chrome.borrow();
    let tree = st.tree();
    let group = &tree.workspaces[0].strips[0].window_groups[0];
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
fn dropping_a_folder_opens_files_app() {
    // A path with no extension that isn't a real dir on disk classifies as
    // a plain file → Files. A real folder classifies as Files too; either
    // way a non-code drop resolves to the file manager, never the Editor.
    let (tree, _strip_id, _ids) = single_strip_tree(&[100]);
    let (_host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let tmp = std::env::temp_dir().join("qos-deskenv-drop-folder");
    let _ = std::fs::create_dir_all(&tmp);

    let tab_id = chrome
        .borrow_mut()
        .handle_file_drop(&tmp)
        .expect("folder drop resolves");

    let st = chrome.borrow();
    let tree = st.tree();
    let group = &tree.workspaces[0].strips[0].window_groups[0];
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
