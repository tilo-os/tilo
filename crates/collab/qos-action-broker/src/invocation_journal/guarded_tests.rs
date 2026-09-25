#![allow(
    clippy::needless_pass_by_value,
    reason = "fixture builders take the JSON they embed by value, like `json!`"
)]
use super::*;
use serde_json::{Value, json};
fn fixture(update: Value) -> Value {
    json!({"version":3,"host":"00000000-0000-0000-0000-000000000063","records":[{
      "key":{"room":"00000000-0000-0000-0000-000000000001","host":"00000000-0000-0000-0000-000000000063","owner":{"Local":{"actor":"person"}},"invocation":"00000000-0000-0000-0000-000000000004"},
      "intent":{},"wire_id":1,"update":update,"uncertain":false,"cancellation_requested":false,
      "guarded_read":{"version":1,"selection_digest":"11".repeat(32)} }]})
}
fn accepted() -> Value {
    json!({"state":"accepted","invocation":"00000000-0000-0000-0000-000000000004"})
}
fn terminal() -> Value {
    json!({"state":"guarded_read_succeeded","invocation":"00000000-0000-0000-0000-000000000004","selection_digest":"11".repeat(32),"output_digest":"22".repeat(32),"basis_digest":"33".repeat(32),"output_bytes":65536})
}
fn disk(value: &Value) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("journal");
    drop(InvocationJournal::open(&root).unwrap());
    std::fs::write(
        root.join("records.json"),
        serde_json::to_vec(value).unwrap(),
    )
    .unwrap();
    (dir, root)
}
#[test]
fn guarded_journal_terminal_is_unavailable_and_never_redispatched() {
    let literal = fixture(terminal());
    let (_dir, root) = disk(&literal);
    let bytes = std::fs::read(root.join("records.json")).unwrap();
    let mut journal =
        InvocationJournal::open(&root).expect("closed v3 marker loads without authority");
    let key: InvocationKey = serde_json::from_value(literal["records"][0]["key"].clone()).unwrap();
    let row = journal.lookup(&key).unwrap().unwrap();
    assert!(
        matches!(row.update,InvocationUpdate::Failed{ref error,..} if error.code=="action_read_payload_unavailable" && !error.retryable && error.details.is_none())
    );
    assert!(!row.uncertain);
    assert!(!journal.admit(key.clone(), json!({}), 2).unwrap());
    assert!(
        journal
            .admit(key.clone(), json!({"changed":true}), 2)
            .is_err()
    );
    assert!(journal.cancel(&key).is_err());
    assert_eq!(bytes, std::fs::read(root.join("records.json")).unwrap());
    drop(journal);
    let journal = InvocationJournal::open(&root).unwrap();
    assert!(
        matches!(journal.lookup(&key).unwrap().unwrap().update,InvocationUpdate::Failed{error,..} if error.code=="action_read_payload_unavailable")
    );
}
#[test]
fn guarded_journal_pending_recovery_preserves_requirement() {
    let value = fixture(accepted());
    let (_dir, root) = disk(&value);
    let mut journal = InvocationJournal::open(&root).expect("guarded pending admitted row");
    let key: InvocationKey = serde_json::from_value(value["records"][0]["key"].clone()).unwrap();
    let row = journal.lookup(&key).unwrap().unwrap();
    assert!(row.uncertain);
    assert!(
        matches!(row.update,InvocationUpdate::Failed{error,..} if error.code=="action_outcome_unknown")
    );
    assert!(!journal.admit(key, json!({}), 2).unwrap());
    drop(journal);
    let saved: Value =
        serde_json::from_slice(&std::fs::read(root.join("records.json")).unwrap()).unwrap();
    assert_eq!(saved["version"], 3);
    assert_eq!(
        saved["records"][0]["guarded_read"],
        value["records"][0]["guarded_read"]
    );
    assert!(
        saved["records"][0]["update"]["error"]
            .get("details")
            .is_none()
    );
    assert!(InvocationJournal::open(&root).is_ok());
}
#[test]
fn guarded_journal_v3_enrolled_write_cannot_downgrade_or_forget_marker() {
    let value = fixture(terminal());
    let (_dir, root) = disk(&value);
    let mut journal = InvocationJournal::open(&root).expect("v3 baseline");
    let mut key: InvocationKey =
        serde_json::from_value(value["records"][0]["key"].clone()).unwrap();
    key.invocation = InvocationId(uuid::Uuid::from_u128(9));
    key.owner = InvocationOwner::Enrolled {
        root: [1; 32],
        device: [2; 32],
        task: uuid::Uuid::from_u128(10).to_string(),
        run: uuid::Uuid::from_u128(11).to_string(),
        channel: uuid::Uuid::from_u128(12).to_string(),
    };
    assert!(journal.admit(key, json!({}), 2).unwrap());
    drop(journal);
    let saved: Value =
        serde_json::from_slice(&std::fs::read(root.join("records.json")).unwrap()).unwrap();
    assert_eq!(saved["version"], 3);
    assert_eq!(saved["records"][0], value["records"][0]);
}
#[test]
fn guarded_journal_strict_presence_and_guarded_update_shape() {
    let base = fixture(terminal());
    let mut invalid = vec![];
    for version in [1, 2] {
        let mut v = base.clone();
        v["version"] = version.into();
        invalid.push(v);
    }
    for version in [1, 2, 3] {
        let mut v = fixture(accepted());
        v["version"] = version.into();
        v["records"][0]["guarded_read"] = Value::Null;
        invalid.push(v);
    }
    let mut v = base.clone();
    v["records"][0]
        .as_object_mut()
        .unwrap()
        .remove("guarded_read");
    invalid.push(v);
    for field in ["selection_digest", "output_digest", "basis_digest"] {
        for bad in ["aa".repeat(31), "AA".repeat(32), "gg".repeat(32)] {
            let mut v = base.clone();
            v["records"][0]["update"][field] = bad.into();
            invalid.push(v);
        }
    }
    for size in [0, 65537] {
        let mut v = base.clone();
        v["records"][0]["update"]["output_bytes"] = size.into();
        invalid.push(v);
    }
    let mut v = base.clone();
    v["records"][0]["update"]["selection_digest"] = "44".repeat(32).into();
    invalid.push(v);
    let mut v = base;
    v["records"][0]["update"]["output"] = json!({"value":"must not be retained"});
    invalid.push(v);
    let mut v = fixture(accepted());
    v["records"][0]["update"]["secret"] = "hidden".into();
    invalid.push(v);
    for error in [
        json!({"code":"action_read_unavailable","message":"The document read is unavailable.","retryable":false,"details":null}),
        json!({"code":"action_read_unavailable","message":"secret","retryable":false}),
        json!({"code":"action_read_unavailable","message":"The document read is unavailable.","retryable":false,"extra":"secret"}),
    ] {
        invalid.push(fixture(json!({"state":"failed","invocation":"00000000-0000-0000-0000-000000000004","error":error})));
    }
    invalid.push(fixture(json!({"state":"succeeded","invocation":"00000000-0000-0000-0000-000000000004","output":{"value":"secret"}})));
    for (i, v) in invalid.iter().enumerate() {
        let (_dir, root) = disk(v);
        assert!(
            InvocationJournal::open(&root).is_err(),
            "invalid case {i} admitted"
        );
    }
}

