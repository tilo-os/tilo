//! Unit coverage for the per-frame `WindowGroup::maximized` writeback,
//! complementing the full-pipeline test in
//! `tests/maximize_persist_wireup_test.rs`. Two pins: a no-op tick must
//! leave persisted state alone, and a pending change must write through.
use super::*;
use crate::shell::build_tilo_overlay_host_with_chrome;
use crate::shell::state::FreshId as _;
use crate::shell::state::{
    AppKind, GroupId, SharedSurfaces, Strip, StripId, Tab, TileContent, WindowGroup as StateGroup,
    WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree,
};
use std::collections::HashMap as StdHashMap;

fn one_group_tree(maximized: bool) -> (WorkspaceTree, StripId, GroupId) {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();
    let group_id: GroupId = 7777;
    let tab_id: u64 = 0xa1;
    let strip = Strip {
        id: strip_id,
        name: "Solo".into(),
        window_groups: vec![StateGroup {
            id: group_id,
            tabs: vec![Tab {
                id: tab_id,
                title: "Solo".into(),
                tile: TileContent::Stub {
                    kind: AppKind::Other("Test".into()),
                    label: "solo".into(),
                },
            }],
            active_tab: tab_id,
            width: StateWidth::Normal,
            maximized,
        }],
    };
    let ws = Workspace {
        owner: None,
        pending_ownership_transfer: None,
        ownership_transfer_audit: Vec::new(),
        ownership_transfer_cancellations: Vec::new(),
        permission_profiles: Vec::new(),
        capabilities: Vec::new(),
        default_apps: desktop_policy::WorkspaceDefaults::default(),
        tasks: Vec::new(),
        effect_receipts: Vec::new(),
        shortcuts: Vec::new(),
        schedule_claims: Vec::new(),
        id: ws_id,
        room: crate::shell::state::RoomId::new(),
        name: "Test".into(),
        strips: vec![strip],
        collaborators: vec![],
        shared_surfaces: SharedSurfaces::default(),
    };
    let mut active_strip = StdHashMap::new();
    active_strip.insert(ws_id, strip_id);
    let mut active_group = StdHashMap::new();
    active_group.insert(strip_id, group_id);
    let tree = WorkspaceTree {
        desktop: crate::shell::state::DesktopTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group,
            collaborators: vec![],
            ..Default::default()
        },
        ..Default::default()
    };
    (tree, strip_id, group_id)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn maximize_and_resize_harvests_flush_layout_changes_to_disk() {
    /// Priming `pending_maximize_change = Some(true)` and ticking the
    /// harvest must mirror it into the persisted `WorkspaceTree`.
    fn harvest_writes_pending_maximize_change_through_to_tree() {
        use std::collections::HashMap;
        use taffy::{AvailableSpace, NodeId, Size, Style, TaffyTree};
        use vello::kurbo::Point;
        use winit::dpi::PhysicalPosition;
        use winit::event::{Modifiers, WindowEvent};
        let (tree, strip_id, group_id) = one_group_tree(false);
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);

        // Flip `maximized` and arm the pending-change slot the way the
        // production drain block does — the integration test covers the click.
        {
            let g = host
                .group_at_for_tests(strip_id, 0)
                .expect("strip-0 group-0 widget");
            assert_eq!(g.id, group_id);
            assert!(!g.maximized, "seed mirrors persisted state");
            g.maximized = true;
            // Can't poke `pending_maximize_change` from outside the crate,
            // so go through the publish + drain path the production click
            // flow uses.
            g.publish_maximize_intent_for_tests();
            // The drain runs inside `handle_event`; reset to false so the
            // publish flips it back to true through `take_max`.
            g.maximized = false;
        }

        // A no-op cursor-move runs the drain block and arms
        // `pending_maximize_change`.
        let g = host
            .group_at_for_tests(strip_id, 0)
            .expect("strip-0 group-0 widget");
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(Style::default(), &[group_node])
            .unwrap();
        let map_for_measure = node_map.clone();
        let mut tr = ui::text_render::TextRenderer::new();
        let _ = taffy.compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(800.0),
                height: AvailableSpace::Definite(600.0),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = map_for_measure.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        );
        g.apply_layout(&taffy, group_node, Point::ZERO);
        let mut state = automerge::AutoCommit::new();
        let root_id = automerge::ROOT;
        let mut handled = false;
        let mut focus_target: Option<NodeId> = None;
        let moved = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(9000.0, 9000.0),
        };
        g.handle_event(
            &moved,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: &mut state,
                path: &root_id,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: group_node,
                taffy: &taffy,
            },
        );
        assert!(g.maximized, "drain must flip maximized true");

        // Pre-harvest: persisted state still un-maximized.
        {
            let st = chrome.borrow();
            assert!(
                !st.tree().desktop.workspaces[0].strips[0].window_groups[0].maximized,
                "persisted state lags widget until harvest runs",
            );
        }

        host.tick_pending_maximize_harvest_for_tests();

        // Post-harvest: persisted state matches widget.
        {
            let st = chrome.borrow();
            assert!(
                st.tree().desktop.workspaces[0].strips[0].window_groups[0].maximized,
                "harvest writes pending change through to persisted state",
            );
        }
    }

    /// A no-op tick must not overwrite a persisted `maximized = true` with
    /// the widget default — gated on `take_pending_maximize_change`
    /// returning `Some(_)`, not on the widget's current value.
    fn harvest_no_op_tick_leaves_persisted_state_alone() {
        let (tree, _strip_id, _group_id) = one_group_tree(true);
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);

        host.tick_pending_maximize_harvest_for_tests();

        let st = chrome.borrow();
        assert!(
            st.tree().desktop.workspaces[0].strips[0].window_groups[0].maximized,
            "no-op harvest must not overwrite persisted true with widget default",
        );
    }

    fn set_group_widths_flushes_the_layout_to_disk() {
        let (mut tree, strip_id, group_id) = one_group_tree(false);
        let state_dir = tempfile::tempdir().expect("temporary state directory");
        let state_path = state_dir.path().join("state.json");
        tree.persist_path = Some(state_path.clone());
        let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&tree);

        assert_eq!(host.set_group_widths(600), vec![group_id]);

        let restarted = crate::shell::persist::load_tree_from(&state_path)
            .expect("width writeback creates valid durable state");
        let group = restarted.desktop.workspaces[0]
            .strips
            .iter()
            .find(|strip| strip.id == strip_id)
            .and_then(|strip| {
                strip
                    .window_groups
                    .iter()
                    .find(|group| group.id == group_id)
            })
            .expect("persisted group");
        assert_eq!(
            group.width,
            StateWidth::Fraction(600),
            "a control-driven width change must survive restart"
        );
    }

    fn resize_focused_group_flushes_the_layout_to_disk() {
        let (mut tree, strip_id, group_id) = one_group_tree(false);
        let state_dir = tempfile::tempdir().expect("temporary state directory");
        let state_path = state_dir.path().join("state.json");
        tree.persist_path = Some(state_path.clone());
        let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&tree);

        assert!(host.resize_focused_group(50));

        let restarted = crate::shell::persist::load_tree_from(&state_path)
            .expect("keyboard width writeback creates valid durable state");
        let group = restarted.desktop.workspaces[0]
            .strips
            .iter()
            .find(|strip| strip.id == strip_id)
            .and_then(|strip| {
                strip
                    .window_groups
                    .iter()
                    .find(|group| group.id == group_id)
            })
            .expect("persisted group");
        assert_eq!(
            group.width,
            StateWidth::Fraction(ui::widget::WIDTH_NORMAL_PERMILLE.saturating_add(50)),
            "a keyboard resize must survive restart"
        );
    }

    harvest_writes_pending_maximize_change_through_to_tree();
    harvest_no_op_tick_leaves_persisted_state_alone();
    set_group_widths_flushes_the_layout_to_disk();
    resize_focused_group_flushes_the_layout_to_disk();
}
