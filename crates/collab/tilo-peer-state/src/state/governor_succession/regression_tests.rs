use super::*;
use crate::{Custody, TransportOrigin};
use tilo_automerge::ApplyIncoming;
use tilo_user_identity::{DeviceCapabilities, RevocationEntry, RevocationReason};

fn following_fixture() -> Fixture {
    let mut f = fixture();
    f.peer
        .receive_from(
            TransportOrigin::RelayDelta(f.writer.verifying_key().to_bytes()),
            NeighbourMsg::GovernorSuccession {
                room: f.peer.room().into(),
                records: vec![postcard::to_stdvec(&f.record).unwrap()],
            },
            f.now,
        )
        .unwrap();
    f
}

#[test]
fn legacy_contested_v7_restores_and_nested_envelopes_refuse() {
    let f = fixture();
    let envelope = crate::state::DurableContested {
        version: 7,
        contested: "historical competing successor".into(),
        state: f.peer.durable().unwrap(),
    };
    let bytes = postcard::to_stdvec(&envelope).unwrap();
    let restored = StatePeer::restore(
        &bytes,
        f.peer.room(),
        f.record.previous.clone(),
        f.peer.certificates(),
        f.peer.core.revocations.clone(),
        f.now,
    )
    .unwrap();
    assert_eq!(
        restored.contested_succession(),
        Some(envelope.contested.as_str())
    );
    assert_eq!(restored.authority(), f.peer.authority());
    assert_eq!(
        postcard::take_from_bytes::<u32>(&restored.durable().unwrap())
            .unwrap()
            .0,
        8
    );
    let nested = crate::state::DurableContested {
        state: bytes,
        ..envelope
    };
    assert!(
        StatePeer::restore(
            &postcard::to_stdvec(&nested).unwrap(),
            f.peer.room(),
            f.record.previous,
            f.peer.certificates(),
            f.peer.core.revocations.clone(),
            f.now,
        )
        .err()
        .expect("invalid durable state must refuse")
        .contains("invalid contested durable envelope")
    );
}

#[test]
fn relayed_follow_restores_with_previous_or_successor_already_pinned() {
    let f = following_fixture();
    let bytes = f.peer.durable().unwrap();
    assert_eq!(postcard::take_from_bytes::<u32>(&bytes).unwrap().0, 8);
    for pinned in [f.record.previous.clone(), f.record.successor.clone()] {
        let restored = StatePeer::restore(
            &bytes,
            f.peer.room(),
            pinned,
            f.peer.certificates(),
            f.peer.core.revocations.clone(),
            f.now,
        )
        .unwrap();
        assert_eq!(restored.authority(), &f.record.successor);
        assert!(restored.revocations().contains(&f.record.revocation));
        assert_eq!(
            restored.succession_message().unwrap(),
            f.peer.succession_message().unwrap()
        );
    }
}

#[test]
fn relayed_follow_restore_rechecks_tainted_history_under_both_pins() {
    let mut f = following_fixture();
    f.peer.core.room_doc = Some(reverted_governance(&f.peer, [95; 32]).into());
    let bytes = f.peer.durable().unwrap();
    for pinned in [f.record.previous.clone(), f.record.successor.clone()] {
        let error = StatePeer::restore(
            &bytes,
            f.peer.room(),
            pinned,
            f.peer.certificates(),
            f.peer.core.revocations.clone(),
            f.now,
        )
        .err()
        .expect("invalid durable state must refuse");
        assert!(
            error.contains("room governance changed after the succession cutoff"),
            "{error}"
        );
    }
}

#[test]
fn relayed_follow_restore_refuses_forged_link_even_with_successor_pinned() {
    let mut f = following_fixture();
    f.peer.core.followed[0].signature[0] ^= 1;
    let error = StatePeer::restore(
        &f.peer.durable().unwrap(),
        f.peer.room(),
        f.record.successor,
        f.peer.certificates(),
        f.peer.core.revocations.clone(),
        f.now,
    )
    .err()
    .expect("invalid durable state must refuse");
    assert!(
        error.contains("succession is not signed by the identity's root"),
        "{error}"
    );
}