#[test]
fn guarded_journal_real_requirement_and_terminal_barriers_poison_without_redispatch() {
    for terminal_stage in [false, true] {
        for failure in [BarrierFailure::File, BarrierFailure::Directory] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("journal");
            let mut journal = InvocationJournal::open(&root).unwrap();
            let mut key: InvocationKey =
                serde_json::from_value(fixture(accepted())["records"][0]["key"].clone()).unwrap();
            key.host = journal.host();
            assert!(journal.admit(key.clone(), json!({}), 1).unwrap());
            if terminal_stage {
                journal.require_guarded_read(&key, [0x11; 32]).unwrap();
            }
            journal.fail_at = Some(failure);
            let outcome = if terminal_stage {
                journal.complete_guarded_read(&key, [0x11; 32], [0x22; 32], [0x33; 32], 64)
            } else {
                journal.require_guarded_read(&key, [0x11; 32])
            };
            assert!(outcome.is_err());
            assert!(journal.lookup(&key).is_err());
            assert!(journal.admit(key.clone(), json!({}), 2).is_err());
            drop(journal);
            let mut reopened = InvocationJournal::open(&root).unwrap();
            assert!(!reopened.admit(key.clone(), json!({}), 2).unwrap());
            let row = reopened.lookup(&key).unwrap().unwrap();
            let saved: Value =
                serde_json::from_slice(&std::fs::read(root.join("records.json")).unwrap()).unwrap();
            if terminal_stage && failure == BarrierFailure::Directory {
                assert!(!row.uncertain);
                assert_eq!(
                    saved["records"][0]["update"]["state"],
                    "guarded_read_succeeded"
                );
                assert!(
                    matches!(row.update,InvocationUpdate::Failed{error,..} if error.code=="action_read_payload_unavailable")
                );
            } else {
                assert!(row.uncertain);
                assert!(
                    matches!(row.update,InvocationUpdate::Failed{error,..} if error.code=="action_outcome_unknown")
                );
            }
        }
    }
}
#[test]
fn guarded_journal_real_writer_is_inert_and_never_accepts_ordinary_payload() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("journal");
    let mut journal = InvocationJournal::open(&root).unwrap();
    let mut key: InvocationKey =
        serde_json::from_value(fixture(accepted())["records"][0]["key"].clone()).unwrap();
    key.host = journal.host();
    assert!(journal.admit(key.clone(), json!({}), 1).unwrap());
    journal.require_guarded_read(&key, [0x11; 32]).unwrap();
    assert!(journal.require_guarded_read(&key, [0x44; 32]).is_err());
    assert!(
        journal
            .update(
                &key,
                InvocationUpdate::Succeeded {
                    invocation: key.invocation,
                    output: tilo_actions::ActionOutput {
                        value: Some(json!("SECRET_READ_RESULT")),
                        resources: vec![]
                    }
                },
                false
            )
            .is_err()
    );
    assert!(
        journal
            .complete_guarded_read(&key, [0x44; 32], [0x22; 32], [0x33; 32], 64)
            .is_err()
    );
    for bytes in [0, 65537] {
        assert!(
            journal
                .complete_guarded_read(&key, [0x11; 32], [0x22; 32], [0x33; 32], bytes)
                .is_err()
        );
    }
    journal
        .complete_guarded_read(&key, [0x11; 32], [0x22; 32], [0x33; 32], 64)
        .unwrap();
    assert!(
        journal
            .complete_guarded_read(&key, [0x11; 32], [0x22; 32], [0x33; 32], 64)
            .is_err()
    );
    let stored = std::fs::read_to_string(root.join("records.json")).unwrap();
    assert!(!stored.contains("SECRET_READ_RESULT"));
    assert!(!stored.contains("action_read_payload_unavailable"));
    assert!(stored.contains("guarded_read_succeeded"));
    let row = journal.lookup(&key).unwrap().unwrap();
    assert!(
        matches!(row.update,InvocationUpdate::Failed{error,..} if error.code=="action_read_payload_unavailable")
    );
}
#[test]
fn guarded_journal_duplicate_unknown_and_oversized_updates_do_not_hide_payload() {
    let value = fixture(terminal());
    let bytes = serde_json::to_string(&value).unwrap();
    for invalid in [
        bytes.replace(
            "\"output_bytes\":65536",
            "\"output_bytes\":1,\"output_bytes\":65536",
        ),
        bytes.replace("\"version\":1", "\"version\":1,\"version\":1"),
        bytes.replace(
            "\"state\":\"guarded_read_succeeded\"",
            "\"state\":\"guarded_read_succeeded\",\"state\":\"guarded_read_succeeded\"",
        ),
    ] {
        let (_dir, root) = disk(&value);
        std::fs::write(root.join("records.json"), invalid).unwrap();
        assert!(InvocationJournal::open(&root).is_err());
    }
    for target in ["update", "guarded_read"] {
        let mut invalid = value.clone();
        invalid["records"][0][target]["extra"] = Value::String("x".repeat(100_000));
        let (_dir, root) = disk(&invalid);
        let Err(error) = InvocationJournal::open(&root) else {
            panic!("oversized guarded metadata accepted")
        };
        assert!(error.contains("preflight"), "{error}");
    }
    let mut invalid = fixture(accepted());
    invalid["records"][0]["update"]["invocation"] = "00000000000000000000000000000004".into();
    let (_dir, root) = disk(&invalid);
    assert!(InvocationJournal::open(&root).is_err());
}
#[test]
fn guarded_journal_ordinary_public_decoder_behavior_stays_legacy() {
    let mut value = fixture(
        json!({"state":"succeeded","invocation":"00000000-0000-0000-0000-000000000004","output":{"value":"legacy","unknown":"preserved public permissiveness"},"unknown":"permitted"}),
    );
    value["version"] = 1.into();
    value["records"][0]
        .as_object_mut()
        .unwrap()
        .remove("guarded_read");
    let (_dir, root) = disk(&value);
    let journal = InvocationJournal::open(&root).unwrap();
    let key: InvocationKey = serde_json::from_value(value["records"][0]["key"].clone()).unwrap();
    assert!(matches!(
        journal.lookup(&key).unwrap().unwrap().update,
        InvocationUpdate::Succeeded { .. }
    ));
    drop(journal);
    let raw = serde_json::to_string(&value).unwrap().replace(
        "\"value\":\"legacy\"",
        "\"value\":\"legacy\",\"value\":\"duplicate\"",
    );
    std::fs::write(root.join("records.json"), raw).unwrap();
    assert!(
        InvocationJournal::open(&root).is_err(),
        "ordinary typed output duplicate was formerly rejected"
    );
    let mut guarded = fixture(
        json!({"state":"failed","invocation":"00000000-0000-0000-0000-000000000004","error":{"code":"action_read_unavailable","message":"The document read is unavailable.","retryable":false}}),
    );
    let (_dir, root) = disk(&guarded);
    assert!(InvocationJournal::open(&root).is_ok());
    guarded["records"][0]["update"]["error"]["retryable"] = true.into();
    let (_dir, root) = disk(&guarded);
    assert!(InvocationJournal::open(&root).is_err());
}

