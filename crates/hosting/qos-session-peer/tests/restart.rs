use qos_session_peer::SessionOwner;
use std::{fs, process::Command};

#[test]
fn restart_preserves_canonical_state_and_journal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    owner
        .queue_pending("action-42", b"canonical invocation", true)
        .unwrap();
    let before = owner.status();
    assert_eq!(before.records.len(), 3);
    assert!(before.outgoing > 0);
    assert!(SessionOwner::open(&path).is_err());
    drop(owner);
    let restored = SessionOwner::open(&path).unwrap();
    assert_eq!(restored.status(), before);
    assert_eq!(restored.pending()[0].outcome_unknown, true);
}

#[test]
fn corrupt_or_missing_state_never_reseeds_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    assert!(SessionOwner::open(&path).is_err());
    assert!(!path.exists());
    drop(SessionOwner::initialize(&path).unwrap());
    let root = fs::read(path.join("identity/user-identity/privkey.bin")).unwrap();
    fs::write(path.join("session.bin"), b"interrupted corrupt bytes").unwrap();
    assert!(SessionOwner::open(&path).is_err());
    assert!(SessionOwner::initialize(&path).is_err());
    assert_eq!(
        fs::read(path.join("identity/user-identity/privkey.bin")).unwrap(),
        root
    );
    assert_eq!(
        fs::read(path.join("session.bin")).unwrap(),
        b"interrupted corrupt bytes"
    );
}

#[test]
fn unfinished_temporary_write_keeps_last_committed_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let owner = SessionOwner::initialize(&path).unwrap();
    let before = owner.status();
    drop(owner);
    fs::write(path.join(".session.bin.999.tmp"), b"partial replacement").unwrap();
    assert_eq!(SessionOwner::open(&path).unwrap().status(), before);
}

#[test]
fn failed_commit_never_returns_a_receipt_and_requires_restore() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let before = owner.status();
    fs::rename(path.join("session.bin"), path.join("saved.bin")).unwrap();
    fs::create_dir(path.join("session.bin")).unwrap();
    assert!(
        owner
            .queue_pending("must-not-ack", b"request", false)
            .is_err()
    );
    assert_eq!(owner.status(), before);
    fs::remove_dir(path.join("session.bin")).unwrap();
    fs::rename(path.join("saved.bin"), path.join("session.bin")).unwrap();
    assert!(
        owner.flush().is_err(),
        "a failed durable barrier poisons the current owner"
    );
    drop(owner);
    assert_eq!(SessionOwner::open(&path).unwrap().status(), before);
}

#[test]
fn a_second_process_cannot_take_custody() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let owner = SessionOwner::initialize(&path).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_qos-session-peer"))
        .arg("status")
        .arg(&path)
        .output()
        .unwrap();
    assert!(!result.status.success());
    drop(owner);
    assert!(
        Command::new(env!("CARGO_BIN_EXE_qos-session-peer"))
            .arg("status")
            .arg(&path)
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn checked_export_needs_matching_custody_and_detects_tampering() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let export = dir.path().join("export.bin");
    owner.export(&export).unwrap();
    let expected = owner.status();
    assert!(
        SessionOwner::import(
            &dir.path().join("concurrent"),
            &export,
            &path.join("identity"),
            &path.join("host-identity")
        )
        .is_err()
    );
    assert!(!dir.path().join("concurrent").exists());
    drop(owner);
    let restored = SessionOwner::import(
        &dir.path().join("imported"),
        &export,
        &path.join("identity"),
        &path.join("host-identity"),
    )
    .unwrap();
    assert_eq!(restored.status(), expected);
    let mut bytes = fs::read(&export).unwrap();
    let last = bytes.last_mut().unwrap();
    *last ^= 1;
    fs::write(&export, bytes).unwrap();
    assert!(
        SessionOwner::import(
            &dir.path().join("bad"),
            &export,
            &path.join("identity"),
            &path.join("host-identity")
        )
        .is_err()
    );
    assert!(!dir.path().join("bad").exists());
}

