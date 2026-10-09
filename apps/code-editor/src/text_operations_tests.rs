//! File text operations exercised through the mounted app's public door.
use automerge::transaction::Transactable as _;
use serde_json::{Value, json};
use ui::framework::testing::TestApp;
use ui::tilo_actions::ActionAddress;

use crate::actions::{file_resource_id, text_digest};
use crate::doc::EditorDoc;
use crate::resource::CodeEditorShell;

fn fixture(text: &str) -> (tempfile::TempDir, TestApp<CodeEditorShell>, String) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fixture.txt");
    std::fs::write(&path, text).unwrap();
    let mut app = TestApp::<CodeEditorShell>::launched(&[path.to_str().unwrap()]).unwrap();
    app.settle();
    let file = app
        .state()
        .files
        .iter()
        .find(|file| file.name == "fixture.txt")
        .unwrap()
        .id
        .clone();
    (directory, app, file)
}

fn invoke(
    app: &mut TestApp<CodeEditorShell>,
    file: &str,
    action: &str,
    args: Value,
) -> Result<Option<ui::tilo_actions::ActionOutput>, ui::tilo_actions::DoorError> {
    let arguments = serde_json::from_value::<serde_json::Map<String, Value>>(args).unwrap();
    let instance = app.harness().component_instance_id().unwrap();
    let output = app.harness().invoke_bound_action(
        &ui::bound_dispatch::BoundActionAddress {
            instance_id: instance,
            address: ActionAddress::new(file_resource_id(file), action),
        },
        &arguments,
    )?;
    app.settle();
    Ok(output.completed())
}

fn read(app: &mut TestApp<CodeEditorShell>, file: &str, start: usize, maximum: usize) -> Value {
    invoke(
        app,
        file,
        "read-text",
        json!({"start_chars":start,"max_chars":maximum}),
    )
    .unwrap()
    .unwrap()
    .value
    .unwrap()
}

fn edit_args(read: &Value, insert: &str) -> Value {
    json!({"expected_base_digest":read["text_sha256"],"start_chars":read["start_chars"],
        "delete_chars":read["text"].as_str().unwrap().chars().count(),"insert":insert,
        "deleted_text":read["text"],"before_context":read["before_context"],"after_context":read["after_context"]})
}

fn body(app: &TestApp<CodeEditorShell>, file: &str) -> String {
    app.read_document(|doc| EditorDoc::read_buffer_key(doc, file))
        .unwrap()
}

#[test]
fn text_operations_public_read_uses_unicode_positions_and_current_identity() {
    let initial = "αβ😀γ\nfinal";
    let (_directory, mut app, file) = fixture(initial);
    let descriptor = app
        .actions()
        .unwrap()
        .into_iter()
        .find(|action| {
            action.address.action == "read-text" && action.resource_kind == "code_editor.file"
        })
        .unwrap();
    assert_eq!(
        descriptor.contract.retry,
        ui::tilo_actions::RetryPolicy::Idempotent
    );
    assert_eq!(
        descriptor
            .params
            .iter()
            .map(|param| &*param.name)
            .collect::<Vec<_>>(),
        ["start_chars", "max_chars"]
    );
    let value = read(&mut app, &file, 1, 2);
    assert_eq!(value["text"], "β😀");
    assert_eq!(value["start_chars"], 1);
    assert_eq!(value["next_chars"], 3);
    assert_eq!(value["position_units"], "unicode_scalar_values");
    assert_eq!(value["text_sha256"], text_digest(initial));
    assert_eq!(value["before_context"], "α");
    assert_eq!(value["after_context"], "γ\nfinal");
    assert_eq!(
        read(&mut app, &file, initial.chars().count(), 1)["eof"],
        true
    );
    assert!(
        invoke(
            &mut app,
            &file,
            "read-text",
            json!({"start_chars":0,"max_chars":4097})
        )
        .is_err()
    );
    assert!(
        invoke(
            &mut app,
            &file,
            "read-text",
            json!({"start_chars":0,"max_chars":0})
        )
        .is_err()
    );
    assert!(
        invoke(
            &mut app,
            &file,
            "read-text",
            json!({"start_chars":0,"max_chars":2,"file":"foreign-file"})
        )
        .is_err()
    );
}