#[test]
fn guarded_journal_terminal_reopen_requires_observed_file_directory_barrier() {
    let value = fixture(terminal());
    let (_dir, root) = disk(&value);
    let called = std::cell::Cell::new(false);
    let result = InvocationJournal::open_with_barriers(
        &root,
        |_| Ok(()),
        |observed, directory| {
            called.set(true);
            assert!(directory.metadata().unwrap().is_dir());
            assert_eq!(
                directory.metadata().unwrap().ino(),
                std::fs::metadata(&root).unwrap().ino()
            );
            assert_eq!(
                observed.metadata().unwrap().len(),
                std::fs::metadata(root.join("records.json")).unwrap().len()
            );
            Err(std::io::Error::other(
                "injected guarded reconciliation failure",
            ))
        },
    );
    assert!(
        called.get(),
        "existing guarded terminal skipped reconciliation"
    );
    assert!(result.is_err());
    let journal = InvocationJournal::open(&root).unwrap();
    let key: InvocationKey = serde_json::from_value(value["records"][0]["key"].clone()).unwrap();
    assert!(
        matches!(journal.lookup(&key).unwrap().unwrap().update, InvocationUpdate::Failed { error, .. } if error.code == "action_read_payload_unavailable")
    );
}

#[test]
fn guarded_journal_uncertainty_and_cancellation_flags_are_exact() {
    let failed = json!({"state":"failed","invocation":"00000000-0000-0000-0000-000000000004","error":{"code":"action_read_unavailable","message":"The document read is unavailable.","retryable":false}});
    let unknown =
        serde_json::to_value(super::unknown(InvocationId(uuid::Uuid::from_u128(4)))).unwrap();
    for update in [accepted(), terminal(), failed, unknown] {
        let expected_uncertain = update["error"]["code"] == "action_outcome_unknown";
        for (uncertain, cancelled) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut value = fixture(update.clone());
            value["records"][0]["uncertain"] = uncertain.into();
            value["records"][0]["cancellation_requested"] = cancelled.into();
            let (_dir, root) = disk(&value);
            assert_eq!(
                InvocationJournal::open(&root).is_ok(),
                !cancelled && uncertain == expected_uncertain,
                "{value}"
            );
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("journal");
    let mut journal = InvocationJournal::open(&root).unwrap();
    let mut key: InvocationKey =
        serde_json::from_value(fixture(accepted())["records"][0]["key"].clone()).unwrap();
    key.host = journal.host();
    journal.admit(key.clone(), json!({}), 1).unwrap();
    journal.require_guarded_read(&key, [0x11; 32]).unwrap();
    assert!(journal.cancel(&key).is_err());
    assert!(
        journal
            .update(&key, super::unknown(key.invocation), false)
            .is_err()
    );
    journal
        .update(&key, super::unknown(key.invocation), true)
        .unwrap();
}

fn duplicate_extent_must_fail_preflight(target: &str) {
    let value = fixture(accepted());
    let small = serde_json::to_string(&value["records"][0][target]).unwrap();
    for extra_bytes in [2 * 1024 * 1024, 0] {
        let mut first = value["records"][0][target].clone();
        if extra_bytes != 0 {
            first["extra"] = Value::String("x".repeat(extra_bytes));
        }
        let field = format!("\"{target}\":{small}");
        let duplicate = format!(
            "\"{target}\":{},\"{target}\":{small}",
            serde_json::to_string(&first).unwrap()
        );
        let raw = serde_json::to_string(&value)
            .unwrap()
            .replacen(&field, &duplicate, 1);
        assert!(raw.contains(&duplicate));
        let (_dir, root) = disk(&value);
        std::fs::write(root.join("records.json"), raw).unwrap();
        let mut file = File::open(root.join("records.json")).unwrap();
        assert!(
            guarded::preflight(&mut file).is_err(),
            "duplicate {target} with first extent {extra_bytes} reached owned decoding"
        );
        let Err(error) = InvocationJournal::open(&root) else {
            panic!("duplicate {target} was admitted")
        };
        assert!(error.contains("preflight"), "{error}");
    }
}

#[test]
fn guarded_journal_duplicate_extent_update_is_rejected_before_owned_decode() {
    duplicate_extent_must_fail_preflight("update");
}

#[test]
fn guarded_journal_duplicate_extent_requirement_is_rejected_before_owned_decode() {
    duplicate_extent_must_fail_preflight("guarded_read");
}
