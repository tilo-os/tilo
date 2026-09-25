use automerge::{AutoCommit, ROOT, transaction::Transactable};
use qos_desktop::activity::{ActivitySnapshot, ActivityView, RoomActivityPane};
use qos_desktop::{Workspace, WorkspaceId, WorkspaceTree};

// Removing causal replay or writing to the admitted document while reviewing
// must fail this regression. The root schema is a real admitted room document.
#[test]
fn workspace_activity_review_keeps_live_heads() {
    let workspace = Workspace::new(WorkspaceId::new(), "Design");
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "room", workspace.room.to_string()).unwrap();
    doc.put(ROOT, "name", "Design").unwrap();
    doc.commit();
    doc.put(ROOT, "name", "Design review").unwrap();
    doc.commit();
    let mut tree = ActivitySnapshot::new(
        WorkspaceTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..Default::default()
        },
        doc,
    )
    .unwrap();
    let before = tree.doc.get_heads();
    let mut pane = RoomActivityPane::new();
    pane.begin_view(tree.active.raw());
    pane.refresh(&mut tree);
    pane.set_view(ActivityView::Replay);
    assert!(pane.entries().len() >= 2);
    pane.set_author_filter(Some("absent-author".into()));
    pane.set_author_filter(None);
    pane.finish_view(tree.active.raw());
    assert!(!pane.replay_is_playing());
    assert_eq!(tree.doc.get_heads(), before);
}

#[test]
fn workspace_activity_view_descriptors_use_exact_ids_and_opaque_workspace_payloads() {
    use qos_desktop::activity::view_actions::*;
    let id = WorkspaceId(u64::MAX - 3);
    let workspace = Workspace::new(id, "Same label");
    let tree = WorkspaceTree {
        active: id,
        workspaces: vec![workspace],
        ..Default::default()
    };
    let descriptors = workspace_view_descriptors(&tree, true);
    for (action, view) in [
        ("room.activity.open", ActivityView::CatchUp),
        ("room.activity.explore", ActivityView::Explore),
        ("room.activity.replay", ActivityView::Replay),
    ] {
        let descriptor = descriptors.iter().find(|d| d.id == action).unwrap();
        assert_eq!(
            descriptor.request,
            WorkspaceViewRequest::OpenActivity {
                workspace: id,
                view
            }
        );
        let action = descriptor.action();
        assert_eq!(action.kind, tilo_actions::OperationKind::View);
        assert_eq!(action.operation_type.as_deref(), Some(OPERATION_TYPE));
        let operation = action.build_args([]).unwrap();
        assert_eq!(operation.type_id, OPERATION_TYPE);
        let encoded: serde_json::Value = serde_json::from_slice(&operation.payload).unwrap();
        assert_eq!(encoded["workspace"], id.0.to_string());
        assert_eq!(
            serde_json::from_value::<WorkspaceViewRequest>(encoded).unwrap(),
            descriptor.request
        );
    }
    assert!(
        descriptors
            .iter()
            .any(|d| d.id == format!("workspace.select.{}", id.0))
    );
    assert!(
        workspace_view_descriptors(&tree, false)
            .iter()
            .all(|d| matches!(d.request, WorkspaceViewRequest::Select { .. }))
    );
}

#[test]
fn workspace_activity_detached_record_origins_do_not_collide() {
    use qos_desktop::activity::*;
    use ui::record::{RecordCheckout, RecordId, RecordKind};
    let workspace = Workspace::new(WorkspaceId(9), "Design");
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "room", workspace.room.to_string()).unwrap();
    doc.commit();
    let mut tree = ActivitySnapshot::new(
        WorkspaceTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..Default::default()
        },
        doc,
    )
    .unwrap();
    let mut record = AutoCommit::new();
    record.put(ROOT, "text", "shared causal hash").unwrap();
    record.commit();
    let checkout = RecordCheckout::new(RecordId::for_tab(1), RecordKind::new("test"));
    let inputs = vec![
        DetachedRecordHistory {
            app: "app-a".into(),
            checkout: checkout.clone(),
            document: Some(record.clone()),
        },
        DetachedRecordHistory {
            app: "app-b".into(),
            checkout: checkout.clone(),
            document: Some(record),
        },
        DetachedRecordHistory {
            app: "missing".into(),
            checkout,
            document: None,
        },
    ];
    let mut pane = RoomActivityPane::new();
    pane.refresh(&mut tree);
    pane.refresh_detached_records(&mut tree, inputs);
    assert_eq!(pane.feed_coverage().referenced_records, 3);
    assert_eq!(pane.feed_coverage().indexed_records, 2);
    assert!(!pane.feed_coverage().is_complete());
    let rows = pane.timeline().entries();
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows.iter()
            .map(|r| r.id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3
    );
    let mut apps = Vec::new();
    for i in 0..3 {
        pane.timeline_mut().set_selection(Some((i, i)));
        let detail = pane.selection_detail().unwrap();
        if let Some(app) = detail.resources.first().and_then(|r| r.app_id.clone()) {
            apps.push(app);
            assert!(pane.compare_selection(&mut tree, None).is_some());
        }
    }
    apps.sort();
    assert_eq!(apps, ["app-a", "app-b"]);
}

#[test]
fn workspace_activity_unsettled_refresh_retains_only_current_admission_rows() {
    use qos_desktop::activity::*;
    use ui::record::{RecordCheckout, RecordId, RecordKind};
    let workspace = Workspace::new(WorkspaceId(9), "Design");
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "room", workspace.room.to_string()).unwrap();
    doc.commit();
    let mut tree = ActivitySnapshot::new(
        WorkspaceTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..Default::default()
        },
        doc,
    )
    .unwrap();
    let mut record = AutoCommit::new();
    record.put(ROOT, "value", "settled").unwrap();
    record.commit();
    let input = DetachedRecordHistory {
        app: "app".into(),
        checkout: RecordCheckout::new(RecordId::for_tab(1), RecordKind::new("test")),
        document: Some(record),
    };
    let mut pane = RoomActivityPane::new();
    pane.refresh(&mut tree);
    pane.refresh_detached_records(&mut tree, vec![input.clone()]);
    let settled = pane.entries().to_vec();
    pane.refresh_detached_records(
        &mut tree,
        vec![DetachedRecordHistory {
            document: None,
            ..input
        }],
    );
    assert_eq!(pane.entries(), settled);
    assert!(!pane.feed_coverage().is_complete());
    pane.refresh_detached_records(&mut tree, vec![]);
    assert_eq!(pane.entries().len(), 1, "removed owner rows must disappear");
}

#[test]
fn workspace_activity_admitted_tasks_are_projected_read_only() {
    let workspace = Workspace::new(WorkspaceId(9), "Design");
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "room", workspace.room.to_string()).unwrap();
    let task: qos_desktop::WorkspaceTask = serde_json::from_value(serde_json::json!({
        "id":"real-task", "title":"Review actual admission", "description":"", "status":"queued", "route":"manual", "created_by":"author", "assigned_agent":null, "created_at":1, "updated_at":1
    })).unwrap();
    let tasks = doc
        .put_object(ROOT, "tasks_v1", automerge::ObjType::Map)
        .unwrap();
    doc.put(tasks, "real-task", serde_json::to_vec(&task).unwrap())
        .unwrap();
    doc.commit();
    let snapshot = ActivitySnapshot::new(
        WorkspaceTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..Default::default()
        },
        doc,
    )
    .unwrap();
    assert_eq!(snapshot.active_workspace().unwrap().tasks, vec![task]);
}
