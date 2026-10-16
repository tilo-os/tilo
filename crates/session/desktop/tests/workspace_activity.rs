use automerge::{AutoCommit, ROOT, transaction::Transactable};
use tilo_desktop::activity::{ActivitySnapshot, ActivityView, RoomActivityPane};
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId};

// Removing causal replay or writing to the admitted document while reviewing
// must fail this regression. The root schema is a real admitted room document.
#[test]
fn workspace_activity_review_keeps_live_heads() {
    let workspace = Workspace::new(WorkspaceId(1), "Design");
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "room", workspace.room.to_string()).unwrap();
    doc.put(ROOT, "name", "Design").unwrap();
    doc.commit();
    doc.put(ROOT, "name", "Design review").unwrap();
    doc.commit();
    let mut tree = ActivitySnapshot::new(
        DesktopTree {
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
    assert!(!pane.entries().is_empty(), "the rename is a feed change");
    pane.set_author_filter(Some("absent-author".into()));
    pane.set_author_filter(None);
    pane.finish_view(tree.active.raw());
    assert!(!pane.replay_is_playing());
    assert_eq!(tree.doc.get_heads(), before);
}

// A local desktop's room document, in the `contents` schema: its bookkeeping
// (the id counter, the record index) listed as a change, or any storage key
// shown as a row's words, must fail this regression.
#[test]
fn workspace_activity_names_room_events_and_leaves_bookkeeping_out() {
    use automerge::{ObjType, transaction::CommitOptions};
    let workspace = Workspace::new(WorkspaceId(3), "Reading group");
    let mut doc = AutoCommit::new();
    let stamp = |doc: &mut AutoCommit, at: i64| {
        doc.commit_with(CommitOptions::default().with_time(1_800_000_000 + at));
    };
    doc.put(ROOT, "room", workspace.room.to_string()).unwrap();
    doc.put(ROOT, "name", "Reading group").unwrap();
    doc.put(ROOT, "owner", "You").unwrap();
    doc.put(ROOT, "desktop_counter_v1", 3_u64).unwrap();
    let owners = doc
        .put_object(ROOT, "contents_owners_v1", ObjType::Map)
        .unwrap();
    doc.put(&owners, "0001", "device").unwrap();
    let contents = doc.put_object(ROOT, "contents", ObjType::Map).unwrap();
    let strip = doc
        .put_object(&contents, "0001000000000003", ObjType::Map)
        .unwrap();
    doc.put(&strip, "name", "The Odyssey").unwrap();
    doc.put(&strip, "order", 0_u64).unwrap();
    let groups = doc.put_object(&strip, "groups", ObjType::Map).unwrap();
    let open = |doc: &mut AutoCommit, group: &str, tab: &str, title: &str| {
        let group = doc.put_object(&groups, group, ObjType::Map).unwrap();
        doc.put(&group, "order", 0_u64).unwrap();
        let tabs = doc.put_object(&group, "tabs", ObjType::Map).unwrap();
        let tab = doc.put_object(&tabs, tab, ObjType::Map).unwrap();
        doc.put(&tab, "title", title).unwrap();
        doc.put(&tab, "order", 0_u64).unwrap();
    };
    open(
        &mut doc,
        "0001000000000001",
        "0001000000000002",
        "The Odyssey",
    );
    stamp(&mut doc, 0);
    doc.put(ROOT, "desktop_counter_v1", 5_u64).unwrap();
    let records = doc.put_object(ROOT, "records", ObjType::Map).unwrap();
    doc.put(&records, "uuid:notes", "word_processor.document")
        .unwrap();
    open(
        &mut doc,
        "0001000000000004",
        "0001000000000005",
        "Word Processor",
    );
    stamp(&mut doc, 1);
    doc.put(ROOT, "desktop_counter_v1", 6_u64).unwrap();
    doc.put(&records, "uuid:chat", "chat.workspace").unwrap();
    stamp(&mut doc, 2);
    doc.put(ROOT, "layout_hint_v2", "wide").unwrap();
    stamp(&mut doc, 3);
    doc.delete(&groups, "0001000000000004").unwrap();
    stamp(&mut doc, 4);
    let mut tree = ActivitySnapshot::new(
        DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..Default::default()
        },
        doc,
    )
    .unwrap();

    let mut pane = RoomActivityPane::new();
    pane.refresh(&mut tree);
    let mut rows = pane
        .entries()
        .iter()
        .map(|row| (row.timestamp, row.summary.clone()))
        .collect::<Vec<_>>();
    rows.sort();
    let summaries = rows.into_iter().map(|(_, s)| s).collect::<Vec<_>>();
    assert_eq!(
        summaries.len(),
        2,
        "setup, a plain open and bookkeeping stay out: {summaries:?}"
    );
    let [updated, closed] = summaries.as_slice() else {
        return;
    };
    assert_eq!(updated, "Updated the workspace");
    assert_eq!(closed, "Closed 1 window");
    assert!(
        summaries
            .iter()
            .all(|s| !s.starts_with("Opened") && !s.starts_with("Created")),
        "a plain open and the workspace's first write are not changes: {summaries:?}"
    );
    for summary in &summaries {
        assert!(
            !["_v1", "_v2", "contents", "counter", "records"]
                .iter()
                .any(|key| summary.contains(key)),
            "a storage key in a row: {summary}"
        );
    }
}

