//! A persisted desktop loads with its ids, defaults and local view intact.

#![allow(clippy::unwrap_used, reason = "test assertions")]
use serde_json::json;
use tilo_desktop_model::DropTarget;
use tilo_desktop_model::commands::{DesktopCommand, TabLocation, apply_command};
use tilo_desktop_model::{AppKind, DesktopTree, IdMinter, StripId, WindowWidth, WorkspaceId};

fn gap(gap_index: usize) -> DropTarget {
    DropTarget::GroupGap { gap_index }
}

// Captures the native persisted contract, including an id beyond
// JavaScript's exact numeric range. This fixture is not generated
// by the model under test.
fn persisted_tree() -> DesktopTree {
    serde_json::from_value(json!({
        "schema_version": 1,
        "workspaces": [{
            "id": 10, "room": "6f1c2d9e-4b3a-4e8f-9a7b-1c2d3e4f5a6b", "name": "Research", "strips": [{
                "id": 11, "name": "Main", "window_groups": [{
                    "id": 12, "active_tab": 9_007_199_254_741_001_u64,
                    "width": {"Fraction": 500}, "tabs": [
                        {"id": 13, "title": "Notes", "tile": {"AppPlaceholder": {"kind": {"Other": "Notes"}, "label": "Notes"}}},
                        {"id": 9_007_199_254_741_001_u64, "title": "Draft", "tile": {"AppPlaceholder": {"kind": {"Other": "Word"}, "label": "Writer"}}}
                    ]
                }]
            }], "collaborators": [{"id":"stale", "name":"Stale"}]
        }, {"id":20,"room":"0b9a8c7d-6e5f-4a3b-8c2d-1e0f9a8b7c6d","name":"Other","strips":[]}],
        "active": 10, "active_strip": {"10":11},
        "collaborators": [{"id":"stale", "name":"Stale"}]
    })).unwrap()
}

#[test]
fn persisted_state_preserves_ids_defaults_and_record_binding() {
    let tree = persisted_tree();
    assert_eq!(tree.schema_version, 1);
    assert_eq!(
        tree.workspaces[0].room.to_string(),
        "6f1c2d9e-4b3a-4e8f-9a7b-1c2d3e4f5a6b"
    );
    assert!(tree.workspaces[0].collaborators.is_empty());
    assert!(tree.collaborators.is_empty());
    let group = &tree.workspaces[0].strips[0].window_groups[0];
    assert_eq!(group.width, WindowWidth::Fraction(500));
    assert!(!group.maximized);
    assert_eq!(group.active_tab, 9_007_199_254_741_001);
    let serialized = serde_json::to_value(&tree).unwrap();
    assert_eq!(
        serialized["workspaces"][0]["strips"][0]["window_groups"][0]["tabs"][1]["id"],
        json!(9_007_199_254_741_001_u64)
    );
    assert!(serialized.get("doc").is_none());
    assert!(serialized.get("native_document_tabs").is_none());
}

#[test]
fn peer_local_selection_does_not_change_another_desktop_or_topology() {
    let mut one = persisted_tree();
    let two = one.clone();
    one.set_active(WorkspaceId(20));
    one.set_active_group(StripId(11), 12);
    one.workspaces[0].strips[0].window_groups[0].maximized = true;
    assert_eq!(two.active, WorkspaceId(10));
    assert!(two.active_group.is_empty());
    assert!(!two.workspaces[0].strips[0].window_groups[0].maximized);
    assert_eq!(
        one.workspaces[0].strips[0].window_groups[0].tabs[1].id,
        two.workspaces[0].strips[0].window_groups[0].tabs[1].id
    );
    assert!(one.switch_up());
    assert_eq!(one.active, WorkspaceId(10));
    assert!(!one.switch_up());
    assert!(one.switch_down());
    one.set_active(WorkspaceId(999));
    assert_eq!(one.active, WorkspaceId(20));
}

