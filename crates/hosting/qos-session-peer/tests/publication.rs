//! Real owner publication fixtures: unchanged frozen NeighbourMsg wire bodies.
use qos_peer_state::{NeighbourMsg, StatePeer, TransportOrigin, transport::Retention};
use qos_session_peer::SessionOwner;
use ui::automerge::{AutoCommit, ROOT, transaction::Transactable};
use web_time::SystemTime;

fn owner_key(owner: &SessionOwner) -> ed25519_dalek::SigningKey {
    tilo_user_identity::persist::read_device_key(
        &tilo_user_identity::persist::IdentityPaths::under(
            owner.root().canonicalize().unwrap().join("identity"),
        )
        .device_privkey,
    )
    .unwrap()
}

#[tokio::test]
async fn local_host_persists_exact_native_owner_authority_before_serving() {
    let temp = tempfile::tempdir().unwrap();
    let owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let host = qos_session_peer::host::LocalHost::start(&owner)
        .await
        .unwrap();
    let workspace = tilo_relay::room_workspace_id(owner.peer().room()).unwrap();
    let path = owner
        .root()
        .join("relay/workspaces")
        .join(workspace.to_string())
        .join("shell-authority-v1.json");
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(stored["version"], 1);
    let authority: qos_peer_state::retained_proof::RetentionAuthority =
        serde_json::from_value(stored["authority"].clone()).unwrap();
    assert_eq!(authority.room(), owner.peer().room());
    assert_eq!(authority.governor(), owner.peer().authority());
    assert_ne!(authority.governor(), owner.host_certificate());
    host.shutdown().await.unwrap();
}
fn replay(peer: &mut StatePeer, author: [u8; 32], messages: &[Vec<u8>], now: SystemTime) {
    for bytes in messages {
        peer.receive_from(
            TransportOrigin::RelayDelta(author),
            postcard::from_bytes(bytes).unwrap(),
            now,
        )
        .unwrap();
    }
}
fn fixture_batch(
    dir: &std::path::Path,
    name: &str,
    messages: &[Vec<u8>],
    kinds: &[Retention],
) -> serde_json::Value {
    let folder = dir.join(name);
    std::fs::create_dir_all(&folder).unwrap();
    let entries = messages.iter().zip(kinds).enumerate().map(|(index, (bytes, kind))| {
        let file = format!("{name}/{index:03}.postcard");
        std::fs::write(dir.join(&file), bytes).unwrap();
        let msg: NeighbourMsg = postcard::from_bytes(bytes).unwrap();
        let (channel, actual) = qos_peer_state::transport::retention(&msg, *kind == Retention::Checkpoint).unwrap();
        assert_eq!(actual, *kind);
        serde_json::json!({"file": file, "kind": format!("{kind:?}"), "channel": hex::encode(channel), "bytes":bytes.len(), "blake3":blake3::hash(bytes).to_hex().to_string()})
    }).collect::<Vec<_>>();
    serde_json::json!(entries)
}

