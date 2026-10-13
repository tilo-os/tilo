//! Presentation over committed action context and exact causal comparisons.
use super::*;
use automerge::transaction::CommitOptions;
use std::collections::BTreeMap;
use tilo_actions::{ActionContext, ActionFacet};

#[test]
fn readable_comparison_keeps_historical_names_and_machine_evidence() {
    let mut doc = automerge::AutoCommit::new();
    doc.put(automerge::ROOT, "title", "Before").unwrap();
    doc.commit();
    let context = ActionContext::for_resource("task:opaque-123", "task", "Prepare launch brief")
        .with_app("org.tilo.Tasks", "Tasks")
        .with_facet(ActionFacet::new("tilo.receipt", "invocation:private-456"))
        .with_facet(ActionFacet::new("file-type", "Markdown"));
    doc.put(automerge::ROOT, "task:opaque-123", "secret payload")
        .unwrap();
    let hash = doc
        .commit_with(CommitOptions::default().with_message(context.change_message().unwrap()))
        .unwrap();
    let id = ui::history_source::entry_id_from_hash(&hash.to_string());
    let entries = HistorySource::new().entries(&mut doc);
    let index = entries.iter().position(|entry| entry.id == id).unwrap();
    let mut detail = HistorySelectionDetail::from_range(&entries, (index, index)).unwrap();
    // A current registry or detached-record projection must not overwrite the
    // name the selected action recorded.
    detail.resources[0].label = "Renamed task today".into();
    let heads = doc.get_heads();
    let comparison = ui::history_source::compare_entries(&mut doc, &[id]).unwrap();
    let provenance = detail_provenance(&detail, &entries);
    let preview = comparison_preview_labels(&comparison, Some(&detail), &entries).join("\n");
    assert!(preview.contains("Prepare launch brief"));
    assert!(provenance.contains("Prepare launch brief"));
    assert!(provenance.contains("File type: Markdown"));
    assert!(!provenance.contains("Renamed task today"));
    for private in [
        "task:opaque-123",
        "tilo.receipt",
        "invocation:private-456",
        "secret payload",
    ] {
        assert!(!provenance.contains(private));
        assert!(!preview.contains(private));
    }
    assert_eq!(detail.resources[0].id, "task:opaque-123");
    assert_eq!(detail.facets[0].value, "invocation:private-456");
    assert_eq!(comparison.entries, [id]);
    assert_eq!(
        readable_resources(&detail, &[]),
        [t("activity-readable-task-unavailable")]
    );
    let mut empty = comparison.clone();
    empty.patches.clear();
    assert!(comparison_preview_labels(&empty, Some(&detail), &entries).is_empty());
    assert_eq!(comparison_empty_label(&empty), t("activity-no-differences"));
    let after = automerge::AutoCommit::load(&comparison.after_document).unwrap();
    assert_eq!(
        after
            .get(automerge::ROOT, "task:opaque-123")
            .unwrap()
            .unwrap()
            .0
            .as_str(),
        Some("secret payload")
    );
    assert_eq!(doc.get_heads(), heads);
}

#[test]
fn readable_comparison_unknown_fields_and_resources_are_explicitly_unavailable() {
    let mut doc = automerge::AutoCommit::new();
    let context =
        ActionContext::for_resource("invocation:private", "tilo.receipt", "invocation:private");
    doc.put(automerge::ROOT, "token=secret", "secret value")
        .unwrap();
    let hash = doc
        .commit_with(CommitOptions::default().with_message(context.change_message().unwrap()))
        .unwrap();
    let id = ui::history_source::entry_id_from_hash(&hash.to_string());
    let entries = HistorySource::new().entries(&mut doc);
    let detail = HistorySelectionDetail::from_range(&entries, (0, 0)).unwrap();
    assert_eq!(
        readable_resources(&detail, &entries),
        [t("activity-readable-receipt-unavailable")]
    );
    let comparison = ui::history_source::compare_entries(&mut doc, &[id]).unwrap();
    assert_eq!(
        comparison_change_labels(&comparison.patches),
        [field_change(
            "activity-change-updated-field",
            t("activity-readable-field-unavailable")
        )]
    );
    assert_eq!(humanize_field("title"), t("activity-readable-field-title"));
    assert_eq!(humanize_field("name"), t("activity-readable-field-name"));
    assert_eq!(
        humanize_field("status"),
        t("activity-readable-field-status")
    );
    assert_eq!(humanize_field("body"), t("activity-change-content"));
}

