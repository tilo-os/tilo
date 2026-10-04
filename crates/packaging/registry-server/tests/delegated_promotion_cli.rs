#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::similar_names,
    clippy::too_many_lines,
    reason = "integration-test setup and assertions"
)]

use std::os::unix::fs::PermissionsExt as _;
use std::process::{Command, Output};

use ed25519_dalek::SigningKey;
use registry_server::{
    PendingSubmission, PromotionJournal, PublisherAllowlist, PublisherRow, ServerIdentity,
    StagedArtifact, StagedPromotion, Storage,
};
use registry_types::{
    EntryAction, RegistryEntry, RegistryTarget, RegistryVersion, SignedRegistrySnapshot,
    sign_change_request,
};
use tilo_app_package::Manifest;
use tilo_app_package::pack::{SigningIdentity, pack};

fn run(data_dir: &std::path::Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tilo-registry-server"))
        .arg("--data-dir")
        .arg(data_dir)
        .args(arguments)
        .output()
        .unwrap()
}

fn secret(path: &std::path::Path, key: &SigningKey) {
    std::fs::write(path, hex::encode(key.to_bytes())).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[tokio::test]
async fn networkless_publisher_reverifies_content_addressed_bytes_and_delegated_scope() {
    let temp = tempfile::tempdir().unwrap();
    let storage = Storage::open(temp.path()).unwrap();
    let owner = SigningKey::from_bytes(&[51; 32]);
    let channel = SigningKey::from_bytes(&[52; 32]);
    let publisher_key = SigningKey::from_bytes(&[53; 32]);
    let publisher = SigningIdentity::from_signing_key(publisher_key.clone());
    let owner_hex = hex::encode(owner.verifying_key().to_bytes());
    let channel_hex = hex::encode(channel.verifying_key().to_bytes());
    let publisher_hex = hex::encode(publisher.public_key_bytes());
    let identity = ServerIdentity {
        owner_pubkey_hex: owner_hex.clone(),
        name: "test".into(),
        description: "test".into(),
        base_url: "https://registry.test".into(),
    };
    storage.save_identity(&identity).unwrap();
    storage
        .save_allowlist(&PublisherAllowlist {
            publishers: vec![PublisherRow {
                pubkey_hex: publisher_hex.clone(),
                can_add_new: false,
                note: "first party".into(),
                namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
            }],
        })
        .unwrap();
    let owner_path = temp.path().join("owner.seed");
    let channel_path = temp.path().join("channel.seed");
    secret(&owner_path, &owner);
    secret(&channel_path, &channel);
    let authorization_lock = storage.lock_snapshot().unwrap();
    let authorization_dir = temp.path().to_path_buf();
    let authorization_owner_path = owner_path.clone();
    let authorization_channel_hex = channel_hex.clone();
    let authorization_publisher_hex = publisher_hex.clone();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let authorizer = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        run(
            &authorization_dir,
            &[
                "authorize-channel",
                "--owner-key-file",
                authorization_owner_path.to_str().unwrap(),
                "--channel-pubkey",
                &authorization_channel_hex,
                "--publisher-pubkey",
                &authorization_publisher_hex,
                "--generation",
                "1",
            ],
        )
    });
    started_rx.recv().unwrap();
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
    drop(authorization_lock);
    let authorized = authorizer.join().unwrap();
    assert!(
        authorized.status.success(),
        "{}",
        String::from_utf8_lossy(&authorized.stderr)
    );
    let authorized_again = run(
        temp.path(),
        &[
            "authorize-channel",
            "--owner-key-file",
            owner_path.to_str().unwrap(),
            "--channel-pubkey",
            &channel_hex,
            "--publisher-pubkey",
            &publisher_hex,
            "--generation",
            "1",
        ],
    );
    assert!(authorized_again.status.success());
    assert_eq!(authorized_again.stdout, authorized.stdout);

    let idle_publish = run(
        temp.path(),
        &[
            "publish-staged-first-party",
            "--channel-key-file",
            channel_path.to_str().unwrap(),
        ],
    );
    assert!(idle_publish.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&idle_publish.stdout).unwrap()["status"],
        "noop"
    );

    let blocking_journal = PromotionJournal {
        publisher_pubkey_hex: publisher_hex.clone(),
        reviewer: "auto".into(),
        reason: "test active transaction".into(),
        submissions: Vec::new(),
        candidate_snapshot: SignedRegistrySnapshot {
            protocol: registry_types::SNAPSHOT_PROTOCOL,
            sequence: 1,
            issued_at_unix: 1,
            expires_at_unix: 2,
            previous_snapshot_hash_hex: String::new(),
            moderation_head_hex: String::new(),
            publishers: Vec::new(),
            entries: Vec::new(),
            owner_signature_hex: "unfinished fixture".into(),
            channel_authorization: None,
        },
    };
    storage
        .save_promotion_transaction(&blocking_journal)
        .unwrap();
    let replacement = SigningKey::from_bytes(&[54; 32]);
    let replacement_hex = hex::encode(replacement.verifying_key().to_bytes());
    let blocked_rotation = run(
        temp.path(),
        &[
            "authorize-channel",
            "--owner-key-file",
            owner_path.to_str().unwrap(),
            "--channel-pubkey",
            &replacement_hex,
            "--publisher-pubkey",
            &publisher_hex,
            "--generation",
            "2",
        ],
    );
    assert!(!blocked_rotation.status.success());
    assert!(
        String::from_utf8_lossy(&blocked_rotation.stderr)
            .contains("complete or recover the active first-party promotion")
    );
    let idempotent_during_recovery = run(
        temp.path(),
        &[
            "authorize-channel",
            "--owner-key-file",
            owner_path.to_str().unwrap(),
            "--channel-pubkey",
            &channel_hex,
            "--publisher-pubkey",
            &publisher_hex,
            "--generation",
            "1",
        ],
    );
    assert!(idempotent_during_recovery.status.success());
    storage.delete_promotion_transaction().unwrap();

    let (bytes, package_id) = signed_tapp(&publisher);
    let digest = blake3::hash(&bytes).to_hex().to_string();
    let entry = RegistryEntry {
        package_id: "so.tilo.editor".into(),
        name: "Editor".into(),
        description: "Editor".into(),
        tagline: "Edit video on Tilo OS".into(),
        images: registry_types::LocalizedImages::english_icon(
            "https://apps.tilo.so/images/test-icon.svg",
            "Test icon",
        ),
        supported_languages: vec!["en".into()],
        categories: vec!["Productivity".into()],
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: publisher_hex.clone(),
        versions: vec![RegistryVersion {
            version: "1.0.0".into(),
            tapp_url: "https://apps.tilo.so/artifacts/editor-x86_64.tapp".into(),
            tapp_root_hash_hex: hex::encode(package_id.as_bytes()),
            tapp_size_bytes: bytes.len() as u64,
            released_at_unix: 1,
            tapp_blake3_hex: digest.clone(),
            target: RegistryTarget {
                platform: registry_types::TILO_OS_PLATFORM.into(),
                architecture: "x86_64".into(),
                isa: "x86_64-v3".into(),
                runtime: None,
            },
        }],
        updated_at_unix: 1,
        signature_hex: String::new(),
    };
    let pending = PendingSubmission {
        submission_id: entry.package_id.clone(),
        submitted_at_unix: 1,
        change: sign_change_request(EntryAction::Add, entry, &publisher_key),
    };
    let recovery_pending = pending.clone();
    storage.save_pending(&pending).unwrap();
    storage.save_staged_artifact(&digest, &bytes).unwrap();
    storage
        .save_staged_promotion(&StagedPromotion {
            publisher_pubkey_hex: publisher_hex.clone(),
            collected_at_unix: 1,
            submissions: vec![pending],
            artifacts: vec![StagedArtifact {
                submission_id: "so.tilo.editor".into(),
                version: "1.0.0".into(),
                target: Some(RegistryTarget {
                    platform: registry_types::TILO_OS_PLATFORM.into(),
                    architecture: "x86_64".into(),
                    isa: "x86_64-v3".into(),
                    runtime: None,
                }),
                blake3_hex: digest,
            }],
        })
        .unwrap();

    std::fs::remove_file(&owner_path).unwrap();
    let published = run(
        temp.path(),
        &[
            "publish-staged-first-party",
            "--channel-key-file",
            channel_path.to_str().unwrap(),
            "--valid-for-seconds",
            "3600",
        ],
    );
    assert!(
        published.status.success(),
        "{}",
        String::from_utf8_lossy(&published.stderr)
    );
    let snapshot = storage.load_snapshot().unwrap().unwrap();
    registry_types::verify_snapshot(&snapshot, &owner_hex, snapshot.issued_at_unix).unwrap();
    assert_eq!(snapshot.entries.len(), 1);
    assert!(snapshot.channel_authorization.is_some());
    assert!(storage.load_staged_promotion().unwrap().is_none());
    let live_authorization = snapshot.channel_authorization.clone().unwrap();
    // The channel key signs the compact form too.
    let compact = storage.load_compact_snapshot().unwrap().unwrap();
    registry_types::verify_compact_snapshot(&compact, &owner_hex, compact.issued_at_unix).unwrap();
    assert_eq!(
        compact.snapshot_hash_hex,
        registry_types::snapshot_hash(&snapshot).unwrap()
    );

    // Simulate a crash after the signed snapshot and moderation state became
    // durable and the staged manifest was consumed, but before the promotion
    // journal deletion. Recovery must not require the untrusted stage again.
    storage
        .save_promotion_transaction(&PromotionJournal {
            publisher_pubkey_hex: publisher_hex.clone(),
            reviewer: "auto".into(),
            reason: "first party".into(),
            submissions: vec![recovery_pending],
            candidate_snapshot: snapshot.clone(),
        })
        .unwrap();
    let mut expired_authorization = live_authorization.clone();
    expired_authorization.not_before_unix = 1;
    expired_authorization.expires_at_unix = 2;
    expired_authorization.owner_signature_hex.clear();
    registry_types::sign_channel_authorization(&mut expired_authorization, &owner).unwrap();
    storage
        .save_channel_authorization(&expired_authorization)
        .unwrap();
    std::fs::remove_file(&channel_path).unwrap();
    let recovered = run(
        temp.path(),
        &[
            "publish-staged-first-party",
            "--channel-key-file",
            channel_path.to_str().unwrap(),
            "--valid-for-seconds",
            "3600",
        ],
    );
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    assert!(storage.load_promotion_transaction().unwrap().is_none());
    storage
        .save_channel_authorization(&live_authorization)
        .unwrap();
    secret(&channel_path, &channel);

    let refreshed = run(
        temp.path(),
        &[
            "sign-delegated-snapshot",
            "--channel-key-file",
            channel_path.to_str().unwrap(),
            "--valid-for-seconds",
            "3600",
        ],
    );
    assert!(
        refreshed.status.success(),
        "{}",
        String::from_utf8_lossy(&refreshed.stderr)
    );
    let snapshot = storage.load_snapshot().unwrap().unwrap();
    assert_eq!(snapshot.sequence, 2);
    registry_types::verify_snapshot(&snapshot, &owner_hex, snapshot.issued_at_unix).unwrap();
    let compact = storage.load_compact_snapshot().unwrap().unwrap();
    assert_eq!(compact.sequence, 2);
    registry_types::verify_compact_snapshot(&compact, &owner_hex, compact.issued_at_unix).unwrap();

    // A higher-generation root authorization revokes the old channel at the
    // serving boundary immediately, even while its certificate is unexpired.
    secret(&owner_path, &owner);
    let rotated = run(
        temp.path(),
        &[
            "authorize-channel",
            "--owner-key-file",
            owner_path.to_str().unwrap(),
            "--channel-pubkey",
            &replacement_hex,
            "--publisher-pubkey",
            &hex::encode(publisher.public_key_bytes()),
            "--generation",
            "2",
        ],
    );
    assert!(rotated.status.success());
    let bound = registry_server::bind_and_run(
        "127.0.0.1:0".parse().unwrap(),
        registry_server::AppState::new(storage, identity),
    )
    .await
    .unwrap();
    let response = tilo_net::http::client()
        .build()
        .unwrap()
        .get(format!("http://{bound}/entries"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
}

#[test]
fn collector_reuses_staged_bytes_and_leaves_uncollectable_submissions_pending() {
    let temp = tempfile::tempdir().unwrap();
    let storage = Storage::open(temp.path()).unwrap();
    let publisher_key = SigningKey::from_bytes(&[55; 32]);
    let publisher = SigningIdentity::from_signing_key(publisher_key.clone());
    let publisher_hex = hex::encode(publisher.public_key_bytes());
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
    let (bytes, package_id) = signed_tapp(&publisher);
    let digest = blake3::hash(&bytes).to_hex().to_string();
    let submission = |id: &str, url: &str, blake3_hex: &str| {
        let entry = RegistryEntry {
            package_id: id.into(),
            name: "Editor".into(),
            description: "Editor".into(),
            tagline: "Edit video on Tilo OS".into(),
            images: registry_types::LocalizedImages::english_icon(
                "https://apps.tilo.so/images/test-icon.svg",
                "Test icon",
            ),
            supported_languages: vec!["en".into()],
            categories: vec!["Productivity".into()],
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: publisher_hex.clone(),
            versions: vec![RegistryVersion {
                version: "1.0.0".into(),
                tapp_url: url.into(),
                tapp_root_hash_hex: hex::encode(package_id.as_bytes()),
                tapp_size_bytes: bytes.len() as u64,
                released_at_unix: 1,
                tapp_blake3_hex: blake3_hex.into(),
                target: RegistryTarget {
                    platform: registry_types::TILO_OS_PLATFORM.into(),
                    architecture: "x86_64".into(),
                    isa: "x86_64-v3".into(),
                    runtime: None,
                },
            }],
            updated_at_unix: 1,
            signature_hex: String::new(),
        };
        PendingSubmission {
            submission_id: id.into(),
            submitted_at_unix: 1,
            change: sign_change_request(EntryAction::Add, entry, &publisher_key),
        }
    };
    // The advertised URL no longer serves these bytes, but the registry
    // already holds them under the digest the publisher signed.
    let collectable = submission(
        "so.tilo.editor",
        "https://apps.tilo.so/artifacts/gone/editor-x86_64.tapp",
        &digest,
    );
    let uncollectable = submission(
        "so.tilo.viewer",
        "file:///untrusted/viewer-x86_64.tapp",
        &"cc".repeat(32),
    );
    storage.save_pending(&collectable).unwrap();
    storage.save_pending(&uncollectable).unwrap();
    storage.save_staged_artifact(&digest, &bytes).unwrap();

    let collected = run(
        temp.path(),
        &["collect-first-party", "--publisher-pubkey", &publisher_hex],
    );
    assert!(!collected.status.success());
    let report: serde_json::Value = serde_json::from_slice(&collected.stdout).unwrap();
    assert_eq!(report["status"], "incomplete");
    assert_eq!(report["collected"], 1);
    assert_eq!(report["failed"][0]["submission"], "so.tilo.viewer");
    let staged = storage.load_staged_promotion().unwrap().unwrap();
    assert_eq!(staged.submissions, vec![collectable]);
    assert_eq!(staged.artifacts.len(), 1);
    assert_eq!(staged.artifacts[0].blake3_hex, digest);
    assert!(
        storage
            .load_pending("so.tilo.viewer")
            .unwrap()
            .is_some_and(|pending| pending == uncollectable)
    );
}

fn signed_tapp(identity: &SigningIdentity) -> (Vec<u8>, tilo_app_package::PackageId) {
    let dir = tempfile::tempdir().unwrap();
    let payload = dir.path().join("payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/editor"), b"#!/bin/true\n").unwrap();
    let manifest = Manifest::from_toml_str(&format!(
        r#"
[package]
id = "so.tilo.editor"
version = "1.0.0"
kind = "app"

[package.isa]
x86_64 = "v3"

[publisher]
identity = "ed25519:{}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/editor"
type = "gui"
"#,
        hex::encode(identity.public_key_bytes())
    ))
    .unwrap();
    let packed = pack(&payload, &manifest, identity).unwrap();
    (
        tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap(),
        packed.id(),
    )
}