#[test]
fn actual_shell_receive_publication_preserves_foreign_intent_and_recovers_baseline_tail() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let key = owner_key(&owner);
    let root = tilo_user_identity::persist::read_signing_key(
        &tilo_user_identity::persist::IdentityPaths::under(
            owner.root().canonicalize().unwrap().join("identity"),
        )
        .user_privkey,
    )
    .unwrap();
    let foreign_key = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
    let mut certificate = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: foreign_key.verifying_key().to_bytes(),
        device_label: "foreign fixture author".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_relay::clock::now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
    let mut candidate = owner.peer().clone();
    candidate
        .admit_certificate(certificate.clone(), &key, SystemTime::now())
        .unwrap();
    owner
        .commit_admission(candidate, owner.directory().to_vec())
        .unwrap();
    let baseline = outgoing_bytes(&owner).to_vec();
    let baseline_kinds = outgoing_kinds(&owner).to_vec();
    let baseline_state = owner.peer().durable().unwrap();
    let authority = owner.peer().authority().device_public_key;
    let mut offline = owner.peer().clone();
    let mut sender = owner.peer().clone();
    let app = "so.tilo.word-processor";
    let record = "tab:0000000000000001";
    let mut doc = AutoCommit::load(&sender.document(app, record).unwrap()).unwrap();
    doc.set_actor(ui::automerge::ActorId::from(
        certificate.device_public_key.to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(ROOT, "foreign_once", "original signed change")
        .unwrap();
    doc.commit();
    let original = sender
        .publish_incremental(
            app,
            record,
            doc.save_after(&heads),
            &foreign_key,
            SystemTime::now(),
        )
        .unwrap();
    let exact_original = postcard::to_stdvec(&original).unwrap();
    let untouched = owner.peer().durable_coverage("so.tilo.chat", "").unwrap();
    owner
        .receive(
            TransportOrigin::RelayDelta(certificate.device_public_key),
            original.clone(),
        )
        .unwrap()
        .unwrap();
    assert!(outgoing_bytes(&owner).starts_with(&baseline));
    assert_eq!(outgoing_bytes(&owner).len(), baseline.len() + 2);
    assert_eq!(
        &outgoing_kinds(&owner)[baseline.len()..],
        &[Retention::Delta, Retention::Checkpoint]
    );
    assert_eq!(
        owner.peer().durable_coverage("so.tilo.chat", "").unwrap(),
        untouched
    );
    assert!(owner.peer().covers(&original));
    let committed = owner.status();
    let outgoing = outgoing_bytes(&owner).to_vec();
    assert!(
        owner
            .receive(
                TransportOrigin::RelayDelta(certificate.device_public_key),
                original.clone()
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(owner.status(), committed);
    assert_eq!(outgoing_bytes(&owner), outgoing);
    assert_eq!(postcard::to_stdvec(&original).unwrap(), exact_original);
    let before_room = outgoing_bytes(&owner).len();
    let receipts =
        qos_peer_state::durability::entries(owner.peer().admitted_room().unwrap()).unwrap();
    let mut room = owner.peer().admitted_room().unwrap().clone();
    let heads = room.get_heads();
    room.put(ROOT, "name", "selectively renamed room").unwrap();
    room.commit();
    let changes = room
        .get_changes(&heads)
        .into_iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect();
    owner
        .receive(
            TransportOrigin::RelayDelta(authority),
            NeighbourMsg::SyncResp {
                room: owner.peer().room().into(),
                changes,
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(outgoing_bytes(&owner).len(), before_room + 1);
    assert_eq!(outgoing_kinds(&owner).last(), Some(&Retention::Delta));
    assert_eq!(
        qos_peer_state::durability::entries(owner.peer().admitted_room().unwrap()).unwrap(),
        receipts
    );
    let tail = outgoing_bytes(&owner)[baseline.len()..].to_vec();
    let tail_kinds = outgoing_kinds(&owner)[baseline.len()..].to_vec();
    let committed_bytes = outgoing_bytes(&owner).to_vec();
    let committed_kinds = outgoing_kinds(&owner).to_vec();
    let rows = owner.peer().rows();
    // No send has happened. Reopening must preserve the exact committed recovery set.
    drop(owner);
    let mut owner = SessionOwner::open(&path).unwrap();
    assert_eq!(outgoing_bytes(&owner), committed_bytes);
    assert_eq!(outgoing_kinds(&owner), committed_kinds);
    let now = SystemTime::now();
    let mut fresh = StatePeer::new(
        owner.peer().room(),
        owner.peer().authority().clone(),
        owner.peer().certificates(),
        tilo_user_identity::RevocationList::new(),
        now,
    )
    .unwrap();
    replay(&mut fresh, authority, &outgoing_bytes(&owner), now);
    replay(&mut offline, authority, &tail, now);
    replay(&mut sender, authority, &tail, now);
    replay(&mut offline, authority, &tail, now);
    for peer in [&fresh, &offline, &sender] {
        assert_eq!(peer.rows(), rows);
        assert!(peer.covers(&original));
        assert!(peer.durable_covers_current(app, record).unwrap());
        assert_eq!(
            peer.durable_coverage("so.tilo.chat", "").unwrap(),
            untouched
        );
    }
    owner.refresh_checkpoint().unwrap();
    let mut checkpoint_peer = StatePeer::new(
        owner.peer().room(),
        owner.peer().authority().clone(),
        owner.peer().certificates(),
        tilo_user_identity::RevocationList::new(),
        now,
    )
    .unwrap();
    replay(
        &mut checkpoint_peer,
        authority,
        &outgoing_bytes(&owner),
        now,
    );
    assert_eq!(checkpoint_peer.rows(), rows);
    assert!(checkpoint_peer.covers(&original));
    // Optional test-only evidence export for Task 3b's native storage fixtures.
    if let Some(dir) = std::env::var_os("QOS_PUBLICATION_FIXTURE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = serde_json::json!({
            "version":1, "room":owner.peer().room(), "authority":owner.peer().authority(), "certificates":owner.peer().certificates(),
            "now_ns":tilo_relay::clock::now_ns(), "rows":rows,
            "baseline":fixture_batch(&dir,"baseline",&baseline,&baseline_kinds),
            "tail":fixture_batch(&dir,"tail",&tail,&tail_kinds),
            "checkpoint2":fixture_batch(&dir,"checkpoint2",&outgoing_bytes(&owner),&outgoing_kinds(&owner))
        });
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join("original-foreign.postcard"), exact_original).unwrap();
        std::fs::write(dir.join("baseline-peer.postcard"), baseline_state).unwrap();
        std::fs::write(
            dir.join("final-peer.postcard"),
            owner.peer().durable().unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn multiple_bounded_records_compact_by_aggregate_bytes_and_reconstruct_exactly() {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let baseline = owner.publication_baseline();
    let key = owner_key(&owner);
    let touched = owner.peer().rows();
    assert_eq!(touched.len(), 3, "fixture requires three canonical records");
    // Distinct valid per-message mutations exercise aggregate byte compaction
    // while every incremental and reconnect snapshot remains transport-bounded.
    for (chunk, row) in touched.iter().enumerate() {
        let mut candidate = owner.peer().clone();
        let mut doc =
            AutoCommit::load(&candidate.document(&row.app_id, &row.record_id).unwrap()).unwrap();
        let heads = doc.get_heads();
        let mut bytes = Vec::new();
        for index in 0u64..48_000 {
            bytes.extend_from_slice(
                blake3::hash(
                    &(chunk as u64)
                        .wrapping_mul(48_000)
                        .wrapping_add(index)
                        .to_le_bytes(),
                )
                .as_bytes(),
            );
        }
        doc.put(
            ROOT,
            format!("large_record_{chunk}"),
            ui::automerge::ScalarValue::Bytes(bytes),
        )
        .unwrap();
        doc.commit();
        let incremental = doc.save_after(&heads);
        assert!(
            incremental.len() < 3 * 1024 * 1024,
            "fixture mutation must remain below the production transport cap"
        );
        assert!(
            doc.save().len() < 3 * 1024 * 1024,
            "fixture snapshot must remain below the production transport cap"
        );
        candidate
            .publish_incremental(
                &row.app_id,
                &row.record_id,
                incremental,
                &key,
                SystemTime::now(),
            )
            .unwrap();
        owner
            .commit_mutation_candidate(candidate, owner.directory().to_vec())
            .unwrap();
    }
    assert!(
        owner.publication_baseline().0 > baseline.0,
        "bounded causal mutations must trigger aggregate byte compaction"
    );
    assert_eq!(owner.publication_baseline().1, outgoing_bytes(&owner).len());
    assert!(
        outgoing_bytes(&owner).iter().map(Vec::len).sum::<usize>() > 4 * 1024 * 1024,
        "canonical baseline may exceed the redundant-tail byte bound"
    );
    assert!(
        outgoing_kinds(&owner)
            .iter()
            .all(|kind| *kind == Retention::Checkpoint)
    );
    let now = SystemTime::now();
    let mut fresh = StatePeer::new(
        owner.peer().room(),
        owner.peer().authority().clone(),
        owner.peer().certificates(),
        tilo_user_identity::RevocationList::new(),
        now,
    )
    .unwrap();
    replay(
        &mut fresh,
        owner.peer().authority().device_public_key,
        &outgoing_bytes(&owner),
        now,
    );
    assert_eq!(fresh.rows(), owner.peer().rows());
    for row in touched {
        assert_eq!(
            fresh.document(&row.app_id, &row.record_id),
            owner.peer().document(&row.app_id, &row.record_id),
            "the compacted baseline reconstructs the exact canonical document for {}/{}",
            row.app_id,
            row.record_id
        );
        assert_eq!(
            fresh.document_heads(&row.app_id, &row.record_id),
            owner.peer().document_heads(&row.app_id, &row.record_id),
            "the compacted baseline reconstructs the exact canonical heads for {}/{}",
            row.app_id,
            row.record_id
        );
        assert!(
            fresh
                .durable_covers_current(&row.app_id, &row.record_id)
                .unwrap(),
            "the compacted baseline does not cover {}/{}",
            row.app_id,
            row.record_id
        );
    }
}

fn outgoing_bytes(owner: &qos_session_peer::SessionOwner) -> Vec<Vec<u8>> {
    owner.outgoing().iter().map(|p| p.bytes.to_vec()).collect()
}
fn outgoing_kinds(
    owner: &qos_session_peer::SessionOwner,
) -> Vec<qos_peer_state::transport::Retention> {
    owner.outgoing().iter().map(|p| p.kind).collect()
}

#[test]
fn delivery_selected_publication_replays_native_checkpoint_after_local_edit_and_restart() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("delivery-owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    owner.activate_delivery().unwrap();
    let mut offline = owner.peer().clone();
    let mut candidate = owner.peer().clone();
    let key = owner_key(&owner);
    let mut document = AutoCommit::load(&candidate.document("so.tilo.chat", "").unwrap()).unwrap();
    document
        .put(ROOT, "delivery-local", "persisted once")
        .unwrap();
    document.commit();
    candidate
        .publish_document("so.tilo.chat", "", document.save(), &key, SystemTime::now())
        .unwrap();
    let before_len = owner.outgoing().len();
    let receipt = owner
        .commit_mutation_candidate(candidate, owner.directory().to_vec())
        .unwrap();
    let tail = owner
        .outgoing()
        .iter()
        .skip(before_len)
        .map(|publication| publication.bytes.to_vec())
        .collect::<Vec<_>>();
    assert_eq!(tail.len(), 2);
    replay(
        &mut offline,
        key.verifying_key().to_bytes(),
        &tail,
        SystemTime::now(),
    );
    assert_eq!(offline.rows(), owner.peer().rows());
    assert_eq!(
        offline
            .durable_coverage("so.tilo.chat", "")
            .unwrap()
            .unwrap()
            .generation,
        receipt.generation
    );
    let outgoing = outgoing_bytes(&owner);
    drop(owner);
    let owner = SessionOwner::open(&path).unwrap();
    assert_eq!(outgoing_bytes(&owner), outgoing);
    assert!(owner.peer().admission_v5_selected());
}