#[test]
fn readable_comparison_excludes_workspace_bookkeeping_but_keeps_task_actions() {
    use automerge::{ObjType, ROOT};
    use tilo_actions::{Did, EditOp, FieldEdit};

    let mut doc = automerge::AutoCommit::new();
    // The desktop tree stores bookkeeping maps under workspaces[list index].
    let workspaces = doc.put_object(ROOT, "workspaces", ObjType::List).unwrap();
    let workspace = doc.insert_object(&workspaces, 0, ObjType::Map).unwrap();
    doc.commit();
    let mut context = ActionContext::for_resource("task:private", "task", "Launch brief")
        .with_app("org.tilo.Tasks", "Tasks at the time");
    context.what = Some(Did::Edit(FieldEdit {
        ty: "task".into(),
        field: "status".into(),
        path: Vec::new(),
        op: EditOp::Set,
        old: None,
        new: None,
        args: BTreeMap::default(),
    }));
    for (map, key) in [
        ("effect_receipts_v1", "invocation:private"),
        ("tasks_v1", "task:private"),
    ] {
        let object = doc.put_object(&workspace, map, ObjType::Map).unwrap();
        doc.put(&object, key, automerge::ScalarValue::Bytes(vec![1, 2, 3]))
            .unwrap();
    }
    let hash = doc
        .commit_with(CommitOptions::default().with_message(context.change_message().unwrap()))
        .unwrap();
    let id = ui::history_source::entry_id_from_hash(&hash.to_string());
    let entries = HistorySource::new().entries(&mut doc);
    let comparison = ui::history_source::compare_entries(&mut doc, &[id]).unwrap();
    let patches = format!("{:?}", comparison.patches);
    let before = comparison.before_document.clone();
    let after = comparison.after_document.clone();
    assert!(!comparison.patches.is_empty());
    assert!(comparison_change_labels(&comparison.patches).is_empty());
    assert_eq!(
        comparison_empty_label(&comparison),
        t("activity-comparison-no-readable-content")
    );
    assert_ne!(
        comparison_empty_label(&comparison),
        t("activity-no-differences")
    );
    let preview = comparison_preview_labels(&comparison, None, &entries).join("\n");
    assert!(preview.contains("Launch brief"), "{preview}");
    assert!(preview.contains("Tasks at the time"), "{preview}");
    assert!(!preview.contains("unavailable"), "{preview}");
    for private in [
        "task:private",
        "invocation:private",
        "tasks_v1",
        "effect_receipts_v1",
    ] {
        assert!(!preview.contains(private), "{preview}");
    }
    assert_eq!(format!("{:?}", comparison.patches), patches);
    assert_eq!(comparison.before_document, before);
    assert_eq!(comparison.after_document, after);
    let after = automerge::AutoCommit::load(&after).unwrap();
    let receipts = after
        .get(&workspace, "effect_receipts_v1")
        .unwrap()
        .unwrap()
        .1;
    assert!(
        after
            .get(&receipts, "invocation:private")
            .unwrap()
            .is_some()
    );
}

#[test]
fn readable_comparison_excludes_room_receipt_invocation_updates_and_deletions() {
    let mut doc = automerge::AutoCommit::new();
    let receipts = doc
        .put_object(
            automerge::ROOT,
            "effect_receipts_v1",
            automerge::ObjType::Map,
        )
        .unwrap();
    doc.put(&receipts, "invocation:private", "Pending").unwrap();
    doc.commit();
    doc.put(&receipts, "invocation:private", "Completed")
        .unwrap();
    let hash = doc.commit().unwrap();
    let id = ui::history_source::entry_id_from_hash(&hash.to_string());
    let comparison = ui::history_source::compare_entries(&mut doc, &[id]).unwrap();
    assert_eq!(comparison.patches.len(), 1);
    assert!(comparison_change_labels(&comparison.patches).is_empty());
    assert!(comparison_preview_labels(&comparison, None, &[]).is_empty());
    doc.delete(&receipts, "invocation:private").unwrap();
    let hash = doc.commit().unwrap();
    let id = ui::history_source::entry_id_from_hash(&hash.to_string());
    let comparison = ui::history_source::compare_entries(&mut doc, &[id]).unwrap();
    assert_eq!(comparison.patches.len(), 1);
    assert!(comparison_change_labels(&comparison.patches).is_empty());
}

#[test]
fn readable_comparison_does_not_hide_similarly_named_document_fields() {
    let mut doc = automerge::AutoCommit::new();
    let document = doc
        .put_object(automerge::ROOT, "document", automerge::ObjType::Map)
        .unwrap();
    doc.commit();
    doc.put(&document, "tasks_v1", "Document content").unwrap();
    let hash = doc.commit().unwrap();
    let id = ui::history_source::entry_id_from_hash(&hash.to_string());
    let comparison = ui::history_source::compare_entries(&mut doc, &[id]).unwrap();
    assert_eq!(comparison_change_labels(&comparison.patches).len(), 1);
}

