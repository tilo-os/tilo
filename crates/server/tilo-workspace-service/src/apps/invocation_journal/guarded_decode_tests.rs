#![allow(
    clippy::needless_pass_by_value,
    reason = "fixture builders take the JSON they embed by value, like `json!`"
)]
//! A store holding a guarded read, which no build writes: what opening one
//! makes of it, and what it keeps on disk.
use super::*;
use serde_json::{Value, json};

fn fixture(update: Value) -> Value {
    json!({"version":7,"host":"00000000-0000-0000-0000-000000000063","records":[{
      "key":{"workspace":"00000000-0000-0000-0000-000000000001","host":"00000000-0000-0000-0000-000000000063","owner":{"Local":{"actor":{"person":"person"}}},"invocation":"00000000-0000-0000-0000-000000000004"},
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

tilo_test_env::scenario! {
    fn a_guarded_read_is_never_redispatched_and_keeps_its_requirement;

    phase guarded_journal_terminal_is_unavailable_and_never_redispatched;
    {
        let literal = fixture(terminal());
        let (_dir, root) = disk(&literal);
        let bytes = std::fs::read(root.join("records.json")).unwrap();
        let mut journal =
            InvocationJournal::open(&root).expect("closed v3 marker loads without authority");
        let key: InvocationKey =
            serde_json::from_value(literal["records"][0]["key"].clone()).unwrap();
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
        assert!(journal.record_cancellation(&key).is_err());
        assert_eq!(bytes, std::fs::read(root.join("records.json")).unwrap());
        drop(journal);
        let journal = InvocationJournal::open(&root).unwrap();
        assert!(
            matches!(journal.lookup(&key).unwrap().unwrap().update,InvocationUpdate::Failed{error,..} if error.code=="action_read_payload_unavailable")
        );
    }

    phase guarded_journal_pending_recovery_preserves_requirement;
    {
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
        assert_eq!(saved["version"], 7);
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
}
