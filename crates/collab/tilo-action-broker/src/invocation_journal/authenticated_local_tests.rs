#![allow(
    clippy::needless_pass_by_value,
    reason = "fixture builders take the JSON they embed by value, like `json!`"
)]
use super::*;
use serde_json::{Value, json};

fn local_owner(agent: Option<&str>) -> Value {
    json!({"AuthenticatedLocal":{"root":vec![1;32],"device":vec![2;32],"agent":agent}})
}
fn terminal_fixture(owner: Value, version: u32) -> Value {
    json!({"version":version,"host":"00000000-0000-0000-0000-000000000063","records":[{
      "key":{"room":"00000000-0000-0000-0000-000000000001","host":"00000000-0000-0000-0000-000000000063","owner":owner,"invocation":"00000000-0000-0000-0000-000000000004"},
      "intent":{"request":{"action":"retained"}},"wire_id":1,
      "update":{"state":"succeeded","invocation":"00000000-0000-0000-0000-000000000004","output":{"value":"retained result"}},
      "uncertain":false,"cancellation_requested":false}]})
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
fn authenticated_local_journal_reopen_preserves_exact_owner_and_terminal() {
    for agent in [None, Some("worker-a")] {
        let literal = terminal_fixture(local_owner(agent), VERSION);
        let (_dir, root) = disk(&literal);
        let before = std::fs::read(root.join("records.json")).unwrap();
        let mut journal =
            InvocationJournal::open(&root).expect("the store's own authenticated local owner");
        let key: InvocationKey =
            serde_json::from_value(literal["records"][0]["key"].clone()).unwrap();
        let row = journal.lookup(&key).unwrap().unwrap();
        assert!(
            matches!(&row.update,InvocationUpdate::Succeeded{output,..} if output.value==Some(json!("retained result")))
        );
        assert!(!row.uncertain);
        assert!(
            !journal
                .admit(key.clone(), literal["records"][0]["intent"].clone(), 2)
                .unwrap()
        );
        let mut owners = vec![
            json!({"Local":{"actor":"person"}}),
            json!({"Certified":{"root":vec![1;32],"device":vec![2;32]}}),
            json!({"Enrolled":{"root":vec![1;32],"device":vec![2;32],
                "task":"00000000-0000-0000-0000-000000000005",
                "run":"00000000-0000-0000-0000-000000000006",
                "channel":"00000000-0000-0000-0000-000000000007"}}),
            local_owner(Some("worker-b")),
        ];
        owners.push(local_owner(if agent.is_some() {
            None
        } else {
            Some("worker-a")
        }));
        let mut other_device = local_owner(agent);
        other_device["AuthenticatedLocal"]["device"] = json!(vec![3; 32]);
        owners.push(other_device);
        for owner in owners {
            let mut foreign = key.clone();
            foreign.owner = serde_json::from_value(owner).unwrap();
            assert!(journal.lookup(&foreign).unwrap().is_none());
        }
        assert_eq!(before, std::fs::read(root.join("records.json")).unwrap());
    }
}
#[test]
fn authenticated_local_journal_rejects_downgrade_and_ambiguous_agent() {
    let mut invalid = vec![];
    for version in [1, 2, 3, 4, 5, 6] {
        invalid.push(terminal_fixture(local_owner(None), version));
    }
    let mut missing = terminal_fixture(local_owner(None), VERSION);
    missing["records"][0]["key"]["owner"]["AuthenticatedLocal"]
        .as_object_mut()
        .unwrap()
        .remove("agent");
    invalid.push(missing);
    for bad in [
        String::new(),
        "x".repeat(513),
        "agent\nother".to_string(),
        "agent\0other".to_string(),
    ] {
        invalid.push(terminal_fixture(local_owner(Some(&bad)), VERSION));
    }
    let mut unknown = terminal_fixture(local_owner(None), VERSION);
    unknown["records"][0]["key"]["owner"]["AuthenticatedLocal"]["role"] = json!("owner");
    invalid.push(unknown);
    for value in invalid {
        let (_dir, root) = disk(&value);
        assert!(InvocationJournal::open(&root).is_err());
    }
}
#[test]
fn authenticated_local_journal_failed_upgrade_never_returns_dispatch_permission() {
    for failure in [BarrierFailure::File, BarrierFailure::Directory] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let mut journal = InvocationJournal::open(&root).unwrap();
        let mut key: InvocationKey = serde_json::from_value(
            terminal_fixture(local_owner(None), VERSION)["records"][0]["key"].clone(),
        )
        .unwrap();
        key.host = journal.host();
        journal.fail_at = Some(failure);
        assert!(journal.admit(key.clone(), json!({}), 1).is_err());
        assert!(journal.admit(key.clone(), json!({}), 2).is_err());
        drop(journal);
        let mut reopened = InvocationJournal::open(&root).unwrap();
        if failure == BarrierFailure::Directory {
            let row = reopened.lookup(&key).unwrap().unwrap();
            assert!(row.uncertain);
            assert!(!reopened.admit(key, json!({}), 2).unwrap());
            assert_eq!(reopened.snapshot.version, VERSION);
        } else {
            assert!(reopened.lookup(&key).unwrap().is_none());
            assert_eq!(reopened.snapshot.version, VERSION);
        }
    }
}
