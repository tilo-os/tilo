use super::*;
fn configuration(root: &Path, pin: Option<[u8; 32]>) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "version": 1, "state_root": root, "machine_id": "agent-admission",
        "transport_config": "/tmp/explicit-transport.json",
        "initial_owner_root": pin.map(hex::encode),
    }))
    .unwrap()
}
#[test]
fn cloud_https_config_is_closed_bounded_and_requires_explicit_absolute_choices() {
    let bytes = configuration(Path::new("/tmp/actual-store"), Some([7; 32]));
    let good = OperatorConfig::decode(&bytes).unwrap();
    assert_eq!(good.version, 1);
    assert_eq!(good.state_root, Path::new("/tmp/actual-store"));
    assert_eq!(good.machine_id, "agent-admission");
    assert_eq!(
        good.transport_config,
        Path::new("/tmp/explicit-transport.json")
    );
    for (key, value) in [
        ("version", serde_json::json!(2)),
        ("state_root", serde_json::json!("relative")),
        ("transport_config", serde_json::json!("relative.json")),
        ("machine_id", serde_json::json!("")),
        ("initial_owner_root", serde_json::json!("00")),
        ("initial_owner_root", serde_json::json!("0".repeat(64))),
        ("unknown", serde_json::json!(true)),
    ] {
        let mut wrong: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        wrong[key] = value;
        assert!(OperatorConfig::decode(&serde_json::to_vec(&wrong).unwrap()).is_err());
    }
    assert!(OperatorConfig::decode(&vec![b' '; 16 * 1024 + 1]).is_err());
    assert!(OperatorConfig::decode(br#"{"version":1,"version":1}"#).is_err());
}
#[test]
fn cloud_https_config_operator_pin_precedes_remote_claim_and_never_rebinds_store() {
    let (root, store, _) = crate::agent_owner_tests::enrolled();
    let before = std::fs::read(root.path().join("state.json")).unwrap();
    let owner = store.state.bound.as_ref().unwrap().root;
    for pin in [None, Some(owner)] {
        assert_eq!(
            OperatorConfig::decode(&configuration(root.path(), pin))
                .unwrap()
                .owner_root(&store)
                .unwrap(),
            owner
        );
    }
    assert!(
        OperatorConfig::decode(&configuration(root.path(), Some([9; 32])))
            .unwrap()
            .owner_root(&store)
            .is_err()
    );
    assert!(std::fs::read(root.path().join("state.json")).unwrap() == before);
    let fresh = tempfile::tempdir().unwrap();
    let unbound = crate::Store::open(fresh.path(), "agent-admission").unwrap();
    assert!(
        OperatorConfig::decode(&configuration(fresh.path(), None))
            .unwrap()
            .owner_root(&unbound)
            .is_err()
    );
    assert_eq!(
        OperatorConfig::decode(&configuration(fresh.path(), Some(owner)))
            .unwrap()
            .owner_root(&unbound)
            .unwrap(),
        owner
    );
    assert!(
        unbound.state.bound.is_none(),
        "a local pin is not enrollment"
    );
}
#[cfg(unix)]
#[test]
fn cloud_https_config_reads_only_original_private_regular_file() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("cloud.json");
    std::fs::write(&path, configuration(root.path(), None)).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(OperatorConfig::load(&path).is_ok());
    let link = root.path().join("link.json");
    symlink(&path, &link).unwrap();
    assert!(OperatorConfig::load(&link).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(OperatorConfig::load(&path).is_err());
}
