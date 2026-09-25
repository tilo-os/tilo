use super::*;
use crate::agent_runtime_owner::{AgentControlId, operator_receipt_fixture};
#[test]
fn cloud_operator_receipt_is_exact_actual_admission_and_redacted_read_only() {
    let (root, invocation, digest, agent, task) = operator_receipt_fixture();
    let path = root.path().join("state.json");
    let before = std::fs::read(&path).unwrap();
    let result = agent_receipt(root.path(), "private-runtime", &invocation.to_string()).unwrap();
    let wire = serde_json::to_vec(&result).unwrap();
    assert!(wire.len() <= 1024);
    let value: serde_json::Value = serde_json::from_slice(&wire).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"version":1,"invocationId":invocation.to_string(),
        "phase":"admitted","agentId":agent,"taskId":task,"admissionDigest":hex::encode(digest)})
    );
    assert!(std::fs::read(&path).unwrap() == before);
    let mut store = crate::Store::open(root.path(), "private-runtime").unwrap();
    store
        .mark_agent_unknown(
            &AgentControlId::new(invocation).unwrap(),
            qos_agent_control_protocol::AgentError {
                code: qos_agent_control_protocol::AgentErrorCode::OutcomeUncertain,
                message: "private reason must never leave the operator receipt".into(),
            },
        )
        .unwrap();
    drop(store);
    let terminal = std::fs::read(&path).unwrap();
    let value = serde_json::to_value(
        agent_receipt(root.path(), "private-runtime", &invocation.to_string()).unwrap(),
    )
    .unwrap();
    assert_eq!(value["phase"], "unknown");
    assert_eq!(value["admissionDigest"], hex::encode(digest));
    assert_eq!(value.as_object().unwrap().len(), 6);
    assert!(std::fs::read(&path).unwrap() == terminal);
}
#[test]
fn cloud_operator_receipt_refuses_absent_legacy_invalid_or_corrupt_without_writes() {
    let parent = tempfile::tempdir().unwrap();
    let absent = parent.path().join("absent");
    assert!(
        agent_receipt(
            &absent,
            "private-runtime",
            &uuid::Uuid::from_u128(1).to_string()
        )
        .is_err()
    );
    assert!(!absent.exists());
    let (legacy, store, _) = crate::agent_owner_tests::enrolled();
    drop(store);
    let old = std::fs::read(legacy.path().join("state.json")).unwrap();
    assert!(
        agent_receipt(
            legacy.path(),
            "agent-admission",
            &uuid::Uuid::from_u128(1).to_string()
        )
        .is_err()
    );
    assert!(std::fs::read(legacy.path().join("state.json")).unwrap() == old);
    let (root, id, _, _, _) = operator_receipt_fixture();
    let path = root.path().join("state.json");
    let before = std::fs::read(&path).unwrap();
    for bad in [
        "1357".to_owned(),
        uuid::Uuid::nil().to_string(),
        uuid::Uuid::from_u128(999).to_string(),
    ] {
        assert!(agent_receipt(root.path(), "private-runtime", &bad).is_err());
    }
    assert!(agent_receipt(root.path(), "wrong-machine", &id.to_string()).is_err());
    assert!(std::fs::read(&path).unwrap() == before);
    std::fs::write(&path, b"{corrupt").unwrap();
    assert!(agent_receipt(root.path(), "private-runtime", &id.to_string()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"{corrupt");
}
#[test]
fn cloud_operator_receipt_has_bounded_original_store_lock_wait() {
    let (root, invocation, _, _, _) = operator_receipt_fixture();
    let store = crate::Store::open(root.path(), "private-runtime").unwrap();
    let started = std::time::Instant::now();
    assert!(agent_receipt(root.path(), "private-runtime", &invocation.to_string()).is_err());
    assert!(started.elapsed() >= std::time::Duration::from_millis(400));
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    drop(store);
    assert!(agent_receipt(root.path(), "private-runtime", &invocation.to_string()).is_ok());
}

#[cfg(unix)]
#[test]
fn cloud_operator_receipt_refuses_symlink_root_and_selected_private_v1() {
    let (root, invocation, _, _, _) = operator_receipt_fixture();
    let alias = tempfile::tempdir().unwrap();
    let link = alias.path().join("linked-owner");
    std::os::unix::fs::symlink(root.path(), &link).unwrap();
    assert!(agent_receipt(&link, "private-runtime", &invocation.to_string()).is_err());
    let (v1, mut store, device) = crate::agent_owner_tests::enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_ledger(&caller).unwrap();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    drop(store);
    let before = std::fs::read(v1.path().join("state.json")).unwrap();
    assert!(agent_receipt(v1.path(), "agent-admission", &invocation.to_string()).is_err());
    assert!(std::fs::read(v1.path().join("state.json")).unwrap() == before);
}
