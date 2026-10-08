//! Retained document intent is not permission to repeat an external mutation.

use super::{FileExplorerShell, FilesDocAction};
use crate::file_ops::{FileOp, Outcome};
use ui::prelude::{Invoke, TestApp};
use ui::tilo_actions::Action;

fn inspect_operation(app: &mut TestApp<FileExplorerShell>) -> serde_json::Value {
    let address = app
        .actions()
        .unwrap()
        .into_iter()
        .find(|action| {
            action.address.resource.starts_with("files.operation:")
                && action.address.action == "inspect-file-operation"
        })
        .expect("the retained operation must publish its read action")
        .address;
    app.call_on(&address.resource, &address.action, serde_json::json!({}))
        .unwrap()
        .expect("inspection must return its actual record state")
}

#[test]
fn unsettled_folder_operation_is_not_replayed_after_native_profile_restart() {
    let folder = tempfile::tempdir().unwrap();
    let arguments = [folder.path().to_str().unwrap()];
    let original = TestApp::<FileExplorerShell>::launched(&arguments).unwrap();
    let mut retained = original.peer_document();
    let op = FileOp::NewFolder {
        parent: folder.path().display().to_string(),
        name: "Retained folder".into(),
    };
    // Retain the actual document action but lose its later settlement: the
    // native filesystem mutation completed before its record outcome committed.
    // No new action is invoked when this profile subsequently opens.
    FilesDocAction::QueueFileOp {
        id: "retained-operation".into(),
        device: "test-device".into(),
        op: op.clone(),
    }
    .apply(&mut retained)
    .unwrap();
    assert!(matches!(
        crate::file_ops::execute(&op, None),
        Outcome::Done { .. }
    ));
    drop(original);

    // Hydration installs the real native record before constructing the app.
    let mut resumed =
        TestApp::<FileExplorerShell>::launched_replica(&arguments, &mut retained).unwrap();
    resumed.settle();
    assert!(folder.path().join("Retained folder").is_dir());
    assert_eq!(
        std::fs::read_dir(folder.path()).unwrap().count(),
        1,
        "opening an unsettled record repeated its external mutation"
    );
    assert!(resumed.state().ops[0].outcome.is_none());
    let mut reopened = resumed.relaunch(&arguments).unwrap();
    reopened.settle();
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1);
    assert!(reopened.state().ops[0].outcome.is_none());
    let before = reopened.peer_document().save();
    assert!(
        reopened
            .invoke(
                Invoke::apply(&FilesDocAction::QueueFileOp {
                    id: "unknown-compensation".into(),
                    device: "test-device".into(),
                    op: FileOp::Revert {
                        op: "retained-operation".into(),
                    },
                })
                .unwrap(),
            )
            .is_err()
    );
    assert_eq!(reopened.peer_document().save(), before);
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1);
    reopened.set_role(collab_types::Role::Viewer);
    let before = reopened.peer_document().save();
    let answer = inspect_operation(&mut reopened);
    assert_eq!(answer["state"], "outcome_unknown");
    assert!(answer["outcome"].is_null());
    assert_eq!(reopened.peer_document().save(), before);
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1);
}

#[test]
fn settled_folder_history_compensates_only_new_admitted_actions() {
    let folder = tempfile::tempdir().unwrap();
    let arguments = [folder.path().to_str().unwrap()];
    let mut app = TestApp::<FileExplorerShell>::launched(&arguments).unwrap();
    app.call("new-folder", serde_json::json!({"name": "History folder"}))
        .unwrap();
    for cycle in 0..2 {
        assert!(app.undo());
        app.settle();
        assert!(
            !folder.path().join("History folder").exists(),
            "undo cycle {cycle}: {:?}",
            app.state().ops
        );
        assert!(app.redo());
        app.settle();
        assert!(folder.path().join("History folder").is_dir());
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1);
        assert!(
            app.state()
                .ops
                .iter()
                .all(|operation| { matches!(operation.outcome, Some(Outcome::Done { .. })) })
        );
    }
    let operations = app.state().ops;
    let mut reopened = app.relaunch(&arguments).unwrap();
    reopened.settle();
    assert_eq!(reopened.state().ops, operations);
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1);
}