#[test]
fn new_follow_envelope_refuses_empty_and_nested_metadata() {
    let f = following_fixture();
    let inner = f.peer.durable().unwrap();
    for envelope in [
        crate::state::DurableSuccession {
            version: 8,
            contested: None,
            followed: Vec::new(),
            state: fixture().peer.durable().unwrap(),
        },
        crate::state::DurableSuccession {
            version: 8,
            contested: None,
            followed: vec![f.record.clone()],
            state: inner,
        },
    ] {
        assert!(
            StatePeer::restore(
                &postcard::to_stdvec(&envelope).unwrap(),
                f.peer.room(),
                f.record.previous.clone(),
                f.peer.certificates(),
                f.peer.core.revocations.clone(),
                f.now,
            )
            .err()
            .expect("invalid durable state must refuse")
            .contains("invalid governor succession durable envelope")
        );
    }
}

#[test]
fn custody_refuses_successor_revocation_from_a_later_clean_source() {
    let mut f = fixture();
    let clean = f.peer.clone();
    let mut revoked = f.record.revocation.clone();
    revoked.revoked_device_public_key = f.record.successor.device_public_key;
    tilo_user_identity::sign_revocation_entry(&mut revoked, &SigningKey::from_bytes(&[1; 32]))
        .unwrap();
    f.peer.core.revocations.insert(revoked).unwrap();
    let error = StatePeer::take_custody(
        &f.record,
        [
            Custody::Replica(Box::new(clean)),
            Custody::Replica(Box::new(f.peer)),
        ],
        f.now,
    )
    .err()
    .expect("a clean early source must not hide a later verified revocation");
    assert!(error.contains("the successor is revoked"), "{error}");
}

#[test]
fn custody_refuses_reverted_foreign_topology_but_preserves_an_owner_tail() {
    let mut f = fixture();
    let owner = SigningKey::from_bytes(&[3; 32]).verifying_key().to_bytes();
    let mut initial = f.peer.admitted_room().unwrap().clone();
    initial.set_actor(automerge::ActorId::from(
        f.writer.verifying_key().to_bytes().to_vec(),
    ));
    let owners = initial
        .put_object(ROOT, "contents_owners_v1", ObjType::Map)
        .unwrap();
    initial.put(owners, "ffff", hex::encode(owner)).unwrap();
    let contents = initial.put_object(ROOT, "contents", ObjType::Map).unwrap();
    let strip = initial
        .put_object(contents, "ffff000000000001", ObjType::Map)
        .unwrap();
    initial.put(&strip, "name", "Before").unwrap();
    initial.commit();
    f.peer
        .receive_from(
            TransportOrigin::RelayDelta(f.writer.verifying_key().to_bytes()),
            message(initial, f.peer.room()),
            f.now,
        )
        .unwrap();
    f.record = GovernorSuccession::sign(
        f.peer.room(),
        f.record.previous,
        f.record.revocation,
        f.record.successor,
        f.peer
            .admitted_room()
            .unwrap()
            .clone()
            .get_heads()
            .into_iter()
            .map(|head| head.0)
            .collect(),
        &SigningKey::from_bytes(&[1; 32]),
    )
    .unwrap();
    for (actor, expected) in [([95; 32], false), (owner, true)] {
        let mut peer = f.peer.clone();
        let mut document = peer.admitted_room().unwrap().clone();
        document.set_actor(automerge::ActorId::from(actor.to_vec()));
        document.put(&strip, "name", "After").unwrap();
        document.commit();
        if !expected {
            document.put(&strip, "name", "Before").unwrap();
            document.commit();
        }
        peer.receive_from(
            TransportOrigin::RelayDelta(f.writer.verifying_key().to_bytes()),
            message(document, peer.room()),
            f.now,
        )
        .unwrap();
        assert_eq!(
            StatePeer::take_custody(&f.record, [Custody::Replica(Box::new(peer))], f.now).is_ok(),
            expected,
            "custody must retain topology ownership through reverted history"
        );
    }
}

struct Fixture {
    peer: StatePeer,
    writer: SigningKey,
    successor: SigningKey,
    record: GovernorSuccession,
    now: SystemTime,
}

fn fixture() -> Fixture {
    let (peer, writer, _, mut replacement, now) = crate::admission_tests::seeded();
    let identity = SigningKey::from_bytes(&[1; 32]);
    let successor = SigningKey::from_bytes(&[94; 32]);
    replacement.device_public_key = successor.verifying_key().to_bytes();
    replacement.capabilities = DeviceCapabilities::primary();
    replacement.expires_at_ns = Some(now_ns(now).checked_add(60_000_000_000).unwrap());
    tilo_user_identity::sign_device_cert(&mut replacement, &identity).unwrap();
    let previous = peer.authority().clone();
    let mut revoked = RevocationEntry {
        user_public_key: previous.user_public_key,
        revoked_device_public_key: previous.device_public_key,
        revoked_at_ns: now_ns(now),
        reason: RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revoked, &identity).unwrap();
    let cutoff = peer
        .admitted_room()
        .unwrap()
        .clone()
        .get_heads()
        .into_iter()
        .map(|head| head.0)
        .collect();
    let record = GovernorSuccession::sign(
        peer.room(),
        previous,
        revoked,
        replacement,
        cutoff,
        &identity,
    )
    .unwrap();
    Fixture {
        peer,
        writer,
        successor,
        record,
        now,
    }
}

