use super::{InvocationJournal, InvocationKey, InvocationOwner, InvocationUpdate};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};

fn fixture(
    root: &std::path::Path,
) -> (
    InvocationJournal,
    InvocationKey,
    OwnerCommitRequest,
    InvocationUpdate,
) {
    let mut journal = InvocationJournal::open(root).unwrap();
    let key = InvocationKey {
        room: uuid::Uuid::new_v4().to_string(),
        host: journal.host(),
        owner: InvocationOwner::Local {
            actor: "person".into(),
        },
        invocation: tilo_actions::InvocationId::new(),
    };
    let context = tilo_actions::ActionContext::default();
    let intent = serde_json::json!({"operation_kind":tilo_actions::OperationKind::Effect,
        "context":context,"actor":"person"});
    assert!(journal.admit(key.clone(), intent, 1).unwrap());
    let update = InvocationUpdate::Succeeded {
        invocation: key.invocation,
        output: tilo_actions::ActionOutput::default(),
    };
    let mut receipt = tilo_desktop_model::WorkspaceEffectReceipt::redacted(
        key.invocation,
        "person",
        context,
        tilo_actions::EffectReceiptStatus::Succeeded,
        tilo_clock::unix_now_i64(),
    );
    receipt.completed_at = 100;
    let request = OwnerCommitRequest {
        room: key.room.clone(),
        operation: CommitOperation::EffectReceipt {
            invocation: key.invocation.0,
            redacted: serde_json::to_vec(&receipt).unwrap(),
        },
    };
    (journal, key, request, update)
}

#[test]
fn prepared_effect_restart_preserves_exact_bytes_without_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("journal");
    let (mut journal, key, request, update) = fixture(&root);
    let intent = journal.lookup(&key).unwrap().unwrap().intent;
    let prepared = journal
        .prepare_effect(&key, update.clone(), &request)
        .unwrap();
    drop(journal);
    let mut journal = InvocationJournal::open(&root).unwrap();
    let record = journal.lookup(&key).unwrap().unwrap();
    assert!(record.uncertain);
    assert!(matches!(record.update, InvocationUpdate::Accepted { .. }));
    assert_eq!(record.prepared_effect.as_ref(), Some(&prepared));
    assert_eq!(
        prepared.request().binding().unwrap(),
        request.binding().unwrap()
    );
    assert!(
        !journal.admit(key.clone(), intent, 2).unwrap(),
        "recovery never issues dispatch"
    );
    let terminal = journal.finish_effect(&key, &prepared).unwrap();
    assert_eq!(terminal.update, update);
    assert!(!terminal.uncertain);
    drop(journal);
    let journal = InvocationJournal::open(&root).unwrap();
    let record = journal.lookup(&key).unwrap().unwrap();
    assert_eq!(record.prepared_effect, Some(prepared));
    assert_eq!(record.update, update);
    assert!(!record.uncertain);
}

#[test]
fn prepared_effect_conflict_and_failed_barriers_remain_uncertain() {
    for failure in [
        super::BarrierFailure::File,
        super::BarrierFailure::Directory,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let (mut journal, key, request, update) = fixture(&root);
        journal.fail_at = Some(failure);
        assert!(journal.prepare_effect(&key, update, &request).is_err());
        assert!(
            journal.lookup(&key).is_err(),
            "failed preparation poisons submission path"
        );
        drop(journal);
        let journal = InvocationJournal::open(&root).unwrap();
        assert!(journal.lookup(&key).unwrap().unwrap().uncertain);
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("journal");
    let (mut journal, key, request, update) = fixture(&root);
    let prepared = journal.prepare_effect(&key, update, &request).unwrap();
    let changed = InvocationUpdate::Cancelled {
        invocation: key.invocation,
        message: None,
    };
    assert!(journal.prepare_effect(&key, changed, &request).is_err());
    journal.fail_at = Some(super::BarrierFailure::File);
    assert!(journal.finish_effect(&key, &prepared).is_err());
    drop(journal);
    let journal = InvocationJournal::open(&root).unwrap();
    let record = journal.lookup(&key).unwrap().unwrap();
    assert!(record.uncertain);
    assert_eq!(record.prepared_effect, Some(prepared));
}

fn corrupt_prepared(
    corruption: &str,
    version: &mut serde_json::Value,
    row: &mut serde_json::Value,
) {
    match corruption {
        "version" => *version = 4.into(),
        "room" => {
            row["prepared_effect"]["room"] = uuid::Uuid::new_v4().to_string().into();
        }
        "actor" => row["intent"]["actor"] = "other".into(),
        "invocation" => {
            row["prepared_effect"]["invocation"] = uuid::Uuid::new_v4().to_string().into();
        }
        "null" => row["prepared_effect"] = serde_json::Value::Null,
        "unknown" => row["prepared_effect"]["authority"] = "fake".into(),
        "oversized" => {
            row["prepared_effect"]["redacted"] = serde_json::json!(vec![0u8; 17 * 1024]);
        }
        _ => panic!("unexpected corruption fixture"),
    }
}

const CORRUPTIONS: [&str; 7] = [
    "version",
    "room",
    "actor",
    "invocation",
    "null",
    "unknown",
    "oversized",
];

#[test]
fn prepared_effect_restore_refuses_malformed_or_downgraded_bindings() {
    for corruption in CORRUPTIONS {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let (mut journal, key, request, update) = fixture(&root);
        journal.prepare_effect(&key, update, &request).unwrap();
        drop(journal);
        // Reopening folds the appended preparation into the snapshot.
        drop(InvocationJournal::open(&root).unwrap());
        let path = root.join("records.json");
        let mut data: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut version = data["version"].take();
        corrupt_prepared(corruption, &mut version, &mut data["records"][0]);
        data["version"] = version;
        std::fs::write(path, serde_json::to_vec(&data).unwrap()).unwrap();
        assert!(InvocationJournal::open(&root).is_err(), "{corruption}");
    }
}

#[test]
fn prepared_effect_entry_refuses_malformed_or_downgraded_bindings() {
    for corruption in CORRUPTIONS {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let (mut journal, key, request, update) = fixture(&root);
        journal.prepare_effect(&key, update, &request).unwrap();
        drop(journal);
        let path = root.join("records.json");
        let bytes = std::fs::read(&path).unwrap();
        let mut lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
        assert_eq!(lines.pop(), Some(&b""[..]), "every entry is terminated");
        let mut entry: serde_json::Value =
            serde_json::from_slice(lines.pop().expect("the preparation entry")).unwrap();
        assert!(entry["record"]["prepared_effect"].is_object());
        let mut version = entry["version"].take();
        corrupt_prepared(corruption, &mut version, &mut entry["record"]);
        entry["version"] = version;
        let corrupted = serde_json::to_vec(&entry).unwrap();
        lines.push(&corrupted);
        let mut stored = lines.join(&b'\n');
        stored.push(b'\n');
        std::fs::write(path, stored).unwrap();
        assert!(InvocationJournal::open(&root).is_err(), "{corruption}");
    }
}
