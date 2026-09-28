//! A replica without the channel key keeps sealed writes as their record.
use super::*;
use crate::admission_tests::seeded;
use crate::{InputDisposition, TransportOrigin, peer_handle};

const APP: &str = "test.app";
const RECORD: &str = "tab:1";

/// A receiver holding the seeded record, as a durable host would.
fn host() -> (StatePeer, SigningKey, SigningKey, SystemTime) {
    let (mut native, a, b, bc, now) = seeded();
    let mut receiver = StatePeer::new(
        native.room(),
        native.authority().clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .unwrap();
    receiver.select_admission_v5().unwrap();
    for message in native.bootstrap_messages(&a, now).unwrap() {
        receiver.receive_from(origin(&a), message, now).unwrap();
    }
    (receiver, a, b, now)
}

fn origin(key: &SigningKey) -> TransportOrigin {
    TransportOrigin::RelayDelta(key.verifying_key().to_bytes())
}

/// `key`'s sealed write number `seq` to the seeded record.
fn sealed(peer: &StatePeer, key: &SigningKey, seq: u64, ciphertext: &[u8]) -> NeighbourMsg {
    let author = Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
    let channel = collab_types::document_channel_id(APP, peer.room(), RECORD);
    let mut envelope = SignedOp::new_unsigned_with_encryption(
        ciphertext.to_vec(),
        collab_types::document_artifact_id(APP, peer.room(), RECORD),
        author,
        collab_types::document_attestation_id(APP, peer.room(), RECORD),
        OpClass::Splice,
        peer_time(),
        seq,
        collab_types::EncryptionMetadata::mls_application(1, channel),
    );
    collab_types::sign_op(&mut envelope, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: peer_handle(&key.verifying_key().to_bytes()),
        app_id: APP.into(),
        cluster_id: peer.room().into(),
        record_id: RECORD.into(),
        envelope: Box::new(envelope),
    }
}

/// The seeded fixture's clock.
fn peer_time() -> SystemTime {
    seeded().4
}

fn restore(peer: &StatePeer, now: SystemTime) -> Result<StatePeer, String> {
    restore_bytes(peer, &peer.durable()?, now)
}

fn restore_bytes(peer: &StatePeer, bytes: &[u8], now: SystemTime) -> Result<StatePeer, String> {
    StatePeer::restore(
        bytes,
        peer.room(),
        peer.authority().clone(),
        vec![],
        RevocationList::new(),
        now,
    )
}

fn held(peer: &StatePeer) -> Vec<SignedOp> {
    peer.core.documents[&Route {
        app: APP.into(),
        record: RECORD.into(),
    }]
        .sealed
        .clone()
}

#[test]
fn a_sealed_write_becomes_the_durable_record_and_survives_restart() {
    let (mut host, _, b, now) = host();
    let plaintext = host.document(APP, RECORD).unwrap();
    let unsealed = host.durable().unwrap();
    assert_eq!(
        unsealed[0], 5,
        "a state without sealed writes keeps its v5 bytes"
    );
    let write = sealed(&host, &b, 1, b"ciphertext the host cannot open");
    let report = host.receive_from_report(origin(&b), write, now).unwrap();
    assert!(
        matches!(report.primary(), InputDisposition::Accepted(_)),
        "a sealed write is admitted with an exact receipt, not parked"
    );
    assert_eq!(host.incomplete_reason(), None);
    assert_eq!(
        host.document(APP, RECORD).unwrap(),
        plaintext,
        "ciphertext is never merged into the plaintext document"
    );
    let restored = restore(&host, now).unwrap();
    assert_eq!(host.durable().unwrap()[0], 6);
    assert_eq!(held(&restored), held(&host));
    assert_eq!(
        held(&restored)[0].payload,
        b"ciphertext the host cannot open"
    );
    assert_eq!(restored.incomplete_reason(), None);
}

#[test]
fn tampered_ciphertext_is_refused_and_leaves_the_record_unchanged() {
    let (mut host, _, b, now) = host();
    let before = host.durable().unwrap();
    let mut write = sealed(&host, &b, 1, b"sealed");
    if let NeighbourMsg::AppDocument { envelope, .. } = &mut write {
        envelope.payload[0] ^= 1;
    }
    assert!(host.receive_from(origin(&b), write, now).is_err());
    assert_eq!(host.durable().unwrap(), before);
}

#[test]
fn a_sealed_write_from_another_signer_is_refused() {
    let (mut host, _, b, now) = host();
    let before = held(&host);
    let stranger = SigningKey::from_bytes(&[77; 32]);
    let write = sealed(&host, &stranger, 1, b"sealed");
    let report = host
        .receive_from_report(origin(&stranger), write, now)
        .unwrap();
    assert_eq!(
        report.primary(),
        &InputDisposition::RetryLater(crate::MissingPrerequisite::Certificate),
        "a device outside the room waits for a certificate, never admitted"
    );
    assert_eq!(held(&host), before);
    let mut forged = sealed(&host, &stranger, 1, b"sealed");
    if let NeighbourMsg::AppDocument { envelope, peer, .. } = &mut forged {
        envelope.author = Ed25519PublicKey::from_bytes(b.verifying_key().to_bytes());
        *peer = peer_handle(&b.verifying_key().to_bytes());
    }
    assert!(
        host.receive_from(origin(&b), forged, now).is_err(),
        "a signature that is not the named author's is refused"
    );
    assert_eq!(held(&host), before);
}

#[test]
fn a_replayed_sealed_write_is_kept_once() {
    let (mut host, _, b, now) = host();
    let write = sealed(&host, &b, 1, b"sealed");
    host.receive_from(origin(&b), write.clone(), now).unwrap();
    let report = host.receive_from_report(origin(&b), write, now).unwrap();
    assert!(!matches!(report.primary(), InputDisposition::Accepted(_)));
    assert_eq!(held(&host).len(), 1);
    let mut restored = restore(&host, now).unwrap();
    let again = sealed(&restored, &b, 1, b"sealed");
    restored.receive_from(origin(&b), again, now).ok();
    assert_eq!(
        held(&restored).len(),
        1,
        "the sequence floor survives restart"
    );
}

#[test]
fn a_plaintext_write_after_a_sealed_one_is_refused_as_a_downgrade() {
    let (mut host, a, b, now) = host();
    host.receive_from(origin(&b), sealed(&host, &b, 1, b"sealed"), now)
        .unwrap();
    let mut restored = restore(&host, now).unwrap();
    let plaintext = restored.publish_incremental(APP, RECORD, Vec::new(), &a, now);
    assert!(plaintext.is_err(), "the record stays encrypted");
}

#[test]
fn a_revoked_author_cannot_add_sealed_history() {
    use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};
    let (mut host, a, b, now) = host();
    let mut revocation = RevocationEntry {
        user_public_key: SigningKey::from_bytes(&[1; 32]).verifying_key().to_bytes(),
        revoked_device_public_key: b.verifying_key().to_bytes(),
        revoked_at_ns: 1,
        reason: RevocationReason::Compromised,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut revocation, &SigningKey::from_bytes(&[1; 32])).unwrap();
    host.receive_from(
        TransportOrigin::UntrustedPresence(a.verifying_key().to_bytes()),
        NeighbourMsg::IdentityHello {
            cert: Box::new(host.authority().clone()),
            revocations: vec![revocation],
        },
        now,
    )
    .unwrap();
    let before = held(&host);
    assert!(
        host.receive_from(origin(&b), sealed(&host, &b, 1, b"sealed"), now)
            .is_err()
    );
    assert_eq!(held(&host), before);
}