#[test]
fn readable_comparison_rejects_raw_summaries_and_unknown_semantic_types() {
    use tilo_actions::{Did, EditOp, FieldEdit};
    let mut context = ActionContext::for_resource("route:private", "unknown", "route:private");
    context.purpose = Some("invoke route:private".into());
    assert!(comparison_semantic_label(&context).is_none());
    context.what = Some(Did::Edit(FieldEdit {
        ty: "route:private".into(),
        field: "title".into(),
        path: Vec::new(),
        op: EditOp::Set,
        old: None,
        new: None,
        args: BTreeMap::default(),
    }));
    assert_eq!(
        comparison_semantic_label(&context),
        Some(t("activity-comparison-action-unavailable"))
    );
    if let Some(Did::Edit(edit)) = context.what.as_mut() {
        edit.ty = "task".into();
    }
    assert_eq!(
        comparison_semantic_label(&context),
        Some(t("activity-comparison-action-unavailable"))
    );
}

#[test]
fn structured_labels_reject_address_tokens_without_rejecting_short_id_titles() {
    use tilo_actions::{Did, EditOp, FieldEdit};
    for (id, label, rejected) in [
        ("invocation:private", "Receipt invocation:private", true),
        ("opaque-123", "Receipt (opaque-123)", true),
        ("other-id", "Receipt invocation:private", true),
        ("ann", "Annual report", false),
        ("ann", "Planning annual report", false),
    ] {
        let mut context = ActionContext::for_resource(id, "document", label);
        context.what = Some(Did::Edit(FieldEdit {
            ty: "document".into(),
            field: "title".into(),
            path: Vec::new(),
            op: EditOp::Set,
            old: None,
            new: None,
            args: BTreeMap::default(),
        }));
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "title", "Changed").unwrap();
        doc.commit_with(CommitOptions::default().with_message(context.change_message().unwrap()));
        let entries = HistorySource::new().entries(&mut doc);
        let detail = HistorySelectionDetail::from_range(&entries, (0, 0)).unwrap();
        let provenance = detail_provenance(&detail, &entries);
        let comparison = ui::history_source::compare_entries(&mut doc, &detail.entry_ids).unwrap();
        let preview = comparison_preview_labels(&comparison, Some(&detail), &entries).join("\n");
        let action = comparison_semantic_label(&context).unwrap();
        if rejected {
            assert!(!provenance.contains(label), "{provenance}");
            assert!(!preview.contains(label), "{preview}");
            assert_eq!(action, t("activity-comparison-action-unavailable"));
        } else {
            assert!(provenance.contains(label), "{provenance}");
            assert!(preview.contains(label), "{preview}");
            assert!(action.contains(label), "{action}");
        }
        assert_eq!(detail.resources[0].id, id);
        assert_eq!(context.resource.as_ref().unwrap().label, label);
    }
}

#[test]
fn comparison_values_use_token_boundaries_too() {
    use tilo_actions::{Did, EditOp, FieldEdit, Scalar};
    let mut context = ActionContext::for_resource("ann", "document", "Annual report");
    for (value, rejected) in [
        ("Annual report", false),
        ("Selected ann", true),
        ("Receipt invocation:private", true),
    ] {
        context.what = Some(Did::Edit(FieldEdit {
            ty: "document".into(),
            field: "title".into(),
            path: Vec::new(),
            op: EditOp::Set,
            old: None,
            new: Some(Scalar::text(value)),
            args: BTreeMap::default(),
        }));
        let label = comparison_semantic_label(&context).unwrap();
        assert_eq!(
            label == t("activity-comparison-action-unavailable"),
            rejected,
            "{label}"
        );
    }
}

#[test]
fn scroll_fade_deadline_and_animation_reach_idle() {
    let mut pane = RoomActivityPane::new();
    let start = ui::time::now();
    tilo_clock::testing::with_monotonic_time(start, || {
        pane.catchup_scroll
            .set_viewport(Rect::new(0.0, 0.0, 300.0, 200.0), &Theme::dark());
        pane.catchup_scroll.set_content_size(300.0, 800.0);
        assert!(
            pane.catchup_scroll
                .scroll_by(vello::kurbo::Vec2::new(0.0, 50.0))
        );
        assert!(pane.next_wake_deadline().is_some());
    });
    let deadline =
        tilo_clock::testing::with_monotonic_time(start, || pane.next_wake_deadline().unwrap());
    tilo_clock::testing::with_monotonic_time(deadline + Duration::from_millis(1), || {
        assert!(pane.needs_animation_frame(ui::time::now()));
    });
    tilo_clock::testing::with_monotonic_time(deadline + Duration::from_secs(5), || {
        assert!(!pane.needs_animation_frame(ui::time::now()));
        assert!(pane.next_wake_deadline().is_none());
    });
}

#[test]
fn readable_replay_source_does_not_rewrite_the_machine_address() {
    let source = super::super::index::HistoryOrigin::DesktopTree.key();
    assert_eq!(readable_replay_source(&source), "Workspace layout");
    assert_eq!(source, "workspace-tree");
    assert_eq!(
        readable_replay_source("record:private-address"),
        t("activity-readable-source-unavailable")
    );
}