#[test]
fn text_operations_public_edit_splices_and_preserves_framework_undo() {
    let initial = "αβ😀γ\nfinal";
    let (directory, mut app, file) = fixture(initial);
    let selected = read(&mut app, &file, 1, 2);
    invoke(&mut app, &file, "edit-text", edit_args(&selected, "X")).unwrap();
    assert_eq!(body(&app, &file), "αXγ\nfinal");
    assert_eq!(
        read(&mut app, &file, 0, 1)["text_sha256"],
        text_digest("αXγ\nfinal")
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join("fixture.txt")).unwrap(),
        initial
    );
    assert!(app.undo());
    app.settle();
    assert_eq!(body(&app, &file), initial);
    assert!(app.redo());
    app.settle();
    assert_eq!(body(&app, &file), "αXγ\nfinal");
}

#[test]
fn text_operations_public_stale_edit_rebases_only_untouched_unique_context() {
    let (_directory, mut app, file) = fixture("START middle END");
    let middle = read(&mut app, &file, 6, 6);
    let prefix = read(&mut app, &file, 0, 1);
    invoke(&mut app, &file, "edit-text", edit_args(&prefix, "prefix S")).unwrap();
    invoke(&mut app, &file, "edit-text", edit_args(&middle, "repaired")).unwrap();
    assert_eq!(body(&app, &file), "prefix START repaired END");
    assert_ne!(
        read(&mut app, &file, 0, 1)["text_sha256"],
        middle["text_sha256"]
    );
}

#[test]
fn text_operations_public_conflicting_edit_is_atomic_and_malformed_payloads_refuse() {
    let (_directory, mut app, file) = fixture("START middle END");
    let middle = read(&mut app, &file, 6, 6);
    invoke(
        &mut app,
        &file,
        "edit-text",
        edit_args(&middle, "peer-change"),
    )
    .unwrap();
    let before = app.read_document(|doc| doc.clone().save());
    assert!(
        invoke(
            &mut app,
            &file,
            "edit-text",
            edit_args(&middle, "model-change")
        )
        .is_err()
    );
    assert_eq!(app.read_document(|doc| doc.clone().save()), before);
    for invalid in [json!(true), json!(-1), json!(0.5)] {
        let mut args = edit_args(&middle, "other");
        args["start_chars"] = invalid;
        assert!(invoke(&mut app, &file, "edit-text", args).is_err());
        assert_eq!(app.read_document(|doc| doc.clone().save()), before);
    }
    let mut oversized = edit_args(&middle, &"x".repeat(16385));
    assert!(invoke(&mut app, &file, "edit-text", oversized.clone()).is_err());
    oversized["insert"] = json!("x");
    oversized["delete_chars"] = json!(1);
    assert!(invoke(&mut app, &file, "edit-text", oversized).is_err());
    assert_eq!(app.read_document(|doc| doc.clone().save()), before);
}

#[test]
fn text_operations_public_read_enforces_utf8_and_aggregate_return_bounds() {
    let (_directory, mut app, file) = fixture(&"😀".repeat(5000));
    let value = read(&mut app, &file, 0, 4096);
    assert_eq!(value["text"].as_str().unwrap().len(), 8192);
    assert_eq!(value["next_chars"], 2048);
    assert_eq!(value["eof"], false);
    assert!(serde_json::to_vec(&value).unwrap().len() <= 32768);
    assert!(
        invoke(
            &mut app,
            "foreign-file",
            "read-text",
            json!({"start_chars":0,"max_chars":1})
        )
        .is_err()
    );
}

#[test]
fn text_operations_public_ambiguous_stale_range_refuses_without_a_partial_change() {
    let (_directory, mut app, file) = fixture("L middle R");
    let original = read(&mut app, &file, 2, 6);
    invoke(
        &mut app,
        &file,
        "replace-text",
        json!({"text":"L middle R / L middle R"}),
    )
    .unwrap();
    let before = app.read_document(|doc| doc.clone().save());
    assert!(
        invoke(
            &mut app,
            &file,
            "edit-text",
            edit_args(&original, "wrong-range")
        )
        .is_err()
    );
    assert_eq!(app.read_document(|doc| doc.clone().save()), before);
}