fn message(mut document: AutoCommit, room: &str) -> NeighbourMsg {
    NeighbourMsg::SyncResp {
        room: room.to_owned(),
        changes: document
            .get_changes(&[])
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
    }
}

fn reverted_governance(peer: &StatePeer, actor: [u8; 32]) -> AutoCommit {
    let mut document = peer.admitted_room().unwrap().clone();
    let owner = room::governance_of(Some(&document)).0.unwrap();
    document.set_actor(automerge::ActorId::from(actor.to_vec()));
    document
        .put(ROOT, "owner", "untrusted-owner-after-cutoff")
        .unwrap();
    document.commit();
    document.put(ROOT, "owner", owner).unwrap();
    document.commit();
    assert!(room::governance_unchanged(peer.admitted_room(), &document));
    document
}

fn record_transition(document: &mut AutoCommit, record: &GovernorSuccession) {
    document.set_actor(automerge::ActorId::from(
        record.successor.device_public_key.to_vec(),
    ));
    let map = document
        .put_object(ROOT, succession::SUCCESSIONS_FIELD, ObjType::Map)
        .unwrap();
    document
        .put(
            &map,
            record.key(),
            automerge::ScalarValue::Bytes(postcard::to_stdvec(record).unwrap()),
        )
        .unwrap();
    document.commit();
}

#[test]
fn reverted_governance_cannot_be_taken_into_custody_or_assumed() {
    for actor in [
        [95; 32],
        SigningKey::from_bytes(&[94; 32]).verifying_key().to_bytes(),
    ] {
        let mut f = fixture();
        let document = reverted_governance(&f.peer, actor);
        f.peer
            .receive_from(
                TransportOrigin::RelayDelta(f.writer.verifying_key().to_bytes()),
                message(document, f.peer.room()),
                f.now,
            )
            .unwrap();
        let before = f.peer.durable().unwrap();
        assert!(
            StatePeer::take_custody(
                &f.record,
                [Custody::Replica(Box::new(f.peer.clone()))],
                f.now
            )
            .is_err()
        );
        assert!(
            f.peer
                .assume_governance(&f.record, &f.successor, f.now)
                .is_err()
        );
        assert_eq!(
            f.peer.durable().unwrap(),
            before,
            "failed assumption is atomic"
        );
    }
}

#[test]
fn retained_custody_rejects_tainted_history_and_uses_clean_fallback() {
    let f = fixture();
    let tainted = message(reverted_governance(&f.peer, [95; 32]), f.peer.room());
    let retained = vec![(f.writer.verifying_key().to_bytes(), tainted)];
    assert!(
        StatePeer::take_custody(&f.record, [Custody::Retained(retained.clone())], f.now).is_err()
    );
    let fallback = StatePeer::take_custody(
        &f.record,
        [
            Custody::Retained(retained),
            Custody::Replica(Box::new(f.peer.clone())),
        ],
        f.now,
    )
    .unwrap();
    assert_eq!(fallback.durable().unwrap(), f.peer.durable().unwrap());
}

#[test]
fn following_refuses_reverted_governance_before_or_concurrent_with_transition() {
    for concurrent in [false, true] {
        let mut f = fixture();
        let mut document = reverted_governance(&f.peer, [95; 32]);
        if concurrent {
            let mut governor = f.peer.clone();
            governor
                .assume_governance(&f.record, &f.successor, f.now)
                .unwrap();
            document
                .merge_incoming(&mut governor.admitted_room().unwrap().clone())
                .unwrap();
        } else {
            record_transition(&mut document, &f.record);
        }
        let before = f.peer.durable().unwrap();
        let error = f
            .peer
            .receive_from(
                TransportOrigin::RelayDelta(f.successor.verifying_key().to_bytes()),
                message(document, f.peer.room()),
                f.now,
            )
            .unwrap_err();
        assert!(
            error.contains("governance changed after the succession cutoff"),
            "{error}"
        );
        assert_eq!(
            f.peer.durable().unwrap(),
            before,
            "refused history does not repin or revoke"
        );
    }
}

