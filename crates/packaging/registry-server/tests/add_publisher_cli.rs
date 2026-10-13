//! `add-publisher` as the operator runs it: a namespace is required, and Tilo's
//! own namespace is granted only with an explicit `--first-party`.

#![allow(clippy::unwrap_used, reason = "integration-test setup and assertions")]

use std::path::Path;
use std::process::{Command, Output};

use ed25519_dalek::SigningKey;
use registry_server::{PublisherAllowlist, PublisherRow, Storage};

fn add_publisher(data_dir: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tilo-registry-server"))
        .arg("--data-dir")
        .arg(data_dir)
        .arg("add-publisher")
        .args(arguments)
        .output()
        .unwrap()
}

fn pubkey_hex(seed: u8) -> String {
    hex::encode(
        SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .to_bytes(),
    )
}

#[test]
fn add_publisher_requires_namespace() {
    let temp = tempfile::tempdir().unwrap();
    let storage = Storage::open(temp.path()).unwrap();
    let existing = PublisherRow {
        pubkey_hex: pubkey_hex(71),
        can_add_new: true,
        note: "acme".into(),
        namespaces: vec!["com.acme".into()],
    };
    storage
        .save_allowlist(&PublisherAllowlist {
            publishers: vec![existing.clone()],
        })
        .unwrap();
    let before = std::fs::read(storage.publishers_path()).unwrap();
    let newcomer = pubkey_hex(72);

    // phase: no_namespace_is_refused_and_leaves_the_allowlist_alone
    {
        let output = add_publisher(
            temp.path(),
            &[
                "--pubkey",
                &newcomer,
                "--can-add-new",
                "--note",
                "community",
            ],
        );
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("--namespace"), "{stderr}");
        assert_eq!(std::fs::read(storage.publishers_path()).unwrap(), before);
    }

    // phase: tilo_namespace_without_first_party_is_refused
    for namespace in ["so.tilo", "so.tilo.labs"] {
        let output = add_publisher(
            temp.path(),
            &[
                "--pubkey",
                &newcomer,
                "--can-add-new",
                "--namespace",
                namespace,
            ],
        );
        assert!(!output.status.success(), "{namespace}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("--first-party"), "{stderr}");
        assert_eq!(std::fs::read(storage.publishers_path()).unwrap(), before);
    }

    // phase: a_named_namespace_is_granted_exactly
    {
        let output = add_publisher(
            temp.path(),
            &["--pubkey", &newcomer, "--namespace", "io.github.newcomer"],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let allow = storage.load_allowlist().unwrap();
        assert_eq!(allow.publishers[0], existing);
        let row = &allow.publishers[1];
        assert_eq!(row.namespaces, ["io.github.newcomer"]);
        assert!(!row.owns_package_id("so.tilo.store"));
    }

    // phase: first_party_confirms_the_tilo_grant
    {
        let output = add_publisher(
            temp.path(),
            &[
                "--pubkey",
                &newcomer,
                "--can-add-new",
                "--namespace",
                "so.tilo",
                "--first-party",
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let allow = storage.load_allowlist().unwrap();
        assert_eq!(allow.publishers.len(), 2);
        let row = &allow.publishers[1];
        assert_eq!(row.namespaces, ["so.tilo"]);
        assert!(row.owns_package_id("so.tilo.store"));
    }
}
