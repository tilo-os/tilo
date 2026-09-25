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
        let literal = terminal_fixture(local_owner(agent), 4);
        let (_dir, root) = disk(&literal);
        let before = std::fs::read(root.join("records.json")).unwrap();
        let mut journal =
            InvocationJournal::open(&root).expect("v4 exact authenticated local owner");
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
fn authenticated_local_journal_floor_and_guarded_marker_are_independent() {
    for initial in [1, 2, 3] {
        let mut literal = terminal_fixture(json!({"Local":{"actor":"person"}}), initial);
        if initial == 3 {
            literal["records"][0]["guarded_read"] =
                json!({"version":1,"selection_digest":"11".repeat(32)});
            literal["records"][0]["update"] = json!({"state":"guarded_read_succeeded","invocation":"00000000-0000-0000-0000-000000000004","selection_digest":"11".repeat(32),"output_digest":"22".repeat(32),"basis_digest":"33".repeat(32),"output_bytes":64});
        }
        let (_dir, root) = disk(&literal);
        let mut journal = InvocationJournal::open(&root).unwrap();
        let original = journal.snapshot.records[0].key.clone();
        let mut ordinary = original.clone();
        ordinary.invocation = InvocationId(uuid::Uuid::from_u128(7));
        assert!(journal.admit(ordinary, json!({}), 2).unwrap());
        assert_eq!(journal.snapshot.version, initial);
        let mut key = original.clone();
        key.invocation = InvocationId(uuid::Uuid::from_u128(8));
        key.owner = serde_json::from_value(local_owner(Some("worker-a"))).unwrap();
        assert!(journal.admit(key.clone(), json!({}), 3).unwrap());
        assert_eq!(journal.snapshot.version, 4);
        journal.require_guarded_read(&key, [0x11; 32]).unwrap();
        assert_eq!(journal.snapshot.version, 4);
        let mut later_legacy = original.clone();
        later_legacy.invocation = InvocationId(uuid::Uuid::from_u128(9));
        later_legacy.owner = InvocationOwner::Enrolled {
            root: [1; 32],
            device: [2; 32],
            task: uuid::Uuid::from_u128(10).to_string(),
            run: uuid::Uuid::from_u128(11).to_string(),
            channel: uuid::Uuid::from_u128(12).to_string(),
        };
        assert!(journal.admit(later_legacy, json!({}), 4).unwrap());
        assert_eq!(
            journal.snapshot.version, 4,
            "a later v2 feature cannot downgrade v4"
        );
        drop(journal);
        let mut reopened = InvocationJournal::open(&root).unwrap();
        assert_eq!(reopened.snapshot.version, 4);
        let recovered = reopened.lookup(&key).unwrap().unwrap();
        assert!(recovered.uncertain);
        assert!(!reopened.admit(key, json!({}), 4).unwrap());
        let saved: Value =
            serde_json::from_slice(&std::fs::read(root.join("records.json")).unwrap()).unwrap();
        assert_eq!(saved["records"][0], literal["records"][0]);
        assert_eq!(
            saved["records"][2]["guarded_read"],
            json!({"version":1,"selection_digest":"11".repeat(32)})
        );
    }
}
#[test]
fn authenticated_local_journal_rejects_downgrade_and_ambiguous_agent() {
    let mut invalid = vec![];
    for version in [1, 2, 3] {
        invalid.push(terminal_fixture(local_owner(None), version));
    }
    let mut missing = terminal_fixture(local_owner(None), 4);
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
        invalid.push(terminal_fixture(local_owner(Some(&bad)), 4));
    }
    let mut unknown = terminal_fixture(local_owner(None), 4);
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
            terminal_fixture(local_owner(None), 4)["records"][0]["key"].clone(),
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
            assert_eq!(reopened.snapshot.version, 4);
        } else {
            assert!(reopened.lookup(&key).unwrap().is_none());
            assert_eq!(reopened.snapshot.version, 1);
        }
    }
}

#[test]
fn authenticated_local_journal_v3_decoder_never_reinterprets_owner() {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(
        dead_code,
        reason = "frozen version-three layout is decoded, not constructed"
    )]
    enum VersionThreeOwner {
        Local {
            actor: String,
        },
        Certified {
            root: [u8; 32],
            device: [u8; 32],
        },
        Enrolled {
            root: [u8; 32],
            device: [u8; 32],
            task: String,
            run: String,
            channel: String,
        },
    }
    for owner in [local_owner(None), local_owner(Some("worker-a"))] {
        assert!(serde_json::from_value::<VersionThreeOwner>(owner).is_err());
    }
    for owner in [
        json!({"Local":{"actor":"person"}}),
        json!({"Certified":{"root":vec![1;32],"device":vec![2;32]}}),
    ] {
        assert!(serde_json::from_value::<VersionThreeOwner>(owner).is_ok());
    }
    let version = terminal_fixture(local_owner(None), 4)["version"]
        .as_u64()
        .unwrap();
    assert!(
        !matches!(version, 1..=3),
        "the frozen v3 version gate rejects v4 before observation"
    );
}
