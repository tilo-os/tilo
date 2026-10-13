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
            bots_signature_hex: String::new(),
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
        kind: registry_types::EntryKind::App,
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
            released_at_unix: 1,
            artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                tapp_url: "https://apps.tilo.so/artifacts/editor-x86_64.tapp".into(),
                tapp_root_hash_hex: hex::encode(package_id.as_bytes()),
                tapp_size_bytes: bytes.len() as u64,
                tapp_blake3_hex: digest.clone(),
                target: RegistryTarget {
                    platform: registry_types::TILO_OS_PLATFORM.into(),
                    architecture: "x86_64".into(),
                    isa: "x86_64-v3".into(),
                    runtime: None,
                    interfaces: Vec::new(),
                },
            }),
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
                    interfaces: Vec::new(),
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
            kind: registry_types::EntryKind::App,
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
                released_at_unix: 1,
                artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                    tapp_url: url.into(),
                    tapp_root_hash_hex: hex::encode(package_id.as_bytes()),
                    tapp_size_bytes: bytes.len() as u64,
                    tapp_blake3_hex: blake3_hex.into(),
                    target: RegistryTarget {
                        platform: registry_types::TILO_OS_PLATFORM.into(),
                        architecture: "x86_64".into(),
                        isa: "x86_64-v3".into(),
                        runtime: None,
                        interfaces: Vec::new(),
                    },
                }),
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