#[test]
fn revoked_identity_and_replay_floors_survive_restart() {
    use tilo_user_identity::{
        DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationEntry, RevocationList,
        persist::{self, IdentityPaths},
        sign_device_cert, sign_revocation_entry,
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let root = persist::read_signing_key(&IdentityPaths::under(path.join("identity")).user_privkey)
        .unwrap();
    let now = tilo_relay::clock::now_ns();
    let (_, device) = DeviceIdentity::generate("revoked member", now);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.public_key,
        device_label: "revoked member".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    let mut revoked = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: device.public_key,
        revoked_at_ns: now,
        reason: tilo_user_identity::RevocationReason::Retired,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut revoked, &root).unwrap();
    let mut list = RevocationList::new();
    list.insert(revoked).unwrap();
    let peer = qos_peer_state::StatePeer::restore(
        &owner.peer().durable().unwrap(),
        owner.peer().room(),
        owner.peer().authority().clone(),
        vec![cert],
        list,
        web_time::SystemTime::now(),
    )
    .unwrap();
    owner
        .commit_admission(peer, owner.directory().to_vec())
        .unwrap();
    let before = owner.status();
    assert!(before.revoked.contains(&device.public_key));
    assert!(!before.replay_floors.is_empty());
    drop(owner);
    assert_eq!(SessionOwner::open(&path).unwrap().status(), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_loopback_subscription_reconnect_keeps_same_owner() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let before = owner.status();
    let host = qos_session_peer::host::LocalHost::start(&owner)
        .await
        .unwrap();
    let mut first = host.connect(&owner).await.unwrap();
    admit_until_ready(&mut owner, &mut first).await;
    qos_session_peer::host::publish(&owner, &first).unwrap();
    first.into_client().close().await;
    owner.refresh_checkpoint().unwrap();
    let mut second = host.connect(&owner).await.unwrap();
    admit_until_ready(&mut owner, &mut second).await;
    qos_session_peer::host::publish(&owner, &second).unwrap();
    assert_eq!(owner.status().room, before.room);
    assert_eq!(owner.status().records, before.records);
    second.into_client().close().await;
    host.shutdown().await.unwrap();
    let final_status = owner.status();
    drop(owner);
    assert_eq!(SessionOwner::open(&path).unwrap().status(), final_status);
}

#[test]
fn authenticated_encryption_obligation_is_committed_even_without_plaintext_checkpoint() {
    use qos_peer_state::{NeighbourMsg, TransportOrigin};
    use tilo_user_identity::persist::{self, IdentityPaths};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let key = persist::read_device_key(&IdentityPaths::under(path.join("identity")).device_privkey)
        .unwrap();
    let mut source = owner.peer().clone();
    let messages = source
        .bootstrap_messages(&key, web_time::SystemTime::now())
        .unwrap();
    let plain = messages
        .iter()
        .find(|message| matches!(message, NeighbourMsg::AppDocument { .. }))
        .unwrap()
        .clone();
    let mut encrypted = plain.clone();
    if let NeighbourMsg::AppDocument {
        envelope,
        app_id,
        cluster_id,
        record_id,
        ..
    } = &mut encrypted
    {
        envelope.encryption = Some(collab_types::EncryptionMetadata {
            epoch: 1,
            channel: collab_types::document_channel_id(app_id, cluster_id, record_id),
        });
        envelope.payload = vec![1, 2, 3];
        collab_types::sign_op(envelope, &key).unwrap();
    }
    assert!(
        owner
            .receive(
                TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                encrypted
            )
            .unwrap()
            .is_some()
    );
    assert!(!owner.permits_publication());
    assert!(owner.outgoing().is_empty());
    drop(owner);
    let mut restored = SessionOwner::open(&path).unwrap();
    assert!(!restored.permits_publication());
    assert!(
        restored
            .receive(
                TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                plain
            )
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_polling_does_not_cancel_initial_connection_or_reconnect() {
    use std::io::Write;
    use std::process::Stdio;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let ready = dir.path().join("ready.json");
    drop(SessionOwner::initialize(&path).unwrap());
    let mut child = Command::new(env!("CARGO_BIN_EXE_qos-session-peer"))
        .arg("run")
        .arg(&path)
        .arg(&ready)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A guard kills the isolated test child even on a failed assertion.
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut input = child.stdin.take().unwrap();
    let mut guard = ChildGuard(child);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
    while !ready.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "test host never wrote ready metadata"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let metadata: serde_json::Value = serde_json::from_slice(&fs::read(&ready).unwrap()).unwrap();
    let origin = metadata["fixture_origin"]
        .as_str()
        .unwrap()
        .strip_prefix("http://")
        .unwrap();
    let mut last_generation = 1u64;
    let mut last_subscription = 0u64;
    let mut last_status = None;
    for cycle in 0..2 {
        if cycle == 1 {
            input.write_all(b"reconnect\n").unwrap();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut subscribed = false;
        while std::time::Instant::now() < deadline {
            let mut socket = tokio::net::TcpStream::connect(origin).await.unwrap();
            socket
                .write_all(format!("GET /status HTTP/1.1\r\nHost: {origin}\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut bytes = vec![];
            socket.read_to_end(&mut bytes).await.unwrap();
            let text = String::from_utf8(bytes).unwrap();
            let body = text.split_once("\r\n\r\n").unwrap().1;
            let status: serde_json::Value = serde_json::from_str(body).unwrap();
            let generation = status["generation"].as_u64().unwrap();
            let subscription = status["native_subscription_generation"].as_u64().unwrap();
            if status["native_peer"] == "subscribed"
                && generation > last_generation
                && subscription > last_subscription
            {
                last_subscription = subscription;
                last_generation = generation;
                last_status =
                    Some(serde_json::from_value::<qos_session_peer::Status>(status).unwrap());
                subscribed = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            subscribed,
            "frequent health polls must not starve connection cycle {cycle}"
        );
    }
    // Abrupt process death must release custody without losing acknowledged state.
    guard.0.kill().unwrap();
    guard.0.wait().unwrap();
    let restored = SessionOwner::open(&path).unwrap();
    assert_observed_state_survives(&restored, &last_status.unwrap());
}

// A status response acknowledges this cut, but the running owner can commit
// additional worker signatures before SIGKILL. Require causal preservation of
// every acknowledged route/head and author floor, allowing those later commits.
fn assert_observed_state_survives(owner: &SessionOwner, observed: &qos_session_peer::Status) {
    let current = owner.status();
    assert_eq!(current.room, observed.room);
    assert_eq!(current.authority, observed.authority);
    assert!(current.generation >= observed.generation);
    for row in &observed.records {
        let bytes = owner
            .peer()
            .document(&row.app_id, &row.record_id)
            .expect("every acknowledged route survives abrupt death");
        let mut doc = ui::automerge::AutoCommit::load(&bytes).unwrap();
        for head in &row.heads {
            let hash = ui::automerge::ChangeHash(hex::decode(head).unwrap().try_into().unwrap());
            assert!(
                doc.get_change_by_hash(&hash).is_some(),
                "acknowledged head missing after abrupt death"
            );
        }
    }
    for (app, record, author, floor) in &observed.replay_floors {
        let restored_floor = current
            .replay_floors
            .iter()
            .find(|(a, r, k, _)| a == app && r == record && k == author)
            .expect("acknowledged author floor survives abrupt death")
            .3;
        assert!(
            restored_floor >= *floor,
            "acknowledged author floor regressed"
        );
    }
}

#[test]
fn same_namespace_admission_cannot_roll_back_document_history_or_actor_floors() {
    use tilo_user_identity::persist::{self, IdentityPaths};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let old = owner.peer().clone();
    let key = persist::read_device_key(&IdentityPaths::under(path.join("identity")).device_privkey)
        .unwrap();
    let mut edited = old.clone();
    qos_browser_runtime::fixture_writer_insert(
        &mut edited,
        &key,
        "retained user document",
        web_time::SystemTime::now(),
    )
    .unwrap();
    owner
        .commit_admission(edited, owner.directory().to_vec())
        .unwrap();
    let before = owner.status();
    assert!(
        owner
            .commit_admission(old, owner.directory().to_vec())
            .is_err()
    );
    assert_eq!(owner.status(), before);
}

async fn admit_until_ready(
    owner: &mut SessionOwner,
    session: &mut qos_session_peer::host::NativeSubscription,
) {
    use qos_peer_state::TransportOrigin;
    use tilo_relay::Inbound;
    while !session.is_ready() {
        match session
            .recv()
            .await
            .expect("native connection must produce Ready")
        {
            Inbound::Ready { .. } => {}
            Inbound::Delta { author, data } => {
                owner
                    .receive(
                        TransportOrigin::RelayDelta(author),
                        postcard::from_bytes(&data).unwrap(),
                    )
                    .unwrap();
            }
            Inbound::Presence { author, payload } => {
                owner
                    .receive(
                        TransportOrigin::UntrustedPresence(author),
                        postcard::from_bytes(&payload).unwrap(),
                    )
                    .unwrap();
            }
        }
    }
}

#[test]
fn deferred_snapshot_stays_reachable_until_governed_coverage_then_commits() {
    use qos_peer_state::{NeighbourMsg, TransportOrigin};
    use tilo_user_identity::persist::{self, IdentityPaths};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let before = owner.status();
    let key = persist::read_device_key(&IdentityPaths::under(path.join("identity")).device_privkey)
        .unwrap();
    let mut source = owner.peer().clone();
    qos_browser_runtime::fixture_writer_insert(
        &mut source,
        &key,
        "queued snapshot",
        web_time::SystemTime::now(),
    )
    .unwrap();
    let messages = source
        .bootstrap_messages(&key, web_time::SystemTime::now())
        .unwrap();
    let snapshot=messages.iter().find(|message|matches!(message,NeighbourMsg::AppDocument{app_id,record_id,..} if app_id=="dev.qos.word-processor" && record_id=="tab:0000000000000001")).unwrap().clone();
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    assert!(
        owner.receive(origin, snapshot).unwrap().is_none(),
        "unadmitted data must not return a commit receipt"
    );
    assert_eq!(
        owner.status(),
        before,
        "no acknowledgement or committed record change yet"
    );
    assert_eq!(
        owner.peer().pending_count(),
        1,
        "deferred candidate remains reachable"
    );
    let governance = messages
        .into_iter()
        .find(|message| matches!(message, NeighbourMsg::SyncResp { .. }))
        .unwrap();
    assert!(owner.receive(origin, governance).unwrap().is_some());
    assert_eq!(owner.peer().pending_count(), 0);
    assert_eq!(owner.peer().rows(), source.rows());
    let committed = owner.status();
    drop(owner);
    assert_eq!(SessionOwner::open(&path).unwrap().status(), committed);
}

#[test]
fn published_durability_receipts_bind_committed_generation_and_survive_failed_barrier() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let receipt = owner.refresh_checkpoint().unwrap();
    for row in &receipt.records {
        let proof = owner
            .peer()
            .durable_coverage(&row.app_id, &row.record_id)
            .unwrap()
            .expect("native checkpoint must carry barrier-bound durable proof");
        assert_eq!(proof.generation, receipt.generation);
        assert_eq!(proof.coverage.heads, row.heads);
    }
    let outgoing = owner.outgoing().to_vec();
    let before = owner.status();
    fs::rename(path.join("session.bin"), path.join("saved.bin")).unwrap();
    fs::create_dir(path.join("session.bin")).unwrap();
    assert!(owner.refresh_checkpoint().is_err());
    assert!(!owner.permits_publication());
    assert_eq!(owner.outgoing(), outgoing);
    assert_eq!(owner.status(), before);
    fs::remove_dir(path.join("session.bin")).unwrap();
    fs::rename(path.join("saved.bin"), path.join("session.bin")).unwrap();
    drop(owner);
    let owner = SessionOwner::open(&path).unwrap();
    assert_eq!(owner.outgoing(), outgoing);
    for row in receipt.records {
        assert_eq!(
            owner
                .peer()
                .durable_coverage(&row.app_id, &row.record_id)
                .unwrap()
                .unwrap()
                .generation,
            receipt.generation
        );
    }
}

#[test]
fn desktop_bootstrap_is_durable_and_preserves_existing_record_routes() {
    use ui::automerge::{AutoCommit, ROOT, ReadDoc};
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("desktop");
    let owner = qos_session_peer::SessionOwner::initialize(&root).unwrap();
    let before = owner.status();
    assert!(
        before.records.iter().any(|r| r.app_id == "dev.qos.notes"),
        "desktop must include a real initialized Notes record"
    );
    let writer = owner
        .peer()
        .document("dev.qos.word-processor", "tab:0000000000000001")
        .unwrap();
    let chat = owner.peer().document("dev.qos.chat", "").unwrap();
    let message = owner.peer().room_message();
    let qos_peer_state::NeighbourMsg::SyncResp { changes, .. } = message else {
        panic!("room state")
    };
    let mut doc = AutoCommit::new();
    doc.apply_changes(
        changes
            .iter()
            .map(|b| ui::automerge::Change::try_from(b.as_slice()).unwrap()),
    )
    .unwrap();
    assert!(doc.get(ROOT, "contents").unwrap().is_some());
    let heads = doc.get_heads();
    drop(owner);
    for _ in 0..2 {
        let owner = qos_session_peer::SessionOwner::open(&root).unwrap();
        assert_eq!(owner.status().room, before.room);
        assert_eq!(owner.status().authority, before.authority);
        assert_eq!(
            owner.status().generation,
            before.generation,
            "reopen must not reseed topology"
        );
        assert_eq!(
            owner
                .peer()
                .document("dev.qos.word-processor", "tab:0000000000000001")
                .unwrap(),
            writer
        );
        assert_eq!(owner.peer().document("dev.qos.chat", "").unwrap(), chat);
        let qos_peer_state::NeighbourMsg::SyncResp { changes, .. } = owner.peer().room_message()
        else {
            panic!("room state")
        };
        let mut doc = AutoCommit::new();
        doc.apply_changes(
            changes
                .iter()
                .map(|b| ui::automerge::Change::try_from(b.as_slice()).unwrap()),
        )
        .unwrap();
        assert_eq!(doc.get_heads(), heads);
    }
}