#[test]
fn text_operations_public_viewer_can_read_but_cannot_edit() {
    let (_directory, mut app, file) = fixture("unchanged");
    app.set_role(ui::permissions::Role::Viewer);
    let selected = read(&mut app, &file, 0, 3);
    assert_eq!(selected["text"], "unc");
    let before = app.read_document(|doc| doc.clone().save());
    assert!(
        invoke(
            &mut app,
            &file,
            "edit-text",
            edit_args(&selected, "changed")
        )
        .is_err()
    );
    assert_eq!(app.read_document(|doc| doc.clone().save()), before);
}

fn invoke_low_project(
    app: &mut TestApp<CodeEditorShell>,
    action: &str,
    args: Value,
) -> Result<(), ui::tilo_actions::DoorError> {
    let resource = app
        .harness()
        .resources()
        .into_iter()
        .find(|item| item.kind == crate::resource::PROJECT_KIND)
        .unwrap()
        .id;
    let instance = app.harness().component_instance_id().unwrap();
    app.harness().invoke_bound_action(
        &ui::bound_dispatch::BoundActionAddress {
            instance_id: instance,
            address: ActionAddress::new(resource.0, action),
        },
        &serde_json::from_value(args).unwrap(),
    )?;
    app.settle();
    Ok(())
}

#[test]
fn text_operations_low_door_refuses_unaudited_default_low_and_mixed_group_atomically() {
    use crate::actions::CodeEditorAction;
    let (_directory, mut app, file) = fixture("unchanged");
    let bad = CodeEditorAction::ReplaceText {
        id: file.clone(),
        text: "bad".into(),
    };
    assert_eq!(
        ui::tilo_actions::Action::risk(&bad),
        ui::tilo_actions::RiskLevel::Low
    );
    let before = app.read_document(|doc| doc.clone().save());
    assert!(invoke_low_project(&mut app, "apply-low", json!({"action":bad})).is_err());
    let good = CodeEditorAction::EnsureBuffer {
        key: "new-state-only-buffer".into(),
        seed: "safe".into(),
    };
    assert!(
        invoke_low_project(
            &mut app,
            "group-low",
            json!({"actions":[
                {"action":"apply-low","args":{"action":good}},
                {"action":"apply","args":{"action":bad}}
            ]})
        )
        .is_err()
    );
    assert_eq!(app.read_document(|doc| doc.clone().save()), before);
    assert_eq!(body(&app, &file), "unchanged");
}

#[test]
fn text_operations_low_door_retains_write_permission_and_bounded_atomic_group() {
    use crate::actions::CodeEditorAction;
    let (_directory, mut app, _file) = fixture("initial");
    let good = CodeEditorAction::EnsureBuffer {
        key: "audited-buffer".into(),
        seed: "new".into(),
    };
    let descriptor = app
        .actions()
        .unwrap()
        .into_iter()
        .find(|item| item.address.action == "apply-low")
        .unwrap();
    assert_eq!(
        descriptor.contract.confirmation,
        ui::tilo_actions::ConfirmationPolicy::None
    );
    invoke_low_project(&mut app, "apply-low", json!({"action":good})).unwrap();
    assert_eq!(
        app.read_document(|doc| EditorDoc::read_buffer_key(doc, "audited-buffer")),
        Some("new".into())
    );
    app.set_role(ui::permissions::Role::Viewer);
    let before = app.read_document(|doc| doc.clone().save());
    assert!(invoke_low_project(&mut app, "apply-low", json!({"action":good})).is_err());
    assert_eq!(app.read_document(|doc| doc.clone().save()), before);
}