#[test]
fn workspace_activity_view_descriptors_use_exact_ids_and_opaque_workspace_payloads() {
    use tilo_desktop::activity::view_actions::*;
    let id = WorkspaceId(u64::MAX - 3);
    let workspace = Workspace::new(id, "Same label");
    let tree = DesktopTree {
        active: id,
        workspaces: vec![workspace],
        ..Default::default()
    };
    let descriptors = workspace_view_descriptors(&tree, true);
    for (action, view) in [
        ("room.activity.open", ActivityView::CatchUp),
        ("room.activity.tasks", ActivityView::Tasks),
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
    use tilo_desktop::activity::*;
    use ui::record::{RecordCheckout, RecordId, RecordKind};
    let workspace = Workspace::new(WorkspaceId(9), "Design");
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "room", workspace.room.to_string()).unwrap();
    doc.commit();
    let mut tree = ActivitySnapshot::new(
        DesktopTree {
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
    assert_eq!(rows.len(), 2, "setup bookkeeping is not a feed row");
    assert_eq!(
        rows.iter()
            .map(|r| r.id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        2
    );
    let mut apps = Vec::new();
    for i in 0..2 {
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

// Listing a read marker, or offering a filter for a participant no row can
// be attributed to, must fail this regression.
#[test]
fn workspace_activity_lists_content_and_filters_by_the_people_it_names() {
    use automerge::{ActorId, transaction::CommitOptions};
    use tilo_actions::{ActionContext, ChangeKind};
    use tilo_desktop::activity::*;
    use ui::record::{RecordCheckout, RecordId, RecordKind};
    let workspace = Workspace::new(WorkspaceId(9), "Design");
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "room", workspace.room.to_string()).unwrap();
    doc.commit();
    let mut tree = ActivitySnapshot::new(
        DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..Default::default()
        },
        doc,
    )
    .unwrap();
    let posted = ActionContext::for_resource("chat.channel:c", "chat.channel", "general")
        .with_app("so.tilo.chat", "Chat")
        .with_described(tilo_actions::Described::new(tilo_actions::Did::verb(
            tilo_actions::ActionKey::new("so.tilo.chat", "chat.channel", "post-message"),
            [],
        )));
    let commit = |record: &mut AutoCommit, context: &ActionContext, at: i64| {
        record.commit_with(
            CommitOptions::default()
                .with_time(at)
                .with_message(context.change_message().unwrap()),
        );
    };
    let ada = ActorId::random();
    let mut record = AutoCommit::new().with_actor(ada.clone());
    record.put(ROOT, "message", "hello").unwrap();
    commit(&mut record, &posted, 1_800_000_000);
    record.put(ROOT, "read", 1).unwrap();
    let marker = ActionContext::for_resource("chat.channel:c", "chat.channel", "general")
        .with_change(ChangeKind::Housekeeping);
    commit(&mut record, &marker, 1_800_000_001);
    record.put(ROOT, "preview", "fetched").unwrap();
    commit(
        &mut record,
        &posted.clone().with_change(ChangeKind::External),
        1_800_000_001,
    );
    record.set_actor(ActorId::random());
    record.put(ROOT, "message", "hi").unwrap();
    commit(&mut record, &posted, 1_800_000_002);

    let mut pane = RoomActivityPane::new();
    pane.authors_mut().register(
        &ada.to_hex_string(),
        ui::widget::HistoryAuthor::new("Ada", 120.0, "ada"),
    );
    pane.refresh(&mut tree);
    pane.refresh_detached_records(
        &mut tree,
        vec![DetachedRecordHistory {
            app: "so.tilo.chat".into(),
            checkout: RecordCheckout::new(RecordId::for_tab(1), RecordKind::new("chat")),
            document: Some(record),
        }],
    );
    let labels = pane
        .entries()
        .iter()
        .map(|row| row.summary.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        ["Post message in general", "Post message in general"],
        "no read marker, nothing that came from outside"
    );
    assert!(
        pane.entries()
            .iter()
            .any(|row| row.author.name == ui::history_source::UNKNOWN_AUTHOR_NAME),
        "a change history cannot name still shows under Everyone"
    );
    assert_eq!(pane.contributors(), ["Ada"]);
}

#[test]
fn workspace_activity_unsettled_refresh_retains_only_current_admission_rows() {
    use tilo_desktop::activity::*;
    use ui::record::{RecordCheckout, RecordId, RecordKind};
    let workspace = Workspace::new(WorkspaceId(9), "Design");
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "room", workspace.room.to_string()).unwrap();
    doc.commit();
    let mut tree = ActivitySnapshot::new(
        DesktopTree {
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
    assert_eq!(
        pane.entries().len(),
        0,
        "detached rows disappear and the workspace's first write is not a change"
    );
}

#[test]
fn workspace_activity_admitted_tasks_are_projected_read_only() {
    let workspace = Workspace::new(WorkspaceId(9), "Design");
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "room", workspace.room.to_string()).unwrap();
    let task: tilo_desktop_model::WorkspaceTask = serde_json::from_value(serde_json::json!({
        "id":"real-task", "title":"Review actual admission", "description":"", "status":"queued", "route":"manual", "created_by":"author", "assigned_agent":null, "created_at":1, "updated_at":1
    })).unwrap();
    let tasks = doc
        .put_object(ROOT, "tasks_v1", automerge::ObjType::Map)
        .unwrap();
    doc.put(tasks, "real-task", serde_json::to_vec(&task).unwrap())
        .unwrap();
    doc.commit();
    let snapshot = ActivitySnapshot::new(
        DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..Default::default()
        },
        doc,
    )
    .unwrap();
    assert_eq!(snapshot.active_workspace().unwrap().tasks, vec![task]);
}