#[test]
fn fresh_folder_operation_commits_its_outcome_before_native_profile_restart() {
    let folder = tempfile::tempdir().unwrap();
    let arguments = [folder.path().to_str().unwrap()];
    let mut app = TestApp::<FileExplorerShell>::launched(&arguments).unwrap();
    app.call("new-folder", serde_json::json!({"name": "Fresh folder"}))
        .unwrap();
    app.settle();
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1);
    let operation = app.state().ops[0].clone();
    assert!(matches!(operation.outcome, Some(Outcome::Done { .. })));
    app.flush().unwrap();
    let mut reopened = app.relaunch(&arguments).unwrap();
    reopened.settle();
    assert_eq!(reopened.state().ops, vec![operation]);
    assert_eq!(inspect_operation(&mut reopened)["state"], "done");
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1);
}

#[test]
fn denied_folder_operation_never_creates_a_pending_intent_or_folder() {
    let folder = tempfile::tempdir().unwrap();
    let mut app =
        TestApp::<FileExplorerShell>::launched(&[folder.path().to_str().unwrap()]).unwrap();
    app.set_role(collab_types::Role::Viewer);
    assert!(
        app.call("new-folder", serde_json::json!({"name": "Denied"}))
            .is_err()
    );
    app.settle();
    assert!(app.state().ops.is_empty());
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
}

#[test]
fn replicated_pending_operation_is_not_a_new_admitted_filesystem_action() {
    let folder = tempfile::tempdir().unwrap();
    let arguments = [folder.path().to_str().unwrap()];
    let mut app = TestApp::<FileExplorerShell>::launched(&arguments).unwrap();
    let mut peer = app.peer_document();
    FilesDocAction::QueueFileOp {
        id: "received-operation".into(),
        device: "test-device".into(),
        op: FileOp::NewFolder {
            parent: folder.path().display().to_string(),
            name: "Received intent".into(),
        },
    }
    .apply(&mut peer)
    .unwrap();
    app.merge_peer(&mut peer).unwrap();
    app.settle();
    assert_eq!(app.state().ops.len(), 1);
    assert_eq!(inspect_operation(&mut app)["state"], "outcome_unknown");
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
    let mut reopened = app.relaunch(&arguments).unwrap();
    reopened.settle();
    assert!(reopened.state().ops[0].outcome.is_none());
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
}

#[test]
fn late_original_outcome_does_not_admit_a_retained_unknown_compensation() {
    let folder = tempfile::tempdir().unwrap();
    let arguments = [folder.path().to_str().unwrap()];
    let original = TestApp::<FileExplorerShell>::launched(&arguments).unwrap();
    let mut retained = original.peer_document();
    let op = FileOp::NewFolder {
        parent: folder.path().display().to_string(),
        name: "Late receipt".into(),
    };
    FilesDocAction::QueueFileOp {
        id: "late-original".into(),
        device: "test-device".into(),
        op: op.clone(),
    }
    .apply(&mut retained)
    .unwrap();
    // Historical records could contain an inverse before the original outcome.
    // Construct that legacy record; the current admission path refuses it.
    crate::doc::OpsDoc::queue(
        &mut retained,
        "legacy-undo",
        "test-device",
        &FileOp::Revert {
            op: "late-original".into(),
        },
    )
    .unwrap();
    let outcome = crate::file_ops::execute(&op, None);
    assert!(matches!(outcome, Outcome::Done { .. }));
    drop(original);
    let mut app =
        TestApp::<FileExplorerShell>::launched_replica(&arguments, &mut retained).unwrap();
    app.invoke(
        Invoke::apply(&FilesDocAction::SettleFileOp {
            id: "late-original".into(),
            outcome,
        })
        .unwrap(),
    )
    .unwrap();
    app.settle();
    assert!(folder.path().join("Late receipt").is_dir());
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1);
    assert!(
        app.state()
            .ops
            .iter()
            .find(|queued| queued.id == "legacy-undo")
            .unwrap()
            .outcome
            .is_none()
    );
}
