#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions"
)]
use qos_desktop::{AppKind, StripId, WindowWidth, WorkspaceId, WorkspaceTree};
use serde_json::json;

// Captures the native persisted contract, including a legacy pixel width and
// an id beyond JavaScript's exact numeric range. This fixture is not generated
// by the model under test.
fn legacy_tree() -> WorkspaceTree {
    serde_json::from_value(json!({
        "workspaces": [{
            "id": 10, "name": "Research", "strips": [{
                "id": 11, "name": "Main", "window_groups": [{
                    "id": 12, "active_tab": 9007199254741001u64,
                    "width": {"Custom": 600}, "tabs": [
                        {"id": 13, "title": "Notes", "tile": {"AppPlaceholder": {"kind": {"Other": "Notes"}, "label": "Notes"}}},
                        {"id": 9007199254741001u64, "title": "Draft", "tile": {"AppPlaceholder": {"kind": {"Other": "Word"}, "label": "Writer"}}}
                    ]
                }]
            }], "collaborators": [{"id":"stale", "name":"Stale"}]
        }, {"id":20,"name":"Other","strips":[]}],
        "active": 10, "active_strip": {"10":11},
        "collaborators": [{"id":"stale", "name":"Stale"}]
    })).unwrap()
}

#[test]
fn legacy_state_preserves_ids_defaults_and_record_binding() {
    let tree = legacy_tree();
    assert_eq!(tree.schema_version, 1);
    assert!(tree.workspaces[0].room.is_nil());
    assert!(tree.workspaces[0].collaborators.is_empty());
    assert!(tree.collaborators.is_empty());
    let group = &tree.workspaces[0].strips[0].window_groups[0];
    assert_eq!(group.width, WindowWidth::Fraction(500));
    assert!(!group.maximized);
    assert_eq!(group.active_tab, 9007199254741001);
    assert_eq!(
        ui::record::RecordId::for_tab(group.tabs[1].id).as_str(),
        "tab:0020000000000009"
    );
    let serialized = serde_json::to_value(&tree).unwrap();
    assert_eq!(
        serialized["workspaces"][0]["strips"][0]["window_groups"][0]["tabs"][1]["id"],
        json!(9007199254741001u64)
    );
    assert!(serialized.get("doc").is_none());
    assert!(serialized.get("native_document_tabs").is_none());
}

#[test]
fn peer_local_selection_does_not_change_another_desktop_or_topology() {
    let mut one = legacy_tree();
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
    let mut tree = legacy_tree();
    assert!(tree.reorder_tab_within_group(12, 0, 99));
    let group = &tree.workspaces[0].strips[0].window_groups[0];
    assert_eq!(
        group.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
        vec![9007199254741001, 13]
    );
    assert_eq!(group.active_tab, 9007199254741001);
    assert_eq!(group.width, WindowWidth::Fraction(500));
    assert!(!tree.reorder_tab_within_group(12, 99, 0));
    assert_eq!(
        AppKind::Other("org.example.Foreign".into()).app_id(),
        "org.example.Foreign"
    );
}

#[test]
fn split_and_cross_strip_move_preserve_order_and_local_defaults() {
    let mut tree = legacy_tree();
    let split = tree.split_tab_into_new_group(StripId(11), 13, 1).unwrap();
    assert_eq!(split.source_group, 12);
    assert!(!split.pruned_source_group);
    assert_eq!(split.group.tabs[0].id, 13);
    assert_eq!(split.group.active_tab, 13);
    assert_eq!(split.group.width, WindowWidth::Normal);
    assert!(!split.group.maximized);
    tree.workspaces[1]
        .strips
        .push(qos_desktop::Strip::new(StripId(21), "Side"));
    let moved = tree
        .move_tab_across_strips(StripId(11), StripId(21), 13, 22, 100)
        .unwrap();
    assert!(moved.pruned_source_group);
    assert_eq!(moved.destination, 0);
    assert_eq!(tree.workspaces[0].strips[0].window_groups.len(), 1);
    assert_eq!(tree.workspaces[1].strips[0].window_groups[0].tabs[0].id, 13);
    assert_eq!(tree.active, WorkspaceId(10));
    assert!(
        tree.move_tab_across_strips(StripId(21), StripId(999), 13, 23, 0)
            .is_none()
    );
    assert_eq!(tree.workspaces[1].strips[0].window_groups[0].tabs[0].id, 13);
}

#[test]
fn native_flattening_keeps_numeric_local_selection_keys_loadable() {
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Host {
        #[serde(flatten)]
        desktop: WorkspaceTree,
    }
    let mut desktop = legacy_tree();
    desktop.set_active_group(StripId(11), 12);
    let bytes = serde_json::to_vec(&Host { desktop }).unwrap();
    let loaded: Host = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(loaded.desktop.active_strip[&WorkspaceId(10)], StripId(11));
    assert_eq!(loaded.desktop.active_group[&StripId(11)], 12);
}