#[test]
fn restore_rechecks_history_with_previous_or_successor_pinned() {
    let f = fixture();
    let mut governor = f.peer.clone();
    governor
        .assume_governance(&f.record, &f.successor, f.now)
        .unwrap();
    let mut tainted = reverted_governance(&f.peer, [95; 32]);
    tainted
        .merge_incoming(&mut governor.admitted_room().unwrap().clone())
        .unwrap();
    governor.core.room_doc = Some(tainted.into());
    let bytes = governor.durable().unwrap();
    for pinned in [&f.record.previous, &f.record.successor] {
        let error = StatePeer::restore(
            &bytes,
            f.peer.room(),
            pinned.clone(),
            vec![pinned.clone()],
            tilo_user_identity::RevocationList::new(),
            f.now,
        )
        .err()
        .expect("tampered durable history must fail closed");
        assert!(
            error.contains("governance changed after the succession cutoff"),
            "{error}"
        );
    }
}

#[test]
fn following_requires_transition_to_descend_from_the_signed_cutoff() {
    let mut f = fixture();
    let mut disconnected = AutoCommit::new();
    record_transition(&mut disconnected, &f.record);
    disconnected
        .merge_incoming(&mut f.peer.admitted_room().unwrap().clone())
        .unwrap();
    let before = f.peer.durable().unwrap();
    let error = f
        .peer
        .receive_from(
            TransportOrigin::RelayDelta(f.successor.verifying_key().to_bytes()),
            message(disconnected, f.peer.room()),
            f.now,
        )
        .unwrap_err();
    assert!(
        error.contains("transition does not descend from its cutoff"),
        "{error}"
    );
    assert_eq!(f.peer.durable().unwrap(), before);
}

#[test]
fn custody_and_assumption_reject_forgery_missing_cutoff_and_revoked_successor() {
    #[derive(Debug)]
    enum Failure {
        Forged,
        Missing,
        Revoked,
    }
    for failure in [Failure::Forged, Failure::Missing, Failure::Revoked] {
        let mut f = fixture();
        let clean = f.peer.clone();
        match failure {
            Failure::Forged => f.record.signature[0] ^= 1,
            Failure::Missing => {
                f.record = GovernorSuccession::sign(
                    f.peer.room(),
                    f.record.previous.clone(),
                    f.record.revocation.clone(),
                    f.record.successor.clone(),
                    vec![[77; 32]],
                    &SigningKey::from_bytes(&[1; 32]),
                )
                .unwrap();
            }
            Failure::Revoked => {
                let mut revoked = f.record.revocation.clone();
                revoked.revoked_device_public_key = f.record.successor.device_public_key;
                tilo_user_identity::sign_revocation_entry(
                    &mut revoked,
                    &SigningKey::from_bytes(&[1; 32]),
                )
                .unwrap();
                f.peer.core.revocations.insert(revoked).unwrap();
            }
        }
        let before = f.peer.durable().unwrap();
        assert!(
            StatePeer::take_custody(
                &f.record,
                [
                    Custody::Replica(Box::new(f.peer.clone())),
                    Custody::Replica(Box::new(clean)),
                ],
                f.now
            )
            .is_err(),
            "{failure:?}"
        );
        assert!(
            f.peer
                .assume_governance(&f.record, &f.successor, f.now)
                .is_err(),
            "{failure:?}"
        );
        assert_eq!(f.peer.durable().unwrap(), before, "{failure:?}");
    }
}