#[test]
fn thin_app_promotion_waits_for_its_exact_native_runtime() {
    let temp = tempfile::tempdir().unwrap();
    let storage = Storage::open(temp.path()).unwrap();
    let owner = SigningKey::from_bytes(&[61; 32]);
    let channel = SigningKey::from_bytes(&[62; 32]);
    let publisher_key = SigningKey::from_bytes(&[63; 32]);
    let publisher = SigningIdentity::from_signing_key(publisher_key.clone());
    let publisher_hex = hex::encode(publisher.public_key_bytes());
    let owner_hex = hex::encode(owner.verifying_key().to_bytes());
    storage
        .save_identity(&ServerIdentity {
            owner_pubkey_hex: owner_hex.clone(),
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
    let owner_path = temp.path().join("owner.seed");
    let channel_path = temp.path().join("channel.seed");
    secret(&owner_path, &owner);
    secret(&channel_path, &channel);
    let authorized = run(
        temp.path(),
        &[
            "authorize-channel",
            "--owner-key-file",
            owner_path.to_str().unwrap(),
            "--channel-pubkey",
            &hex::encode(channel.verifying_key().to_bytes()),
            "--publisher-pubkey",
            &publisher_hex,
            "--generation",
            "1",
        ],
    );
    assert!(
        authorized.status.success(),
        "{}",
        String::from_utf8_lossy(&authorized.stderr)
    );
    let runtime = "tilo-runtime/2026.07+native.00112233445566778899aabbccddeeff";
    let app = native_submission(&publisher_key, "so.tilo.editor", "x86_64", runtime, false);
    stage_native_submissions(&storage, &publisher_hex, std::slice::from_ref(&app));
    let arguments = [
        "publish-staged-first-party",
        "--channel-key-file",
        channel_path.to_str().unwrap(),
    ];
    let missing = run(temp.path(), &arguments);
    assert!(missing.status.success());
    let deferred: serde_json::Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(deferred["status"], "deferred");
    assert_eq!(deferred["published"], 0);
    assert_eq!(deferred["deferred"], 1);
    assert!(storage.load_snapshot().unwrap().is_none());
    assert!(storage.load_promotion_transaction().unwrap().is_none());
    assert!(storage.load_pending("so.tilo.editor").unwrap().is_some());
    assert!(storage.load_staged_promotion().unwrap().is_some());

    let wrong_arch = native_submission(
        &publisher_key,
        "so.tilo.runtime.arm",
        "aarch64",
        runtime,
        true,
    );
    let arm_app = native_submission(&publisher_key, "so.tilo.arm-app", "aarch64", runtime, false);
    let held_request = storage.load_pending("so.tilo.editor").unwrap().unwrap();
    stage_native_submissions(
        &storage,
        &publisher_hex,
        &[app.clone(), arm_app, wrong_arch],
    );
    let mismatched = run(temp.path(), &arguments);
    assert!(mismatched.status.success());
    let partial: serde_json::Value = serde_json::from_slice(&mismatched.stdout).unwrap();
    assert_eq!(partial["status"], "published");
    assert_eq!(partial["promoted"], 2);
    let partial_snapshot = storage.load_snapshot().unwrap().unwrap();
    registry_types::verify_snapshot(
        &partial_snapshot,
        &owner_hex,
        partial_snapshot.issued_at_unix,
    )
    .unwrap();
    assert!(
        partial_snapshot
            .entries
            .iter()
            .any(|entry| entry.package_id == "so.tilo.arm-app")
    );
    assert!(
        partial_snapshot
            .entries
            .iter()
            .any(|entry| entry.package_id == "so.tilo.runtime.arm")
    );
    assert!(
        partial_snapshot
            .entries
            .iter()
            .all(|entry| entry.package_id != "so.tilo.editor")
    );
    assert_eq!(
        storage.load_pending("so.tilo.editor").unwrap().unwrap(),
        held_request
    );
    assert!(storage.load_pending("so.tilo.arm-app").unwrap().is_none());
    assert!(
        storage
            .load_pending("so.tilo.runtime.arm")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        storage.load_staged_artifact(&app.1.blake3_hex).unwrap(),
        app.2
    );
    assert!(storage.load_promotion_transaction().unwrap().is_none());
    assert!(storage.load_staged_promotion().unwrap().is_none());

    // A provider offered in the predecessor is insufficient if this batch
    // removes it. The new ARM request must stay pending, while the exact x86
    // provider and its app can publish.
    let newly_missing = native_submission(
        &publisher_key,
        "so.tilo.new-arm-app",
        "aarch64",
        runtime,
        false,
    );
    let removed_provider = partial_snapshot
        .entries
        .iter()
        .find(|entry| entry.package_id == "so.tilo.runtime.arm")
        .unwrap()
        .clone();
    let removal = PendingSubmission {
        submission_id: "so.tilo.runtime.arm".into(),
        submitted_at_unix: 2,
        change: sign_change_request(EntryAction::Delete, removed_provider, &publisher_key),
    };
    let exact = native_submission(&publisher_key, "so.tilo.runtime", "x86_64", runtime, true);
    stage_native_submissions(
        &storage,
        &publisher_hex,
        &[app, exact, newly_missing.clone()],
    );
    storage.save_pending(&removal).unwrap();
    let mut removal_stage = storage.load_staged_promotion().unwrap().unwrap();
    removal_stage.submissions.push(removal);
    storage.save_staged_promotion(&removal_stage).unwrap();
    let complete = run(temp.path(), &arguments);
    assert!(
        complete.status.success(),
        "{}",
        String::from_utf8_lossy(&complete.stderr)
    );
    let snapshot = storage.load_snapshot().unwrap().unwrap();
    registry_types::verify_snapshot(&snapshot, &owner_hex, snapshot.issued_at_unix).unwrap();
    assert_eq!(snapshot.entries.len(), 3);
    assert!(storage.load_pending("so.tilo.editor").unwrap().is_none());
    assert!(storage.load_pending("so.tilo.runtime").unwrap().is_none());
    assert!(
        snapshot
            .entries
            .iter()
            .all(|entry| entry.package_id != "so.tilo.new-arm-app")
    );
    assert!(
        snapshot
            .entries
            .iter()
            .all(|entry| entry.package_id != "so.tilo.runtime.arm")
    );
    assert_eq!(
        storage
            .load_pending("so.tilo.new-arm-app")
            .unwrap()
            .unwrap(),
        newly_missing.0
    );
    assert_eq!(
        storage
            .load_staged_artifact(&newly_missing.1.blake3_hex)
            .unwrap(),
        newly_missing.2
    );
}

#[test]
fn selected_partial_inventory_recovers_without_consuming_deferred_requests() {
    for journal_saved in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::open(temp.path()).unwrap();
        let owner = SigningKey::from_bytes(&[71; 32]);
        let channel = SigningKey::from_bytes(&[72; 32]);
        let publisher = SigningKey::from_bytes(&[73; 32]);
        let owner_hex = hex::encode(owner.verifying_key().to_bytes());
        let publisher_hex = hex::encode(publisher.verifying_key().to_bytes());
        storage
            .save_identity(&ServerIdentity {
                owner_pubkey_hex: owner_hex.clone(),
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
                    note: "test publisher".into(),
                    namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
                }],
            })
            .unwrap();
        let owner_path = temp.path().join("owner.seed");
        let channel_path = temp.path().join("channel.seed");
        secret(&owner_path, &owner);
        secret(&channel_path, &channel);
        let authorized = run(
            temp.path(),
            &[
                "authorize-channel",
                "--owner-key-file",
                owner_path.to_str().unwrap(),
                "--channel-pubkey",
                &hex::encode(channel.verifying_key().to_bytes()),
                "--publisher-pubkey",
                &publisher_hex,
                "--generation",
                "1",
            ],
        );
        assert!(authorized.status.success());
        let absent_runtime = "tilo-runtime/2026.07+native.00000000000000000000000000000000";
        let offered_runtime = "tilo-runtime/2026.07+native.11111111111111111111111111111111";
        let held = native_submission(&publisher, "so.tilo.held", "x86_64", absent_runtime, false);
        let ready = native_submission(
            &publisher,
            "so.tilo.ready",
            "x86_64",
            offered_runtime,
            false,
        );
        let provider = native_submission(
            &publisher,
            "so.tilo.runtime",
            "x86_64",
            offered_runtime,
            true,
        );
        stage_native_submissions(
            &storage,
            &publisher_hex,
            &[held.clone(), ready.clone(), provider.clone()],
        );
        let mut selected = storage.load_staged_promotion().unwrap().unwrap();
        selected
            .submissions
            .retain(|pending| pending.submission_id != "so.tilo.held");
        selected
            .artifacts
            .retain(|artifact| artifact.submission_id != "so.tilo.held");
        // Model a crash at either durable boundary of subset selection. No
        // approval, snapshot publication, or deletion has happened yet.
        storage.save_staged_promotion(&selected).unwrap();
        if journal_saved {
            let authorization = storage.load_channel_authorization().unwrap().unwrap();
            let now = tilo_clock::unix_now();
            let mut candidate = SignedRegistrySnapshot {
                protocol: registry_types::SNAPSHOT_PROTOCOL,
                sequence: 1,
                issued_at_unix: now,
                expires_at_unix: now + 3600,
                previous_snapshot_hash_hex: String::new(),
                moderation_head_hex: String::new(),
                publishers: authorization.publishers.clone(),
                entries: vec![
                    ready.0.change.entry.clone(),
                    provider.0.change.entry.clone(),
                ],
                owner_signature_hex: String::new(),
                channel_authorization: None,
                bots_signature_hex: String::new(),
            };
            registry_types::sign_snapshot_delegated(&mut candidate, &channel, authorization)
                .unwrap();
            registry_types::verify_snapshot(&candidate, &owner_hex, now).unwrap();
            storage
                .save_promotion_transaction(&PromotionJournal {
                    publisher_pubkey_hex: publisher_hex.clone(),
                    reviewer: "test".into(),
                    reason: "recover selected subset".into(),
                    submissions: selected.submissions.clone(),
                    candidate_snapshot: candidate,
                })
                .unwrap();
        }
        let args = [
            "publish-staged-first-party",
            "--channel-key-file",
            channel_path.to_str().unwrap(),
        ];
        let recovered = run(temp.path(), &args);
        assert!(
            recovered.status.success(),
            "{}",
            String::from_utf8_lossy(&recovered.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&recovered.stdout).unwrap();
        assert_eq!(result["promoted"], 2);
        let snapshot = storage.load_snapshot().unwrap().unwrap();
        registry_types::verify_snapshot(&snapshot, &owner_hex, snapshot.issued_at_unix).unwrap();
        assert_eq!(snapshot.entries.len(), 2);
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| entry.package_id != "so.tilo.held")
        );
        assert_eq!(
            storage.load_pending("so.tilo.held").unwrap().unwrap(),
            held.0
        );
        assert_eq!(
            storage.load_staged_artifact(&held.1.blake3_hex).unwrap(),
            held.2
        );
        assert!(storage.load_pending("so.tilo.ready").unwrap().is_none());
        assert!(storage.load_pending("so.tilo.runtime").unwrap().is_none());
        assert!(storage.load_staged_promotion().unwrap().is_none());
        assert!(storage.load_promotion_transaction().unwrap().is_none());
        assert!(run(temp.path(), &args).status.success());
        assert_eq!(storage.load_snapshot().unwrap().unwrap(), snapshot);
        // The ordinary collector can reconsider the untouched held request
        // from its retained bytes, and it still cannot publish without its SDK.
        let recollected = run(
            temp.path(),
            &["collect-first-party", "--publisher-pubkey", &publisher_hex],
        );
        assert!(
            recollected.status.success(),
            "{}",
            String::from_utf8_lossy(&recollected.stderr)
        );
        let deferred = run(temp.path(), &args);
        assert!(deferred.status.success());
        let result: serde_json::Value = serde_json::from_slice(&deferred.stdout).unwrap();
        assert_eq!(result["status"], "deferred");
        assert_eq!(storage.load_snapshot().unwrap().unwrap(), snapshot);
        assert_eq!(
            storage.load_pending("so.tilo.held").unwrap().unwrap(),
            held.0
        );
        assert_eq!(
            storage.load_staged_artifact(&held.1.blake3_hex).unwrap(),
            held.2
        );
        // An item bound of one must not continually recollect the app while
        // its needed provider sorts beyond the cutoff. Publish the provider,
        // then reconsider the exact unchanged held app in the next batch.
        let restored = native_submission(
            &publisher,
            "so.tilo.runtime.held",
            "x86_64",
            absent_runtime,
            true,
        );
        stage_native_submissions(&storage, &publisher_hex, &[held.clone(), restored.clone()]);
        let collect_one = [
            "collect-first-party",
            "--publisher-pubkey",
            &publisher_hex,
            "--max-items",
            "1",
        ];
        assert!(run(temp.path(), &collect_one).status.success());
        let selected = storage.load_staged_promotion().unwrap().unwrap();
        assert_eq!(selected.submissions, vec![restored.0]);
        assert!(run(temp.path(), &args).status.success());
        assert_eq!(
            storage.load_pending("so.tilo.held").unwrap().unwrap(),
            held.0
        );
        assert!(run(temp.path(), &collect_one).status.success());
        let admitted = run(temp.path(), &args);
        assert!(
            admitted.status.success(),
            "{}",
            String::from_utf8_lossy(&admitted.stderr)
        );
        let complete = storage.load_snapshot().unwrap().unwrap();
        registry_types::verify_snapshot(&complete, &owner_hex, complete.issued_at_unix).unwrap();
        assert_eq!(complete.sequence, snapshot.sequence + 2);
        assert!(
            complete
                .entries
                .iter()
                .any(|entry| entry == &held.0.change.entry)
        );
        assert!(storage.load_pending("so.tilo.held").unwrap().is_none());
    }
}