#[test]
fn text_operations_low_group_retains_contextual_refusal_and_one_undo_step() {
    let (_directory, mut app, file) = fixture("abc");
    let first = crate::actions::text_edit_action(file.clone(), "abc", "aXc");
    let second = crate::actions::text_edit_action(file.clone(), "aXc", "aX!");
    invoke_low_project(
        &mut app,
        "group-low",
        json!({"actions":[
            {"action":"apply-low","args":{"action":first}},
            {"action":"apply-low","args":{"action":second}}
        ]}),
    )
    .unwrap();
    assert_eq!(body(&app, &file), "aX!");
    assert!(app.undo());
    app.settle();
    assert_eq!(body(&app, &file), "abc");
    assert!(app.redo());
    app.settle();
    assert_eq!(body(&app, &file), "aX!");
    let before = app.read_document(|doc| doc.clone().save());
    assert!(invoke_low_project(&mut app, "apply-low", json!({"action":first})).is_err());
    assert_eq!(app.read_document(|doc| doc.clone().save()), before);
}

#[test]
fn text_operations_snapshot_public_read_stamps_canonical_record() {
    let (_directory, mut app, file) = fixture("snapshot fixture");
    let descriptor = app
        .actions()
        .unwrap()
        .into_iter()
        .find(|action| {
            action.address.action == "read-text-snapshot"
                && action.resource_kind == "code_editor.file"
        })
        .unwrap();
    assert_eq!(
        descriptor.contract.permission,
        ui::tilo_actions::PermissionRequirement::Read
    );
    let snapshot = invoke(
        &mut app,
        &file,
        "read-text-snapshot",
        json!({"start_chars":0,"max_chars":16}),
    )
    .unwrap()
    .unwrap()
    .value
    .unwrap();
    assert_eq!(snapshot["version"], 1);
    assert_eq!(snapshot["content"]["text"], "snapshot fixture");
    assert_eq!(snapshot["content"]["file"], file);
    assert_eq!(snapshot["record"]["appId"], "so.tilo.code-editor");
    assert_eq!(snapshot["record"]["recordId"], app.record().as_str());
    let heads = app.read_document(|doc| {
        crate::text_operations::committed_heads(doc, &doc.base_heads(), None).unwrap()
    });
    assert_eq!(snapshot["basis"]["heads"], json!(heads));
    assert!(snapshot.get("origin").is_none());
    assert_eq!(body(&app, &file), "snapshot fixture");
}

#[test]
fn text_operations_snapshot_rejects_real_pending_edits_and_aba_revision() {
    let (_directory, mut app, file) = fixture("before");
    let (old_heads, expected) = app.read_document(|doc| {
        let heads = doc.base_heads();
        let encoded = crate::text_operations::committed_heads(doc, &heads, None).unwrap();
        (heads, serde_json::to_string(&encoded).unwrap())
    });
    let selected = read(&mut app, &file, 0, 6);
    invoke(&mut app, &file, "edit-text", edit_args(&selected, "after")).unwrap();
    let selected = read(&mut app, &file, 0, 5);
    invoke(&mut app, &file, "edit-text", edit_args(&selected, "before")).unwrap();
    assert_eq!(body(&app, &file), "before");
    let stale = invoke(
        &mut app,
        &file,
        "read-text-snapshot",
        json!({"start_chars":0,"max_chars":6,"expected_heads":expected}),
    )
    .unwrap_err();
    assert!(stale.to_string().contains("stale_cursor"));
    let current = invoke(
        &mut app,
        &file,
        "read-text-snapshot",
        json!({"start_chars":0,"max_chars":6}),
    )
    .unwrap()
    .unwrap()
    .value
    .unwrap();
    assert_eq!(current["content"]["text"], "before");
    assert_ne!(
        current["basis"]["heads"],
        serde_json::from_str::<Value>(&expected).unwrap()
    );
    app.read_document(|doc| {
        let heads = doc.base_heads();
        assert_ne!(heads, old_heads);
        let refused = crate::text_operations::committed_heads(doc, &heads, Some(&expected));
        assert!(refused.unwrap_err().contains("revision changed"));
        let mut pending = doc.clone();
        pending
            .put(automerge::ROOT, "pending-snapshot-fixture", true)
            .unwrap();
        let count = pending.pending_ops();
        assert!(count > 0);
        assert!(
            crate::text_operations::committed_heads(&pending, &heads, None)
                .unwrap_err()
                .contains("uncommitted")
        );
        assert_eq!(pending.pending_ops(), count);
        assert_eq!(pending.base_heads(), heads);
    });
}
