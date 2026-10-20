//! A replica without the channel key keeps sealed writes as their record.
use super::*;
use crate::admission_tests::seeded;
use crate::{InputDisposition, TransportOrigin, peer_handle};
use collab_types::CommitNamed as _;

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
        OpKind::Change,
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
        .to_vec()
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
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(host.authority().clone()),
            revocations: vec![revocation],
            successions: Vec::new(),
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
    host.core.documents.get_mut(&route).unwrap().sealed = vec![first; MAX_SEALED_OPS].into();
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
    cleartext.core.documents.get_mut(&route).unwrap().sealed = vec![op].into();
    assert!(
        restore(&cleartext, now).is_err(),
        "sealed history cannot carry plaintext"
    );
}

/// The seeded governor, which holds the channel keys, and its key.
fn holder() -> (StatePeer, SigningKey, SystemTime) {
    let (mut native, a, _, _, now) = seeded();
    native.select_admission_v5().unwrap();
    (native, a, now)
}

fn envelope_of(message: NeighbourMsg) -> SignedOp {
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        panic!("a document write")
    };
    *envelope
}

/// An edit of the seeded record, encoded as the plaintext a sealed write opens to.
fn edit(peer: &StatePeer, value: &str) -> Vec<u8> {
    let mut doc = AutoCommit::load(&peer.document(APP, RECORD).unwrap()).unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, "value", value).unwrap();
    doc.commit_edit("fixture", "value");
    DocumentMutation::Incremental(doc.save_after(&heads))
        .encode()
        .unwrap()
}

fn value(peer: &StatePeer) -> String {
    let doc = AutoCommit::load(&peer.document(APP, RECORD).unwrap()).unwrap();
    doc.get(ROOT, "value")
        .unwrap()
        .map(|(value, _)| value.to_string())
        .unwrap()
}

#[test]
fn the_key_holder_admits_the_opened_plaintext_and_keeps_the_record_encrypted() {
    let (mut holder, a, now) = holder();
    let plaintext = edit(&holder, "opened by its key holder");
    let envelope = envelope_of(sealed(&holder, &a, 2, b"ciphertext"));
    holder
        .admit_new_opened_local_document(
            APP,
            RECORD,
            envelope.clone(),
            &mut |_| Ok(plaintext.clone()),
            now,
        )
        .unwrap();
    assert_eq!(value(&holder), "\"opened by its key holder\"");
    assert_eq!(holder.incomplete_reason(), None);
    let mut restored = restore(&holder, now).unwrap();
    assert_eq!(value(&restored), "\"opened by its key holder\"");
    let original = holder.durable().unwrap();
    let bounded = crate::BoundedStatePeer::restore(
        &original,
        holder.room(),
        holder.authority(),
        std::slice::from_ref(holder.authority()),
        &tilo_user_identity::RevocationList::new(),
        now,
        &crate::PeerLimits::default(),
    )
    .unwrap();
    let mut opened = 0;
    let capture = bounded
        .read()
        .capture_document(
            a.verifying_key().to_bytes(),
            (APP, RECORD),
            now,
            1024 * 1024,
            &mut |retained| {
                assert_eq!(
                    retained, &envelope,
                    "original signed ciphertext must survive restore"
                );
                opened += 1;
                Ok(plaintext.clone())
            },
        )
        .unwrap();
    assert_eq!(opened, 1);
    let mut replayed = automerge::AutoCommit::load(&capture.into_bytes()).unwrap();
    let mut admitted = automerge::AutoCommit::load(&holder.document(APP, RECORD).unwrap()).unwrap();
    assert_eq!(
        replayed.get_heads(),
        admitted.get_heads(),
        "the original change graph is replayed"
    );
    assert_eq!(
        holder.durable().unwrap(),
        original,
        "a read does not publish or rewrite state"
    );

    assert!(
        restored
            .publish_incremental(APP, RECORD, Vec::new(), &a, now)
            .is_err(),
        "a plaintext write to the encrypted record is a downgrade"
    );
}

