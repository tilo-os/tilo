//! Real owner publication fixtures: unchanged frozen NeighbourMsg wire bodies.
use qos_peer_state::{NeighbourMsg, StatePeer, TransportOrigin, transport::Retention};
use qos_session_peer::SessionOwner;
use ui::automerge::{AutoCommit, ROOT, transaction::Transactable};
use web_time::SystemTime;

fn owner_key(owner: &SessionOwner) -> ed25519_dalek::SigningKey {
    qos_user_identity::persist::read_device_key(
        &qos_user_identity::persist::IdentityPaths::under(owner.root().join("identity"))
            .device_privkey,
    )
    .unwrap()
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
    let root = qos_user_identity::persist::read_signing_key(
        &qos_user_identity::persist::IdentityPaths::under(path.join("identity")).user_privkey,
    )
    .unwrap();
    let foreign_key = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
    let mut certificate = qos_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: foreign_key.verifying_key().to_bytes(),
        device_label: "foreign fixture author".into(),
        capabilities: qos_user_identity::DeviceCapabilities::member(),
        issued_at_ns: qos_relay::clock::now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
    };
    qos_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
    let mut candidate = owner.peer().clone();
    candidate
        .admit_certificate(certificate.clone(), &key, SystemTime::now())
        .unwrap();
    owner
        .commit_admission(candidate, owner.directory().to_vec())
        .unwrap();
    let baseline = owner.outgoing().to_vec();
    let baseline_kinds = owner.outgoing_kinds().to_vec();
    let baseline_state = owner.peer().durable().unwrap();
    let authority = owner.peer().authority().device_public_key;
    let mut offline = owner.peer().clone();
    let mut sender = owner.peer().clone();
    let app = "dev.qos.word-processor";
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
    let untouched = owner.peer().durable_coverage("dev.qos.chat", "").unwrap();
    owner
        .receive(
            TransportOrigin::RelayDelta(certificate.device_public_key),
            original.clone(),
        )
        .unwrap()
        .unwrap();
    assert!(owner.outgoing().starts_with(&baseline));
    assert_eq!(owner.outgoing().len(), baseline.len() + 2);
    assert_eq!(
        &owner.outgoing_kinds()[baseline.len()..],
        &[Retention::Delta, Retention::Checkpoint]
    );
    assert_eq!(
        owner.peer().durable_coverage("dev.qos.chat", "").unwrap(),
        untouched
    );
    assert!(owner.peer().covers(&original));
    let committed = owner.status();
    let outgoing = owner.outgoing().to_vec();
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
    assert_eq!(owner.outgoing(), outgoing);
    assert_eq!(postcard::to_stdvec(&original).unwrap(), exact_original);
    let before_room = owner.outgoing().len();
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
    assert_eq!(owner.outgoing().len(), before_room + 1);
    assert_eq!(owner.outgoing_kinds().last(), Some(&Retention::Delta));
    assert_eq!(
        qos_peer_state::durability::entries(owner.peer().admitted_room().unwrap()).unwrap(),
        receipts
    );
    let tail = owner.outgoing()[baseline.len()..].to_vec();
    let tail_kinds = owner.outgoing_kinds()[baseline.len()..].to_vec();
    let committed_bytes = owner.outgoing().to_vec();
    let committed_kinds = owner.outgoing_kinds().to_vec();
    let rows = owner.peer().rows();
    // No send has happened. Reopening must preserve the exact committed recovery set.
    drop(owner);
    let mut owner = SessionOwner::open(&path).unwrap();
    assert_eq!(owner.outgoing(), committed_bytes);
    assert_eq!(owner.outgoing_kinds(), committed_kinds);
    let now = SystemTime::now();
    let mut fresh = StatePeer::new(
        owner.peer().room(),
        owner.peer().authority().clone(),
        owner.peer().certificates(),
        qos_user_identity::RevocationList::new(),
        now,
    )
    .unwrap();
    replay(&mut fresh, authority, owner.outgoing(), now);
    replay(&mut offline, authority, &tail, now);
    replay(&mut sender, authority, &tail, now);
    replay(&mut offline, authority, &tail, now);
    for peer in [&fresh, &offline, &sender] {
        assert_eq!(peer.rows(), rows);
        assert!(peer.covers(&original));
        assert!(peer.durable_covers_current(app, record).unwrap());
        assert_eq!(
            peer.durable_coverage("dev.qos.chat", "").unwrap(),
            untouched
        );
    }
    owner.refresh_checkpoint().unwrap();
    let mut checkpoint_peer = StatePeer::new(
        owner.peer().room(),
        owner.peer().authority().clone(),
        owner.peer().certificates(),
        qos_user_identity::RevocationList::new(),
        now,
    )
    .unwrap();
    replay(&mut checkpoint_peer, authority, owner.outgoing(), now);
    assert_eq!(checkpoint_peer.rows(), rows);
    assert!(checkpoint_peer.covers(&original));
    // Optional test-only evidence export for Task 3b's native storage fixtures.
    if let Some(dir) = std::env::var_os("QOS_PUBLICATION_FIXTURE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = serde_json::json!({
            "version":1, "room":owner.peer().room(), "authority":owner.peer().authority(), "certificates":owner.peer().certificates(),
            "now_ns":qos_relay::clock::now_ns(), "rows":rows,
            "baseline":fixture_batch(&dir,"baseline",&baseline,&baseline_kinds),
            "tail":fixture_batch(&dir,"tail",&tail,&tail_kinds),
            "checkpoint2":fixture_batch(&dir,"checkpoint2",owner.outgoing(),owner.outgoing_kinds())
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
fn one_large_touched_record_compacts_by_bytes_without_limiting_canonical_history() {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let baseline = owner.publication_baseline();
    let key = owner_key(&owner);
    let mut candidate = owner.peer().clone();
    let app = "dev.qos.word-processor";
    let record = "tab:0000000000000001";
    let mut doc = AutoCommit::load(&candidate.document(app, record).unwrap()).unwrap();
    // Incompressible, deterministic bytes exercise the byte threshold before the count threshold.
    let mut large = Vec::new();
    for index in 0u64..140_000 {
        large.extend_from_slice(blake3::hash(&index.to_le_bytes()).as_bytes());
    }
    doc.put(
        ROOT,
        "large_record",
        ui::automerge::ScalarValue::Bytes(large),
    )
    .unwrap();
    doc.commit();
    candidate
        .publish_document(app, record, doc.save(), &key, SystemTime::now())
        .unwrap();
    owner
        .commit_mutation_candidate(candidate, owner.directory().to_vec())
        .unwrap();
    assert!(
        owner.publication_baseline().0 > baseline.0,
        "one large snapshot must trigger byte compaction"
    );
    assert_eq!(owner.publication_baseline().1, owner.outgoing().len());
    assert!(
        owner.outgoing().iter().map(Vec::len).sum::<usize>() > 4 * 1024 * 1024,
        "canonical baseline may exceed the redundant-tail byte bound"
    );
    assert!(
        owner
            .outgoing_kinds()
            .iter()
            .all(|kind| *kind == Retention::Checkpoint)
    );
    let now = SystemTime::now();
    let mut fresh = StatePeer::new(
        owner.peer().room(),
        owner.peer().authority().clone(),
        owner.peer().certificates(),
        qos_user_identity::RevocationList::new(),
        now,
    )
    .unwrap();
    replay(
        &mut fresh,
        owner.peer().authority().device_public_key,
        owner.outgoing(),
        now,
    );
    assert_eq!(fresh.rows(), owner.peer().rows());
    assert!(fresh.durable_covers_current(app, record).unwrap());
}