#[test]
fn delegated_refresh_preserves_incomplete_stage_and_transaction_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let storage = Storage::open(temp.path()).unwrap();
    let owner = SigningKey::from_bytes(&[61; 32]);
    let channel = SigningKey::from_bytes(&[62; 32]);
    let publisher_key = SigningKey::from_bytes(&[63; 32]);
    let publisher = SigningIdentity::from_signing_key(publisher_key.clone());
    let publisher_hex = hex::encode(publisher.public_key_bytes());
    let owner_hex = hex::encode(owner.verifying_key().to_bytes());
    storage
        .save_identity(&ServerIdentity {
            owner_pubkey_hex: owner_hex.clone(),
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
    let owner_path = temp.path().join("owner.seed");
    let channel_path = temp.path().join("channel.seed");
    secret(&owner_path, &owner);
    secret(&channel_path, &channel);
    let authorized = run(
        temp.path(),
        &[
            "authorize-channel",
            "--owner-key-file",
            owner_path.to_str().unwrap(),
            "--channel-pubkey",
            &hex::encode(channel.verifying_key().to_bytes()),
            "--publisher-pubkey",
            &publisher_hex,
            "--generation",
            "1",
        ],
    );
    assert!(
        authorized.status.success(),
        "{}",
        String::from_utf8_lossy(&authorized.stderr)
    );
    // Start with a real signed public member, distinct from the missing ABI.
    let retained = native_submission(
        &publisher_key,
        "so.tilo.runtime.retained",
        "x86_64",
        "tilo-runtime/2026.07+native.ffeeddccbbaa99887766554433221100",
        true,
    );
    stage_native_submissions(&storage, &publisher_hex, &[retained]);
    assert!(
        run(
            temp.path(),
            &[
                "publish-staged-first-party",
                "--channel-key-file",
                channel_path.to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    let initial = run(
        temp.path(),
        &[
            "sign-snapshot",
            "--owner-key-file",
            owner_path.to_str().unwrap(),
        ],
    );
    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );
    // An authentic expired catalogue can be renewed; consumers still reject
    // those old bytes at the current time until renewal is published.
    let mut expired = storage.load_snapshot().unwrap().unwrap();
    expired.previous_snapshot_hash_hex = registry_types::snapshot_hash(&expired).unwrap();
    expired.sequence += 1;
    expired.issued_at_unix = 1;
    expired.expires_at_unix = 2;
    registry_types::sign_snapshot(&mut expired, &owner).unwrap();
    assert!(registry_types::verify_snapshot(&expired, &owner_hex, 3).is_err());
    std::fs::write(
        storage.snapshot_path(),
        serde_json::to_vec(&expired).unwrap(),
    )
    .unwrap();
    let refresh_args = [
        "sign-delegated-snapshot",
        "--channel-key-file",
        channel_path.to_str().unwrap(),
        "--valid-for-seconds",
        "3600",
    ];
    let ordinary = run(temp.path(), &refresh_args);
    assert!(
        ordinary.status.success(),
        "{}",
        String::from_utf8_lossy(&ordinary.stderr)
    );
    let original = storage.load_snapshot().unwrap().unwrap();
    registry_types::verify_snapshot(&original, &owner_hex, original.issued_at_unix).unwrap();

    let runtime = "tilo-runtime/2026.07+native.00112233445566778899aabbccddeeff";
    let app = native_submission(&publisher_key, "so.tilo.editor", "x86_64", runtime, false);
    stage_native_submissions(&storage, &publisher_hex, std::slice::from_ref(&app));
    let arguments = [
        "publish-staged-first-party",
        "--channel-key-file",
        channel_path.to_str().unwrap(),
    ];
    let missing = run(temp.path(), &arguments);
    assert!(missing.status.success());
    let deferred: serde_json::Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(deferred["status"], "deferred");
    assert_eq!(deferred["published"], 0);
    assert_eq!(deferred["deferred"], 1);
    assert_eq!(storage.load_snapshot().unwrap().unwrap(), original);
    assert!(storage.load_promotion_transaction().unwrap().is_none());
    assert!(storage.load_pending("so.tilo.editor").unwrap().is_some());
    assert!(storage.load_staged_promotion().unwrap().is_some());

    let staged = storage.load_staged_promotion().unwrap().unwrap();
    let stage_bytes = std::fs::read(temp.path().join("first-party-promotion.staged.json")).unwrap();
    let pending = storage.load_pending("so.tilo.editor").unwrap().unwrap();
    let refreshed = run(temp.path(), &refresh_args);
    assert!(
        refreshed.status.success(),
        "{}",
        String::from_utf8_lossy(&refreshed.stderr)
    );
    let current = storage.load_snapshot().unwrap().unwrap();
    registry_types::verify_snapshot(&current, &owner_hex, current.issued_at_unix).unwrap();
    assert_eq!(current.sequence, original.sequence + 1);
    assert_eq!(current.entries, original.entries);
    assert_eq!(current.publishers, original.publishers);
    assert_eq!(
        current.previous_snapshot_hash_hex,
        registry_types::snapshot_hash(&original).unwrap()
    );
    assert_eq!(storage.load_staged_promotion().unwrap().unwrap(), staged);
    assert_eq!(
        storage.load_pending("so.tilo.editor").unwrap().unwrap(),
        pending
    );
    assert_eq!(
        std::fs::read(temp.path().join("first-party-promotion.staged.json")).unwrap(),
        stage_bytes
    );
    let invalid_lifetime = run(
        temp.path(),
        &[
            "sign-delegated-snapshot",
            "--channel-key-file",
            channel_path.to_str().unwrap(),
            "--valid-for-seconds",
            "0",
        ],
    );
    assert!(!invalid_lifetime.status.success());
    assert_eq!(storage.load_snapshot().unwrap().unwrap(), current);
    assert_eq!(storage.load_staged_promotion().unwrap().unwrap(), staged);
    let mut tampered = current.clone();
    tampered.owner_signature_hex = "00".repeat(64);
    std::fs::write(
        storage.snapshot_path(),
        serde_json::to_vec(&tampered).unwrap(),
    )
    .unwrap();
    let invalid_signature = run(temp.path(), &refresh_args);
    assert!(!invalid_signature.status.success());
    assert_eq!(storage.load_snapshot().unwrap().unwrap(), tampered);
    assert_eq!(storage.load_staged_promotion().unwrap().unwrap(), staged);
    std::fs::write(
        storage.snapshot_path(),
        serde_json::to_vec(&current).unwrap(),
    )
    .unwrap();

    let authorization = storage.load_channel_authorization().unwrap().unwrap();
    let mut expired_authorization = authorization.clone();
    expired_authorization.not_before_unix = 1;
    expired_authorization.expires_at_unix = 2;
    registry_types::sign_channel_authorization(&mut expired_authorization, &owner).unwrap();
    storage
        .save_channel_authorization(&expired_authorization)
        .unwrap();
    assert!(!run(temp.path(), &refresh_args).status.success());
    assert_eq!(storage.load_snapshot().unwrap().unwrap(), current);
    assert_eq!(storage.load_staged_promotion().unwrap().unwrap(), staged);
    storage.save_channel_authorization(&authorization).unwrap();

    let journal = PromotionJournal {
        publisher_pubkey_hex: publisher_hex.clone(),
        reviewer: "test".into(),
        reason: "recovery".into(),
        submissions: staged.submissions.clone(),
        candidate_snapshot: current.clone(),
    };
    storage.save_promotion_transaction(&journal).unwrap();
    let blocked = run(temp.path(), &refresh_args);
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("active first-party promotion"));
    assert_eq!(storage.load_snapshot().unwrap().unwrap(), current);
    assert_eq!(
        storage.load_promotion_transaction().unwrap().unwrap(),
        journal
    );
    assert_eq!(storage.load_staged_promotion().unwrap().unwrap(), staged);
    storage.delete_promotion_transaction().unwrap();

    let wrong_arch = native_submission(
        &publisher_key,
        "so.tilo.runtime.arm",
        "aarch64",
        runtime,
        true,
    );
    let arm_app = native_submission(&publisher_key, "so.tilo.arm-app", "aarch64", runtime, false);
    let held_request = storage.load_pending("so.tilo.editor").unwrap().unwrap();
    stage_native_submissions(
        &storage,
        &publisher_hex,
        &[app.clone(), arm_app, wrong_arch],
    );
    let mismatched = run(temp.path(), &arguments);
    assert!(mismatched.status.success());
    let partial: serde_json::Value = serde_json::from_slice(&mismatched.stdout).unwrap();
    assert_eq!(partial["status"], "published");
    assert_eq!(partial["promoted"], 2);
    let partial_snapshot = storage.load_snapshot().unwrap().unwrap();
    registry_types::verify_snapshot(
        &partial_snapshot,
        &owner_hex,
        partial_snapshot.issued_at_unix,
    )
    .unwrap();
    assert!(
        partial_snapshot
            .entries
            .iter()
            .any(|entry| entry.package_id == "so.tilo.arm-app")
    );
    assert!(
        partial_snapshot
            .entries
            .iter()
            .any(|entry| entry.package_id == "so.tilo.runtime.arm")
    );
    assert!(
        partial_snapshot
            .entries
            .iter()
            .all(|entry| entry.package_id != "so.tilo.editor")
    );
    assert_eq!(
        storage.load_pending("so.tilo.editor").unwrap().unwrap(),
        held_request
    );
    assert!(storage.load_pending("so.tilo.arm-app").unwrap().is_none());
    assert!(
        storage
            .load_pending("so.tilo.runtime.arm")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        storage.load_staged_artifact(&app.1.blake3_hex).unwrap(),
        app.2
    );
    assert!(storage.load_promotion_transaction().unwrap().is_none());
    assert!(storage.load_staged_promotion().unwrap().is_none());
    assert_eq!(partial_snapshot.sequence, current.sequence + 1);

    let exact = native_submission(&publisher_key, "so.tilo.runtime", "x86_64", runtime, true);
    stage_native_submissions(&storage, &publisher_hex, &[app, exact]);
    let complete = run(temp.path(), &arguments);
    assert!(
        complete.status.success(),
        "{}",
        String::from_utf8_lossy(&complete.stderr)
    );
    let snapshot = storage.load_snapshot().unwrap().unwrap();
    registry_types::verify_snapshot(&snapshot, &owner_hex, snapshot.issued_at_unix).unwrap();
    assert_eq!(snapshot.entries.len(), 5);
    assert_eq!(snapshot.sequence, current.sequence + 2);
    assert!(storage.load_staged_promotion().unwrap().is_none());
    assert!(storage.load_promotion_transaction().unwrap().is_none());
    assert!(storage.load_pending("so.tilo.editor").unwrap().is_none());
    assert!(storage.load_pending("so.tilo.runtime").unwrap().is_none());
    let rollback = run(
        temp.path(),
        &[
            "rollback-snapshot",
            "--to-sequence",
            &original.sequence.to_string(),
            "--owner-key-file",
            owner_path.to_str().unwrap(),
            "--valid-for-seconds",
            "3600",
        ],
    );
    assert!(
        rollback.status.success(),
        "{}",
        String::from_utf8_lossy(&rollback.stderr)
    );
    let rolled_back = storage.load_snapshot().unwrap().unwrap();
    registry_types::verify_snapshot(&rolled_back, &owner_hex, rolled_back.issued_at_unix).unwrap();
    assert_eq!(rolled_back.sequence, snapshot.sequence + 1);
    assert_eq!(rolled_back.entries, original.entries);
    assert!(storage.load_staged_promotion().unwrap().is_none());
    assert!(storage.load_promotion_transaction().unwrap().is_none());
}

fn native_submission(
    key: &SigningKey,
    package_id: &str,
    architecture: &str,
    runtime: &str,
    provides: bool,
) -> (PendingSubmission, StagedArtifact, Vec<u8>) {
    let identity = SigningIdentity::from_signing_key(key.clone());
    let dir = tempfile::tempdir().unwrap();
    let payload = dir.path().join("payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/editor"), b"#!/bin/true\n").unwrap();
    let binding = if provides { "provides" } else { "runtime" };
    let kind = if provides { "runtime" } else { "app" };
    let isa = if architecture == "x86_64" {
        "v3"
    } else {
        "aarch64"
    };
    let entrypoint = if provides {
        ""
    } else {
        r#"[entrypoint]
bin = "bin/editor"
type = "gui""#
    };
    let manifest = Manifest::from_toml_str(&format!(
        r#"
[package]
id = "{package_id}"
version = "1.0.0"
kind = "{kind}"
{binding} = "{runtime}"
[package.isa]
{architecture} = "{isa}"
[publisher]
identity = "ed25519:{}"
signature = ""
[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
{entrypoint}
"#,
        hex::encode(identity.public_key_bytes())
    ))
    .unwrap();
    let packed = pack(&payload, &manifest, &identity).unwrap();
    let bytes = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
    let digest = blake3::hash(&bytes).to_hex().to_string();
    let target = RegistryTarget {
        platform: registry_types::TILO_OS_PLATFORM.into(),
        architecture: architecture.into(),
        isa: manifest
            .package
            .isa
            .for_arch(if architecture == "x86_64" {
                tilo_app_package::manifest::Arch::X86_64
            } else {
                tilo_app_package::manifest::Arch::AArch64
            })
            .unwrap()
            .wire_label()
            .into(),
        runtime: Some(runtime.into()),
        interfaces: Vec::new(),
    };
    let entry = RegistryEntry {
        package_id: package_id.into(),
        kind: registry_types::EntryKind::App,
        name: "Native package".into(),
        description: "Native package".into(),
        tagline: "Native package".into(),
        images: registry_types::LocalizedImages::english_icon(
            "https://apps.tilo.so/images/test.svg",
            "Test icon",
        ),
        supported_languages: vec!["en".into()],
        categories: Vec::new(),
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: hex::encode(identity.public_key_bytes()),
        versions: vec![RegistryVersion {
            version: "1.0.0".into(),
            released_at_unix: 1,
            artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                tapp_url: format!(
                    "https://apps.tilo.so/artifacts/{package_id}-{architecture}.tapp"
                ),
                tapp_root_hash_hex: packed.id().to_hex(),
                tapp_size_bytes: bytes.len() as u64,
                tapp_blake3_hex: digest.clone(),
                target: target.clone(),
            }),
        }],
        updated_at_unix: 1,
        signature_hex: String::new(),
    };
    let pending = PendingSubmission {
        submission_id: package_id.into(),
        submitted_at_unix: 1,
        change: sign_change_request(EntryAction::Add, entry, key),
    };
    let artifact = StagedArtifact {
        submission_id: package_id.into(),
        version: "1.0.0".into(),
        target: Some(target),
        blake3_hex: digest,
    };
    (pending, artifact, bytes)
}

fn stage_native_submissions(
    storage: &Storage,
    publisher: &str,
    submissions: &[(PendingSubmission, StagedArtifact, Vec<u8>)],
) {
    for (pending, artifact, bytes) in submissions {
        storage.save_pending(pending).unwrap();
        storage
            .save_staged_artifact(&artifact.blake3_hex, bytes)
            .unwrap();
    }
    storage
        .save_staged_promotion(&StagedPromotion {
            publisher_pubkey_hex: publisher.into(),
            collected_at_unix: 1,
            submissions: submissions.iter().map(|v| v.0.clone()).collect(),
            artifacts: submissions.iter().map(|v| v.1.clone()).collect(),
        })
        .unwrap();
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