#[test]
fn the_key_holder_never_opens_a_write_that_fails_its_checks() {
    use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};
    let (holder, a, now) = holder();
    let plaintext = edit(&holder, "never");
    let refused = |mut peer: StatePeer, envelope: SignedOp, why: &str| {
        let before = peer.durable().unwrap();
        let mut opened = 0;
        let result = peer.admit_new_opened_local_document(
            APP,
            RECORD,
            envelope,
            &mut |_| {
                opened += 1;
                Ok(plaintext.clone())
            },
            now,
        );
        assert!(result.is_err(), "{why} is admitted");
        assert_eq!(opened, 0, "{why} was opened before it was verified");
        assert_eq!(peer.durable().unwrap(), before, "{why} changed the record");
    };
    let mut tampered = envelope_of(sealed(&holder, &a, 2, b"ciphertext"));
    tampered.payload[0] ^= 1;
    refused(holder.clone(), tampered, "tampered ciphertext");
    let stranger = SigningKey::from_bytes(&[77; 32]);
    refused(
        holder.clone(),
        envelope_of(sealed(&holder, &stranger, 2, b"ciphertext")),
        "a stranger's write",
    );
    let mut forged = envelope_of(sealed(&holder, &stranger, 2, b"ciphertext"));
    forged.author = Ed25519PublicKey::from_bytes(a.verifying_key().to_bytes());
    refused(holder.clone(), forged, "a write signed by another key");
    let mut once = holder.clone();
    let write = envelope_of(sealed(&holder, &a, 2, b"ciphertext"));
    once.admit_new_opened_local_document(
        APP,
        RECORD,
        write.clone(),
        &mut |_| Ok(plaintext.clone()),
        now,
    )
    .unwrap();
    refused(once, write, "a replayed write");
    let mut plain = envelope_of(sealed(&holder, &a, 2, b"ciphertext"));
    plain.encryption = None;
    plain.payload = plaintext.clone();
    collab_types::sign_op(&mut plain, &a).unwrap();
    refused(holder.clone(), plain, "a cleartext write");
    let (member_key, member) = {
        let (_, _, b, bc, _) = seeded();
        (b, bc)
    };
    let mut revoked = holder;
    let mut revocation = RevocationEntry {
        user_public_key: member.user_public_key,
        revoked_device_public_key: member.device_public_key,
        revoked_at_ns: 1,
        reason: RevocationReason::Compromised,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut revocation, &SigningKey::from_bytes(&[1; 32])).unwrap();
    revoked
        .receive_from(
            TransportOrigin::UntrustedPresence(a.verifying_key().to_bytes()),
            NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(revoked.authority().clone()),
                revocations: vec![revocation],
                successions: Vec::new(),
            },
            now,
        )
        .unwrap();
    refused(
        revoked.clone(),
        envelope_of(sealed(&revoked, &member_key, 1, b"ciphertext")),
        "a revoked author's write",
    );
}

#[test]
fn the_key_holder_admits_nothing_its_opener_refuses_or_cannot_parse() {
    let (holder, a, now) = holder();
    for (answer, why) in [
        (Err("stale key epoch".to_owned()), "an opener refusal"),
        (
            Ok(b"not a document mutation".to_vec()),
            "unparseable plaintext",
        ),
    ] {
        let mut peer = holder.clone();
        let before = peer.durable().unwrap();
        let envelope = envelope_of(sealed(&holder, &a, 2, b"ciphertext"));
        assert!(
            peer.admit_new_opened_local_document(
                APP,
                RECORD,
                envelope,
                &mut |_| answer.clone(),
                now
            )
            .is_err(),
            "{why} is admitted"
        );
        assert_eq!(peer.durable().unwrap(), before, "{why} changed the record");
    }
}

#[test]
fn a_keyless_owner_keeps_custody_of_its_own_sealed_write_without_renewing_it() {
    let (mut member, _, b, now) = host();
    let author = b.verifying_key().to_bytes();
    let write = sealed(&member, &b, 1, b"sealed by this member's app");
    member
        .admit_new_local_document(APP, RECORD, envelope_of(write.clone()), now)
        .unwrap();
    assert!(
        member
            .outbox()
            .lookup_retained_local_document(&write, author)
            .unwrap()
            .is_some()
    );
    assert!(
        !member
            .outbox()
            .retained_local_document_retired(&write, author)
            .unwrap(),
        "only the governor's signed watermark retires it"
    );
    let later = now.checked_add(Duration::from_secs(600)).unwrap();
    let projection = member
        .outbox_mut()
        .reconcile_local_recovery_projection(std::slice::from_ref(&write), None, &[], &b, later)
        .unwrap();
    assert!(
        projection.is_empty(),
        "a sealed write is never re-signed by a replica without its key"
    );
    assert!(
        member
            .outbox()
            .reserve_local_recovery(std::slice::from_ref(&write), None, author)
            .unwrap()
            .entries()
            == 0,
        "no recovery is reserved for history nobody here can read"
    );
}

/// A sealed history's durable parts are the whole encoding cut apart: the
/// record's parts are encoded once per state of it, and the parts restore
/// what the whole bytes restore.
#[test]
fn sealed_durable_parts_are_the_whole_encoding_and_restore_the_same() {
    let (mut host, _, b, now) = host();
    for seq in 1..=(3 * 64 + 5) {
        let write = sealed(&host, &b, seq, format!("sealed write {seq}").as_bytes());
        host.receive_from(origin(&b), write, now).unwrap();
    }
    let whole = host.durable_whole().unwrap();
    assert_eq!(whole[0], 6);
    assert_eq!(host.durable().unwrap(), whole);
    let before = host.durable_parts().unwrap();
    assert_eq!(before.concat(), whole);
    let again = host.durable_parts().unwrap();
    assert!(
        before
            .iter()
            .zip(&again)
            .filter(|(a, b)| std::sync::Arc::ptr_eq(a, b))
            .count()
            >= 3,
        "the record, its sealed history and the settled index are encoded once"
    );
    let write = sealed(&host, &b, 3 * 64 + 6, b"one more");
    host.receive_from(origin(&b), write, now).unwrap();
    assert_eq!(host.durable().unwrap(), host.durable_whole().unwrap());
    let restored = restore_bytes(&host, &host.durable_parts().unwrap().concat(), now).unwrap();
    assert_eq!(held(&restored), held(&host));
    assert_eq!(held(&restored).len(), 3 * 64 + 6);
}