#[test]
fn member_tail_and_governance_echo_survive_two_successions_and_expired_history_restore() {
    let mut f = fixture();
    let member = SigningKey::from_bytes(&[3; 32]);
    let mut document = f.peer.admitted_room().unwrap().clone();
    document.set_actor(automerge::ActorId::from(
        member.verifying_key().to_bytes().to_vec(),
    ));
    document.put(ROOT, "topic", "member tail").unwrap();
    let owner = room::governance_of(Some(&document)).0.unwrap();
    document.put(ROOT, "owner", owner).unwrap();
    document.commit();
    f.peer
        .receive_from(
            TransportOrigin::RelayDelta(member.verifying_key().to_bytes()),
            message(document, f.peer.room()),
            f.now,
        )
        .unwrap();
    let mut governor = StatePeer::take_custody(
        &f.record,
        [Custody::Replica(Box::new(f.peer.clone()))],
        f.now,
    )
    .unwrap();
    governor
        .assume_governance(&f.record, &f.successor, f.now)
        .unwrap();
    let next_key = SigningKey::from_bytes(&[96; 32]);
    let root = SigningKey::from_bytes(&[1; 32]);
    let mut next_cert = f.record.successor.clone();
    next_cert.device_public_key = next_key.verifying_key().to_bytes();
    next_cert.expires_at_ns = None;
    tilo_user_identity::sign_device_cert(&mut next_cert, &root).unwrap();
    let mut revocation = f.record.revocation.clone();
    revocation.revoked_device_public_key = f.record.successor.device_public_key;
    tilo_user_identity::sign_revocation_entry(&mut revocation, &root).unwrap();
    let cutoff = governor
        .admitted_room()
        .unwrap()
        .clone()
        .get_heads()
        .into_iter()
        .map(|head| head.0)
        .collect();
    let second = GovernorSuccession::sign(
        governor.room(),
        f.record.successor.clone(),
        revocation,
        next_cert.clone(),
        cutoff,
        &root,
    )
    .unwrap();
    governor
        .assume_governance(&second, &next_key, f.now)
        .unwrap();
    f.peer
        .receive_from(
            TransportOrigin::RelayDelta(next_cert.device_public_key),
            governor.room_message(),
            f.now,
        )
        .unwrap();
    assert_eq!(f.peer.authority(), &next_cert);
    assert!(f.peer.revocations().contains(&f.record.revocation));
    assert!(f.peer.revocations().contains(&second.revocation));
    assert!(
        f.peer
            .receive_from(
                TransportOrigin::RelayDelta(f.writer.verifying_key().to_bytes()),
                message(reverted_governance(&f.peer, [95; 32]), f.peer.room()),
                f.now
            )
            .is_err()
    );
    let later = f.now + web_time::Duration::from_secs(120);
    for pinned in [&f.record.previous, &next_cert] {
        let restored = StatePeer::restore(
            &f.peer.durable().unwrap(),
            f.peer.room(),
            pinned.clone(),
            vec![pinned.clone()],
            tilo_user_identity::RevocationList::new(),
            later,
        )
        .unwrap();
        assert_eq!(restored.authority(), &next_cert);
        assert!(restored.revocations().contains(&f.record.revocation));
        assert!(restored.revocations().contains(&second.revocation));
        let (topic, _) = restored
            .admitted_room()
            .unwrap()
            .get(ROOT, "topic")
            .unwrap()
            .unwrap();
        assert_eq!(topic.as_str(), Some("member tail"));
        assert_eq!(
            room::governance_of(restored.admitted_room()),
            room::governance_of(f.peer.admitted_room())
        );
    }
}

#[test]
fn custody_refuses_governance_written_under_another_actor_after_cutoff() {
    let (mut peer, writer, _, member, now) = crate::admission_tests::seeded();
    let identity = SigningKey::from_bytes(&[1; 32]);
    let mut replacement = member;
    replacement.device_public_key = SigningKey::from_bytes(&[94; 32]).verifying_key().to_bytes();
    replacement.capabilities = DeviceCapabilities::primary();
    tilo_user_identity::sign_device_cert(&mut replacement, &identity).unwrap();
    let previous = peer.authority().clone();
    let mut revoked = RevocationEntry {
        user_public_key: previous.user_public_key,
        revoked_device_public_key: previous.device_public_key,
        revoked_at_ns: now_ns(now),
        reason: RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revoked, &identity).unwrap();
    let mut document = peer.admitted_room().unwrap().clone();
    let heads = document.get_heads();
    let room = peer.room().to_owned();
    let record = GovernorSuccession::sign(
        &room,
        previous,
        revoked,
        replacement,
        heads.iter().map(|head| head.0).collect(),
        &identity,
    )
    .unwrap();
    record.verify(&room, peer.authority(), now_ns(now)).unwrap();
    StatePeer::take_custody(&record, [Custody::Replica(Box::new(peer.clone()))], now).unwrap();
    // The governor signs before peers hear its revocation, but writes
    // governance under a different Automerge actor after the root's cutoff.
    document.set_actor(automerge::ActorId::from(vec![95; 32]));
    document
        .put(ROOT, "owner", "untrusted-owner-after-cutoff")
        .unwrap();
    document.commit();
    let changes = document
        .get_changes(&heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    peer.receive_from(
        TransportOrigin::RelayDelta(writer.verifying_key().to_bytes()),
        NeighbourMsg::SyncResp { changes, room },
        now,
    )
    .unwrap();
    assert!(
        StatePeer::take_custody(&record, [Custody::Replica(Box::new(peer))], now).is_err(),
        "custody must reject changed governance past the root-signed cutoff regardless of actor id"
    );
}
