//! Governance is decoded once per room document state, not once per message.
use super::*;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

fn device(root: &SigningKey, seed: u8) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device-{seed}"),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

/// The governor `a` holding `records` seeded documents, and a fresh member
/// replica with the same pinned authority.
fn governor_and_replica(records: usize) -> (StatePeer, StatePeer, SigningKey, SystemTime) {
    let root = SigningKey::from_bytes(&[1; 32]);
    let now = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(2000))
        .unwrap();
    let (a, ac) = device(&root, 2);
    let (_, bc) = device(&root, 3);
    let roster = vec![ac.clone(), bc];
    let mut governor =
        StatePeer::new(ROOM, ac.clone(), roster.clone(), RevocationList::new(), now).unwrap();
    let documents = (0..records)
        .map(|index| {
            let mut document = AutoCommit::new();
            document
                .put(ROOT, "value", i64::try_from(index).unwrap())
                .unwrap();
            document.commit();
            SeedDocument {
                app_id: "test.app".into(),
                record_id: format!("tab:{index}"),
                kind: "test".into(),
                bytes: document.save(),
            }
        })
        .collect();
    governor
        .install_seed(
            SeedState {
                room_changes: seed_room(ROOM, &ac, &roster).unwrap(),
                documents,
            },
            &a,
            now,
        )
        .unwrap();
    let replica = StatePeer::new(ROOM, ac, roster, RevocationList::new(), now).unwrap();
    (governor, replica, a, now)
}

#[test]
fn authorize_document_decodes_capabilities_at_most_once_per_document_state() {
    const MESSAGES: usize = 10;
    let (mut governor, mut replica, a, now) = governor_and_replica(MESSAGES);
    let source = a.verifying_key().to_bytes();
    let messages = governor
        .publication_mut()
        .signed_snapshots(&a, now)
        .unwrap();
    assert_eq!(messages.len(), MESSAGES);
    replica
        .receive(source, governor.room_message(), now)
        .unwrap();
    // Counted without touching the cache under test.
    let identities = room::governance_of(replica.core.room_doc.as_deref())
        .identities
        .len();
    assert_eq!(identities, 2, "both roster devices are governed identities");
    crate::room::POLICY_READS.with(|count| count.set(0));
    crate::cached_document::IDENTITY_DECODES.with(|count| count.set(0));
    for message in messages {
        // The production admission path: role lookup, then capability decision.
        let outcome = replica.receive(source, message, now).unwrap();
        assert!(outcome.changed && outcome.rejected == 0);
    }
    assert_eq!(replica.rows(), governor.rows());
    assert_eq!(
        crate::room::POLICY_READS.with(std::cell::Cell::get),
        1,
        "capabilities are decoded once for the governance state, not per message"
    );
    assert_eq!(
        crate::cached_document::IDENTITY_DECODES.with(std::cell::Cell::get),
        identities,
        "each identity row is decoded once for the governance state, not per message"
    );
}

/// The governor writes `receipts` into its room document, as an owner commit
/// does, and returns the room message that carries them.
fn publish_receipts(governor: &mut StatePeer, receipts: &[(&str, &[u8])]) -> NeighbourMsg {
    let room = governor.core.room_doc.as_deref_mut().unwrap();
    let object = match room.get(ROOT, "effect_receipts_v1").unwrap() {
        Some((_, object)) => object,
        None => room
            .put_object(ROOT, "effect_receipts_v1", ObjType::Map)
            .unwrap(),
    };
    for (key, bytes) in receipts {
        room.put(&object, *key, automerge::ScalarValue::Bytes(bytes.to_vec()))
            .unwrap();
    }
    room.commit();
    governor.room_message()
}

fn receipt_decodes() -> usize {
    crate::room::RECEIPT_DECODES.with(std::cell::Cell::get)
}

#[test]
fn sync_resp_decodes_each_receipt_once() {
    let (mut governor, mut replica, a, now) = governor_and_replica(1);
    let source = a.verifying_key().to_bytes();
    replica
        .receive(source, governor.room_message(), now)
        .unwrap();
    // Spaced JSON: a re-encoding would not reproduce these bytes.
    let first: Vec<(String, Vec<u8>)> = (0..5)
        .map(|n| {
            (
                format!("invocation-{n}"),
                format!(r#"{{ "outcome": "done", "n": {n} }}"#).into_bytes(),
            )
        })
        .collect();
    let rows: Vec<(&str, &[u8])> = first
        .iter()
        .map(|(key, bytes)| (key.as_str(), bytes.as_slice()))
        .collect();
    let message = publish_receipts(&mut governor, &rows);
    crate::room::RECEIPT_DECODES.with(|count| count.set(0));
    replica.receive(source, message, now).unwrap();
    assert_eq!(receipt_decodes(), first.len(), "each new receipt once");
    for (key, bytes) in &first {
        assert_eq!(
            replica.core.receipts.get(key),
            Some(bytes),
            "receipts are recorded as received, never re-encoded"
        );
    }

    // A later sync restating every admitted receipt decodes only the new one.
    let message = publish_receipts(&mut governor, &[("invocation-5", br#"{"outcome":"late"}"#)]);
    crate::room::RECEIPT_DECODES.with(|count| count.set(0));
    replica.receive(source, message, now).unwrap();
    assert_eq!(receipt_decodes(), 1, "only the new receipt is decoded");
    assert_eq!(replica.core.receipts.len(), first.len() + 1);

    // A ledger row an older version stored re-encoded is still the same
    // evidence: it is compared by value once and then kept as received.
    let legacy: serde_json::Value = serde_json::from_slice(&first[0].1).unwrap();
    replica
        .core
        .receipts
        .insert(first[0].0.clone(), serde_json::to_vec(&legacy).unwrap());
    let message = publish_receipts(
        &mut governor,
        &[("invocation-6", br#"{"outcome":"later"}"#)],
    );
    replica.receive(source, message, now).unwrap();
    assert_eq!(replica.core.receipts.get(&first[0].0), Some(&first[0].1));
    crate::room::RECEIPT_DECODES.with(|count| count.set(0));
    replica
        .receive(source, governor.room_message(), now)
        .unwrap();
    assert_eq!(receipt_decodes(), 0, "an unchanged sync decodes nothing");

    // Rewriting admitted evidence is refused, even by the governor.
    let rewrite = publish_receipts(
        &mut governor,
        &[("invocation-0", br#"{"outcome":"undone"}"#)],
    );
    let before = replica.core.receipts.clone();
    assert_eq!(
        replica.receive(source, rewrite, now).unwrap_err(),
        "effect receipt is immutable"
    );
    assert_eq!(replica.core.receipts, before);
}
