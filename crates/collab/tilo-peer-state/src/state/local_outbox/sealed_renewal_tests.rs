//! A keyless member's own sealed write, renewed through its key holder.
use super::*;
use crate::{InputDisposition, TransportOrigin};
use collab_types::EncryptionMetadata;
use tilo_user_identity::{DeviceCert, RevocationList};

const APP: &str = "test.app";
const RECORD: &str = "tab:1";

struct Fixture {
    governor: StatePeer,
    member: StatePeer,
    governor_key: SigningKey,
    key: SigningKey,
    cert: DeviceCert,
    now: SystemTime,
    write: NeighbourMsg,
}

/// A member holding its own sealed write, which never reached the governor.
fn fixture() -> Fixture {
    let (mut governor, governor_key, key, cert, now) = crate::admission_tests::seeded();
    governor.select_admission_v5().unwrap();
    let mut member = StatePeer::new(
        governor.room(),
        governor.authority().clone(),
        vec![cert.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    member.select_admission_v5().unwrap();
    for message in governor.bootstrap_messages(&governor_key, now).unwrap() {
        member
            .receive_from(origin(&governor_key), message, now)
            .unwrap();
    }
    let write = sealed(&member, &key, now, b"sealed by this member's app");
    member
        .admit_new_local_document(APP, RECORD, envelope(&write).clone(), now)
        .unwrap();
    Fixture {
        governor,
        member,
        governor_key,
        key,
        cert,
        now,
        write,
    }
}

fn origin(key: &SigningKey) -> TransportOrigin {
    TransportOrigin::RelayDelta(key.verifying_key().to_bytes())
}

fn metadata(peer: &StatePeer, epoch: u64) -> EncryptionMetadata {
    EncryptionMetadata::content_key(
        epoch,
        collab_types::document_channel_id(APP, peer.room(), RECORD),
        collab_types::ContentRetention::Delta,
    )
}

fn sealed(peer: &StatePeer, key: &SigningKey, at: SystemTime, ciphertext: &[u8]) -> NeighbourMsg {
    let author = Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
    let mut envelope = SignedOp::new_unsigned_with_encryption(
        ciphertext.to_vec(),
        collab_types::document_artifact_id(APP, peer.room(), RECORD),
        author,
        collab_types::document_attestation_id(APP, peer.room(), RECORD),
        OpKind::Change,
        at,
        1,
        metadata(peer, 1),
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

fn envelope(message: &NeighbourMsg) -> &SignedOp {
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        panic!("a document write")
    };
    envelope
}

/// The key holder's answer while the write's epoch is current.
fn identical(op: &SignedOp) -> Result<(EncryptionMetadata, Vec<u8>), String> {
    Ok((op.encryption.ok_or("not sealed")?, op.payload.clone()))
}

fn expired(now: SystemTime) -> SystemTime {
    now.checked_add(Duration::from_secs(301)).unwrap()
}

fn held(peer: &StatePeer) -> Vec<SignedOp> {
    peer.core.documents[&Route {
        app: APP.into(),
        record: RECORD.into(),
    }]
        .sealed
        .clone()
}

/// The governor admits `message` and records, in governance `member`
/// admits, its signed durable receipt over the record's watermarks. The
/// member holds no key for the record, so that receipt, not a cleartext
/// checkpoint, is what acknowledges its write.
fn acknowledge(fixture: &mut Fixture, message: NeighbourMsg, at: SystemTime) {
    let payload = envelope(&message).payload.clone();
    let report = fixture
        .governor
        .receive_from_report(origin(&fixture.key), message, at)
        .unwrap();
    assert!(matches!(report.primary(), InputDisposition::Accepted(_)));
    let governor = &mut fixture.governor;
    let authority = governor.authority().device_public_key;
    let document = &governor.core.documents[&Route {
        app: APP.into(),
        record: RECORD.into(),
    }];
    let mut watermarks: BTreeMap<String, u64> = document
        .channel
        .last_seq_per_author
        .iter()
        .map(|(author, seq)| (hex::encode(author.as_bytes()), *seq))
        .collect();
    let sequence = watermarks[&hex::encode(authority)].checked_add(1).unwrap();
    watermarks.insert(hex::encode(authority), sequence);
    let coverage = crate::coverage::Coverage {
        room: governor.room().into(),
        app: APP.into(),
        record: RECORD.into(),
        payload_hash: *blake3::hash(&payload).as_bytes(),
        heads: document
            .doc
            .base_heads()
            .iter()
            .map(ToString::to_string)
            .collect(),
        author: authority,
        sequence,
        watermarks,
    };
    let room = governor.core.room_doc.as_deref_mut().unwrap();
    let generation = crate::durability::entries(room)
        .unwrap()
        .values()
        .map(|receipt| receipt.generation)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .unwrap();
    crate::coverage::put_all_with_receipts(room, vec![coverage], |rows| {
        rows.iter()
            .map(|(route, row)| {
                Ok((
                    route.clone(),
                    crate::durability::DurableReceipt::new(row.clone(), generation)?,
                ))
            })
            .collect()
    })
    .unwrap();
    let governance = governor.room_message();
    fixture
        .member
        .receive_from(origin(&fixture.governor_key), governance, at)
        .unwrap();
}

#[test]
fn an_expired_sealed_write_renews_its_ciphertext_under_the_next_sequence_and_retires_with_it() {
    let mut fixture = fixture();
    let author = fixture.key.verifying_key().to_bytes();
    let later = expired(fixture.now);
    let renewal = fixture
        .member
        .renew_local_sealed(&fixture.write, &mut identical, &fixture.key, later)
        .unwrap()
        .expect("an expired unretired sealed write renews");
    let (original, renewed) = (envelope(&fixture.write), envelope(&renewal));
    collab_types::verify_op(renewed).unwrap();
    assert_eq!(
        renewed.payload, original.payload,
        "the ciphertext is identical"
    );
    assert_eq!(renewed.encryption, original.encryption);
    assert_eq!(
        (
            renewed.artifact,
            renewed.attestation_id,
            renewed.kind,
            renewed.author
        ),
        (
            original.artifact,
            original.attestation_id,
            original.kind,
            original.author
        )
    );
    assert_eq!(renewed.seq, original.seq.checked_add(1).unwrap());
    assert_eq!(renewed.timestamp, later);
    assert!(crate::outbox::fresh(&renewal, later));
    assert!(
        fixture
            .member
            .lookup_exact_local_document(APP, RECORD, renewed, later)
            .unwrap()
            .is_some(),
        "the renewal is admitted exactly, as a new local sealed write"
    );
    assert_eq!(held(&fixture.member).last(), Some(renewed));
    assert_eq!(
        fixture
            .member
            .renew_local_sealed(&renewal, &mut identical, &fixture.key, later)
            .unwrap(),
        None,
        "a fresh renewal is not renewed again"
    );
    assert!(
        !fixture
            .member
            .retained_local_document_retired(&renewal, author)
            .unwrap()
    );

    acknowledge(&mut fixture, renewal.clone(), later);
    for message in [&renewal, &fixture.write] {
        assert!(
            fixture
                .member
                .retained_local_document_retired(message, author)
                .unwrap(),
            "the governor's watermark over the renewal retires both"
        );
    }
    assert_eq!(
        fixture
            .member
            .renew_local_sealed(&fixture.write, &mut identical, &fixture.key, later)
            .unwrap(),
        None,
        "a retired write is not renewed"
    );
}

#[test]
fn a_write_from_an_older_epoch_renews_with_what_the_key_holder_resealed() {
    let mut fixture = fixture();
    let later = expired(fixture.now);
    let current = metadata(&fixture.member, 2);
    let renewal = fixture
        .member
        .renew_local_sealed(
            &fixture.write,
            &mut |_| Ok((current, b"resealed under epoch 2".to_vec())),
            &fixture.key,
            later,
        )
        .unwrap()
        .unwrap();
    assert_eq!(envelope(&renewal).encryption, Some(current));
    assert_eq!(envelope(&renewal).payload, b"resealed under epoch 2");
    collab_types::verify_op(envelope(&renewal)).unwrap();

    let before = fixture.member.durable().unwrap();
    let mut foreign = metadata(&fixture.member, 2);
    foreign.channel = collab_types::ChannelId::new();
    assert!(
        fixture
            .member
            .renew_local_sealed(
                &fixture.write,
                &mut |_| Ok((foreign, b"another channel".to_vec())),
                &fixture.key,
                expired(later),
            )
            .is_err(),
        "a renewal cannot move to another channel"
    );
    assert_eq!(fixture.member.durable().unwrap(), before);
}

#[test]
fn a_refused_reseal_or_a_fresh_write_changes_nothing() {
    let mut fixture = fixture();
    let before = fixture.member.durable().unwrap();
    let mut asked = 0;
    assert_eq!(
        fixture
            .member
            .renew_local_sealed(
                &fixture.write,
                &mut |_| {
                    asked += 1;
                    identical(envelope(&fixture.write))
                },
                &fixture.key,
                fixture.now,
            )
            .unwrap(),
        None,
        "a fresh write is not renewed"
    );
    assert_eq!(asked, 0);
    assert!(
        fixture
            .member
            .renew_local_sealed(
                &fixture.write,
                &mut |_| Err("this device holds no current key".into()),
                &fixture.key,
                expired(fixture.now),
            )
            .is_err()
    );
    assert_eq!(
        fixture.member.durable().unwrap(),
        before,
        "a refusal leaves the write exactly as it was"
    );
}

#[test]
fn a_revoked_author_renews_nothing() {
    let mut fixture = fixture();
    let later = expired(fixture.now);
    let mut revocation = tilo_user_identity::RevocationEntry {
        user_public_key: fixture.cert.user_public_key,
        revoked_device_public_key: fixture.cert.device_public_key,
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revocation, &SigningKey::from_bytes(&[1; 32]))
        .unwrap();
    fixture
        .member
        .receive_from(
            TransportOrigin::UntrustedPresence(fixture.governor_key.verifying_key().to_bytes()),
            NeighbourMsg::IdentityHello {
                cert: Box::new(fixture.governor.authority().clone()),
                revocations: vec![revocation],
            },
            later,
        )
        .unwrap();
    let before = fixture.member.durable().unwrap();
    let mut asked = 0;
    assert!(
        fixture
            .member
            .renew_local_sealed(
                &fixture.write,
                &mut |op| {
                    asked += 1;
                    identical(op)
                },
                &fixture.key,
                later,
            )
            .is_err()
    );
    assert_eq!(asked, 0, "nothing is resealed for a revoked author");
    assert_eq!(fixture.member.durable().unwrap(), before);
}

#[test]
fn a_renewal_survives_restart_as_exact_custody() {
    let mut fixture = fixture();
    let later = expired(fixture.now);
    let renewal = fixture
        .member
        .renew_local_sealed(&fixture.write, &mut identical, &fixture.key, later)
        .unwrap()
        .unwrap();
    let mut restored = StatePeer::restore(
        &fixture.member.durable().unwrap(),
        fixture.member.room(),
        fixture.member.authority().clone(),
        vec![fixture.cert.clone()],
        RevocationList::new(),
        later,
    )
    .unwrap();
    let author = fixture.key.verifying_key().to_bytes();
    for message in [&fixture.write, &renewal] {
        assert!(
            restored
                .lookup_retained_local_document(message, author)
                .unwrap()
                .is_some()
        );
    }
    assert_eq!(held(&restored), held(&fixture.member));
    assert_eq!(
        restored
            .renew_local_sealed(&renewal, &mut identical, &fixture.key, later)
            .unwrap(),
        None,
        "the restored renewal is still fresh"
    );
    fixture.member = restored;
    acknowledge(&mut fixture, renewal.clone(), later);
    assert!(
        fixture
            .member
            .retained_local_document_retired(&renewal, author)
            .unwrap()
    );
}