#[test]
fn reordering_keeps_tab_identity_selection_and_geometry() {
    let mut tree = persisted_tree();
    let reorder = |tree: &mut DesktopTree, tab: u64| {
        apply_command(
            &mut tree.workspaces[0],
            &DesktopCommand::ReorderTab {
                location: TabLocation {
                    strip: 11,
                    group: 12,
                    tab,
                },
                before: None,
            },
            &mut || Err("a reorder mints nothing".into()),
            &mut |_, _| Err("a reorder creates nothing".into()),
        )
    };
    assert!(reorder(&mut tree, 13).is_ok());
    let group = &tree.workspaces[0].strips[0].window_groups[0];
    assert_eq!(
        group.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
        vec![9_007_199_254_741_001, 13]
    );
    assert_eq!(group.active_tab, 9_007_199_254_741_001);
    assert_eq!(group.width, WindowWidth::Fraction(500));
    assert!(reorder(&mut tree, 99).is_err());
    assert_eq!(
        AppKind::Other("org.example.Foreign".into()).app_id(),
        "org.example.Foreign"
    );
}

#[test]
fn split_and_cross_strip_move_preserve_order_and_local_defaults() {
    let mut tree = persisted_tree();
    let split = tree
        .split_tab_into_new_group(StripId(11), 13, 1, &mut IdMinter::new(7))
        .unwrap();
    assert_eq!(split.source_group, 12);
    assert!(!split.source_group_removed);
    assert_eq!(split.new_group.tabs[0].id, 13);
    assert_eq!(split.new_group.active_tab, 13);
    assert_eq!(split.new_group.width, WindowWidth::Normal);
    assert!(!split.new_group.maximized);
    tree.workspaces[1]
        .strips
        .push(tilo_desktop_model::Strip::new(StripId(21), "Side"));
    let moved = tree
        .move_tab_across_strips(StripId(11), StripId(21), 13, 22, gap(100))
        .unwrap();
    assert!(moved.source_group_removed);
    assert_eq!(moved.target, gap(0));
    assert_eq!(tree.workspaces[0].strips[0].window_groups.len(), 1);
    assert_eq!(tree.workspaces[1].strips[0].window_groups[0].tabs[0].id, 13);
    assert_eq!(tree.active, WorkspaceId(10));
    assert!(
        tree.move_tab_across_strips(StripId(21), StripId(999), 13, 23, gap(0))
            .is_none()
    );
    assert_eq!(tree.workspaces[1].strips[0].window_groups[0].tabs[0].id, 13);
}

#[test]
fn native_flattening_keeps_numeric_local_selection_keys_loadable() {
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Host {
        #[serde(flatten)]
        desktop: DesktopTree,
    }
    let mut desktop = persisted_tree();
    desktop.set_active_group(StripId(11), 12);
    let bytes = serde_json::to_vec(&Host { desktop }).unwrap();
    let loaded: Host = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(loaded.desktop.active_strip[&WorkspaceId(10)], StripId(11));
    assert_eq!(loaded.desktop.active_group[&StripId(11)], 12);
}

#[test]
fn a_host_record_reads_its_tree_through_from_json() {
    // The browser keeps its desktop as bytes with the tree beside its own
    // fields, and reads it with the tree's one shared deserializer.
    #[derive(serde::Deserialize)]
    struct Stored {
        #[serde(deserialize_with = "DesktopTree::deserialize_via_json")]
        tree: DesktopTree,
        records: Vec<(u64, String)>,
    }
    let bytes = serde_json::to_vec(&json!({
        "tree": serde_json::to_value(persisted_tree()).unwrap(),
        "records": [[13, "notes-record"]],
    }))
    .unwrap();
    let stored: Stored = serde_json::from_slice(&bytes).unwrap();
    let group = &stored.tree.workspaces[0].strips[0].window_groups[0];
    assert_eq!(group.active_tab, 9_007_199_254_741_001);
    assert_eq!(stored.records, vec![(13, "notes-record".to_owned())]);

    let not_a_tree = br#"{"tree": {"workspaces": 3}, "records": []}"#;
    assert!(serde_json::from_slice::<Stored>(not_a_tree).is_err());
}