#[test]
fn a_full_sealed_history_refuses_rather_than_evicts() {
    let (mut host, _, b, now) = host();
    host.receive_from(origin(&b), sealed(&host, &b, 1, b"first"), now)
        .unwrap();
    let route = Route {
        app: APP.into(),
        record: RECORD.into(),
    };
    let first = held(&host)[0].clone();
    host.core.documents.get_mut(&route).unwrap().sealed = vec![first; MAX_SEALED_OPS];
    assert!(
        host.receive_from(origin(&b), sealed(&host, &b, 2, b"one more"), now)
            .unwrap_err()
            .contains("full")
    );
    assert_eq!(held(&host).len(), MAX_SEALED_OPS);
}

#[test]
fn a_tampered_stored_sealed_history_refuses_to_restore() {
    let (mut host, _, b, now) = host();
    host.receive_from(origin(&b), sealed(&host, &b, 1, b"sealed"), now)
        .unwrap();
    let bytes = host.durable().unwrap();
    let rewrite = |change: &dyn Fn(&mut SealedHistory)| {
        let mut value: DurableV6<'_> = postcard::from_bytes(&bytes).unwrap();
        change(&mut value.sealed);
        postcard::to_allocvec(&value).unwrap()
    };
    for (tampered, why) in [
        (
            rewrite(&|sealed| sealed[0].1[0].payload[0] ^= 1),
            "a changed ciphertext",
        ),
        (
            rewrite(&|sealed| sealed[0].1[0].seq = 9),
            "a write that was never admitted",
        ),
        (
            rewrite(&|sealed| sealed[0].0.record = "tab:2".into()),
            "history for another record",
        ),
        (rewrite(&|sealed| sealed.clear()), "an empty sealed section"),
    ] {
        assert!(
            restore_bytes(&host, &tampered, now).is_err(),
            "{why} restores"
        );
    }
    let mut cleartext = host.clone();
    let route = Route {
        app: APP.into(),
        record: RECORD.into(),
    };
    let mut op = held(&host)[0].clone();
    op.encryption = None;
    collab_types::sign_op(&mut op, &b).unwrap();
    cleartext.core.documents.get_mut(&route).unwrap().sealed = vec![op];
    assert!(
        restore(&cleartext, now).is_err(),
        "sealed history cannot carry plaintext"
    );
}
