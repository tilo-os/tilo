#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "integration-test setup and assertions"
)]

use std::process::{Command, Output};

use ed25519_dalek::SigningKey;
use registry_server::{PublisherAllowlist, PublisherRow, ServerIdentity, Storage};

fn run(data_dir: &std::path::Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tilo-registry-server"))
        .arg("--data-dir")
        .arg(data_dir)
        .arg("collect-first-party")
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn collect_first_party_noop_and_bounds_are_enforced_without_a_signing_key() {
    let temp = tempfile::tempdir().unwrap();
    let storage = Storage::open(temp.path()).unwrap();
    let owner = SigningKey::from_bytes(&[41; 32]);
    let publisher = SigningKey::from_bytes(&[42; 32]);
    let owner_hex = hex::encode(owner.verifying_key().to_bytes());
    let publisher_hex = hex::encode(publisher.verifying_key().to_bytes());
    storage
        .save_identity(&ServerIdentity {
            owner_pubkey_hex: owner_hex,
            name: "test".into(),
            description: "test".into(),
            base_url: "https://registry.test".into(),
        })
        .unwrap();
    storage
        .save_allowlist(&PublisherAllowlist {
            publishers: vec![PublisherRow {
                pubkey_hex: publisher_hex.clone(),
                can_add_new: true,
                note: "first party".into(),
                namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
            }],
        })
        .unwrap();
    let success = run(
        temp.path(),
        &["--publisher-pubkey", &publisher_hex, "--max-items", "50"],
    );
    assert!(
        success.status.success(),
        "{}",
        String::from_utf8_lossy(&success.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&success.stdout).unwrap();
    assert_eq!(result["status"], "noop");
    assert_eq!(result["collected"], 0);
    assert!(storage.load_snapshot().unwrap().is_none());
    assert!(storage.load_promotion_transaction().unwrap().is_none());

    for invalid in ["0", "101"] {
        let rejected = run(
            temp.path(),
            &["--publisher-pubkey", &publisher_hex, "--max-items", invalid],
        );
        assert!(!rejected.status.success(), "max-items={invalid} must fail");
        assert!(
            String::from_utf8_lossy(&rejected.stderr)
                .contains("--max-items must be between 1 and 100")
        );
    }
    for invalid in ["0", "9"] {
        let rejected = run(
            temp.path(),
            &[
                "--publisher-pubkey",
                &publisher_hex,
                "--parallelism",
                invalid,
            ],
        );
        assert!(
            !rejected.status.success(),
            "parallelism={invalid} must fail"
        );
        assert!(
            String::from_utf8_lossy(&rejected.stderr)
                .contains("--parallelism must be between 1 and 8")
        );
    }
}
