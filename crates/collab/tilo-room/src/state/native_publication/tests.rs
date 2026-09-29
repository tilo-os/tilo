use super::*;
use crate::InputDisposition;
use crate::SeedState;
use crate::seed_room;
use crate::state::now_ns;
use automerge::AutoCommit;
use automerge::ROOT;
use automerge::transaction::Transactable;
use ed25519_dalek::SigningKey;
use tilo_user_identity::RevocationList;
use web_time::Duration;

fn native(peer: &StatePeer) -> TransportOrigin {
    TransportOrigin::RetainedNativePublication {
        publisher: peer.authority().device_public_key,
    }
}
fn revocation(target: [u8; 32]) -> tilo_user_identity::RevocationEntry {
    let root = SigningKey::from_bytes(&[1; 32]);
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: target,
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    entry
}
#[test]
fn native_publication_retained_bootstrap_preserves_relay_live_freshness_distinction() {
    let (mut host, key, _, cert, now) = crate::admission_tests::seeded();
    let later = now + Duration::from_secs(301);
    let mut browser = StatePeer::new(
        host.room(),
        host.authority().clone(),
        vec![cert],
        RevocationList::new(),
        later,
    )
    .unwrap();
    let origin = native(&browser);
    let messages = host.bootstrap_messages(&key, now).unwrap();
    let mut live = browser.clone();
    let mut live_rejected_document = false;
    for message in messages {
        let result = live.receive_from(
            TransportOrigin::RelayDelta(host.authority().device_public_key),
            message.clone(),
            later,
        );
        if matches!(message, NeighbourMsg::AppDocument { .. }) {
            live_rejected_document |= result.is_err();
        }
        browser.receive_from(origin, message, later).unwrap();
    }
    assert!(live_rejected_document);
    assert_eq!(
        browser.document_heads("test.app", "tab:1"),
        host.document_heads("test.app", "tab:1")
    );
}
#[test]
fn native_publication_preserves_original_member_author_and_exact_retry() {
    let (mut host, _, member, _, now) = crate::admission_tests::seeded();
    host.select_admission_v5().unwrap();
    let mut receiver = host.clone();
    let mut doc = AutoCommit::load(&host.document("test.app", "tab:1").unwrap()).unwrap();
    let before = doc.get_heads();
    doc.put(ROOT, "member-published", "original signed operation")
        .unwrap();
    doc.commit();
    let message = host
        .publish_incremental("test.app", "tab:1", doc.save_after(&before), &member, now)
        .unwrap();
    let origin = native(&receiver);
    assert!(
        receiver
            .clone()
            .receive_from_report(
                TransportOrigin::RelayDelta(receiver.authority().device_public_key),
                message.clone(),
                now
            )
            .is_err()
    );
    let admitted = receiver
        .receive_from_report(origin, message.clone(), now)
        .unwrap();
    assert!(matches!(admitted.primary, InputDisposition::Accepted(_)));
    assert_eq!(
        receiver.document_heads("test.app", "tab:1"),
        host.document_heads("test.app", "tab:1")
    );
    let exact = receiver
        .lookup_exact_document(origin, &message, now + Duration::from_secs(301))
        .unwrap()
        .unwrap();
    let retry = receiver
        .receive_from_report(origin, message.clone(), now + Duration::from_secs(301))
        .unwrap();
    assert!(matches!(retry.primary, InputDisposition::Duplicate(ref found) if found == &exact));
    let mut altered = message;
    if let NeighbourMsg::AppDocument { envelope, .. } = &mut altered {
        envelope.signature.0[0] ^= 1;
    }
    assert!(receiver.receive_from_report(origin, altered, now).is_err());
}
#[test]
fn native_publication_rechecks_pinned_publisher_and_original_author_revocations() {
    let (mut host, _, member, member_cert, now) = crate::admission_tests::seeded();
    let baseline = host.clone();
    let mut doc = AutoCommit::load(&host.document("test.app", "tab:1").unwrap()).unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, "member-change", "retained signed member packet")
        .unwrap();
    doc.commit();
    let message = host
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), &member, now)
        .unwrap();
    let origin = native(&baseline);
    assert!(
        baseline
            .clone()
            .receive_from(
                TransportOrigin::RetainedNativePublication {
                    publisher: member_cert.device_public_key
                },
                message.clone(),
                now
            )
            .is_err()
    );
    // A genuine same-root signed revocation is independently received from an
    // active other device, then the retained packet is rechecked against it.
    for target in [
        member_cert.device_public_key,
        baseline.authority().device_public_key,
    ] {
        let mut receiver = baseline.clone();
        let (sender, cert) = if target == member_cert.device_public_key {
            (
                receiver.authority().device_public_key,
                receiver.authority().clone(),
            )
        } else {
            (member_cert.device_public_key, member_cert.clone())
        };
        receiver
            .receive_from(
                TransportOrigin::RelayDelta(sender),
                NeighbourMsg::IdentityHello {
                    cert: Box::new(cert),
                    revocations: vec![revocation(target)],
                },
                now,
            )
            .unwrap();
        let old = receiver.document_heads("test.app", "tab:1");
        assert!(receiver.receive_from(origin, message.clone(), now).is_err());
        assert_eq!(receiver.document_heads("test.app", "tab:1"), old);
    }
}
#[test]
fn native_publication_closed_families_do_not_promote_presence_or_requests() {
    let (mut peer, _, _, _, now) = crate::admission_tests::seeded();
    let origin = native(&peer);
    let before = peer.durable().unwrap();
    for message in [
        NeighbourMsg::SyncReq {
            since: vec![],
            room: peer.room().into(),
            invite_id: None,
        },
        NeighbourMsg::PresenceCleared {
            peer: 0,
            key: "cursor".into(),
        },
    ] {
        assert!(peer.receive_from(origin, message, now).is_err());
    }
    assert_eq!(peer.durable().unwrap(), before);
}

#[test]
fn native_publication_identity_keeps_original_certificate_signature_checks() {
    let (mut peer, _, _, cert, now) = crate::admission_tests::seeded();
    let origin = native(&peer);
    peer.receive_from(
        origin,
        NeighbourMsg::IdentityHello {
            cert: Box::new(cert.clone()),
            revocations: vec![revocation([99; 32])],
        },
        now,
    )
    .unwrap();
    assert!(
        peer.revocations()
            .iter()
            .any(|entry| entry.revoked_device_public_key == [99; 32])
    );
    let mut bad = cert;
    bad.signature[0] ^= 1;
    assert!(
        peer.receive_from(
            origin,
            NeighbourMsg::IdentityHello {
                cert: Box::new(bad),
                revocations: vec![]
            },
            now
        )
        .is_err()
    );
}

#[test]
fn native_publication_current_pinned_governor_expiry_is_mandatory() {
    let (peer, key, _, other, now) = crate::admission_tests::seeded();
    let mut authority = peer.authority().clone();
    authority.expires_at_ns = Some(now_ns(now + Duration::from_secs(1)));
    tilo_user_identity::sign_device_cert(&mut authority, &SigningKey::from_bytes(&[1; 32]))
        .unwrap();
    let mut short = StatePeer::new(
        peer.room(),
        authority.clone(),
        vec![other.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    short
        .install_seed(
            SeedState {
                room_changes: seed_room(peer.room(), &authority, &[authority.clone(), other])
                    .unwrap(),
                documents: vec![],
            },
            &key,
            now,
        )
        .unwrap();
    let origin = native(&short);
    let message = short.room_message();
    assert!(short.receive_from(origin, message.clone(), now).is_ok());
    assert!(
        short
            .receive_from(origin, message, now + Duration::from_secs(2))
            .unwrap_err()
            .contains("expired")
    );
}

fn assert_deferred_governor_revalidation(reopen: bool) {
    let (mut source, _, member, member_cert, now) = crate::admission_tests::seeded();
    let baseline = source.clone();
    let mut doc = AutoCommit::load(&source.document("test.app", "tab:1").unwrap()).unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, "predecessor", true).unwrap();
    doc.commit();
    let predecessor = source
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), &member, now)
        .unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, "native-descendant", true).unwrap();
    doc.commit();
    let descendant = source
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), &member, now)
        .unwrap();
    {
        let mut receiver = baseline;
        let origin = native(&receiver);
        assert!(receiver.receive_from(origin, descendant, now).is_err());
        assert_eq!(receiver.pending_count(), 0);
        if reopen {
            receiver = StatePeer::restore(
                &receiver.durable().unwrap(),
                receiver.room(),
                receiver.authority().clone(),
                receiver.certificates(),
                RevocationList::new(),
                now,
            )
            .unwrap();
            assert_eq!(receiver.pending_count(), 0);
        }
        receiver
            .receive_from(
                TransportOrigin::RelayDelta(member_cert.device_public_key),
                NeighbourMsg::IdentityHello {
                    cert: Box::new(member_cert.clone()),
                    revocations: vec![revocation(receiver.authority().device_public_key)],
                },
                now,
            )
            .unwrap();
        receiver
            .receive_from(
                TransportOrigin::RelayDelta(member_cert.device_public_key),
                predecessor,
                now,
            )
            .unwrap();
        let doc = AutoCommit::load(&receiver.document("test.app", "tab:1").unwrap()).unwrap();
        assert!(doc.get(ROOT, "predecessor").unwrap().is_some());
        assert!(
            doc.get(ROOT, "native-descendant").unwrap().is_none(),
            "publisher-revoked native packet settled through another input; reopened={reopen}"
        );
    }
}

#[test]
fn native_publication_deferred_packet_rechecks_governor() {
    assert_deferred_governor_revalidation(false);
}
#[test]
fn native_publication_deferred_packet_rechecks_governor_after_restore() {
    assert_deferred_governor_revalidation(true);
}

#[test]
fn native_publication_streaming_cut_accepts_complete_out_of_order_original_packets() {
    let (mut source, _, member, _, now) = crate::admission_tests::seeded();
    let receiver = source.clone();
    let mut doc = AutoCommit::load(&source.document("test.app", "tab:1").unwrap()).unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, "parent", true).unwrap();
    doc.commit();
    let parent = source
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), &member, now)
        .unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, "child", true).unwrap();
    doc.commit();
    let child = source
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), &member, now)
        .unwrap();
    let publisher = receiver.authority().device_public_key;
    let old_heads = receiver.document_heads("test.app", "tab:1");
    let incomplete = receiver
        .begin_native_publication_cut(publisher, now)
        .unwrap()
        .push(child.clone(), now)
        .unwrap();
    assert!(incomplete.finish(now).is_err());
    assert_eq!(receiver.document_heads("test.app", "tab:1"), old_heads);
    let cut = receiver
        .begin_native_publication_cut(publisher, now)
        .unwrap()
        .push(child, now)
        .unwrap();
    assert_eq!(receiver.pending_count(), 0);
    let installed = cut.push(parent, now).unwrap().finish(now).unwrap();
    assert_eq!(installed.pending_count(), 0);
    assert_eq!(
        installed.document_heads("test.app", "tab:1"),
        source.document_heads("test.app", "tab:1")
    );
    assert_eq!(receiver.document_heads("test.app", "tab:1"), old_heads);
}

#[test]
fn native_publication_streaming_cut_rolls_back_on_governor_revocation() {
    let (receiver, _, _, member_cert, now) = crate::admission_tests::seeded();
    let publisher = receiver.authority().device_public_key;
    let before = receiver.durable().unwrap();
    let cut = receiver
        .begin_native_publication_cut(publisher, now)
        .unwrap();
    assert!(
        cut.push(
            NeighbourMsg::IdentityHello {
                cert: Box::new(member_cert),
                revocations: vec![revocation(publisher)]
            },
            now
        )
        .is_err()
    );
    assert_eq!(receiver.durable().unwrap(), before);
}

#[test]
fn native_publication_streaming_cut_checks_expiry_at_push_and_finish() {
    let (peer, _, _, other, now) = crate::admission_tests::seeded();
    let mut authority = peer.authority().clone();
    authority.expires_at_ns = Some(now_ns(now + Duration::from_secs(1)));
    tilo_user_identity::sign_device_cert(&mut authority, &SigningKey::from_bytes(&[1; 32]))
        .unwrap();
    let short = StatePeer::new(
        peer.room(),
        authority.clone(),
        vec![other.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let message = NeighbourMsg::IdentityHello {
        cert: Box::new(other),
        revocations: vec![],
    };
    let expired = now + Duration::from_secs(2);
    let cut = short
        .begin_native_publication_cut(authority.device_public_key, now)
        .unwrap();
    assert!(cut.push(message.clone(), expired).is_err());
    let cut = short
        .begin_native_publication_cut(authority.device_public_key, now)
        .unwrap()
        .push(message, now)
        .unwrap();
    assert!(cut.finish(expired).is_err());
}

#[test]
fn native_publication_streaming_cut_preserves_preexisting_relay_pending() {
    let (mut author, _, member, cert, now) = crate::admission_tests::seeded();
    let mut receiver = author.clone();
    let mut doc = AutoCommit::load(&author.document("test.app", "tab:1").unwrap()).unwrap();
    let before = doc.get_heads();
    doc.put(ROOT, "pending-parent", "parent").unwrap();
    doc.commit();
    let _parent = author
        .publish_incremental("test.app", "tab:1", doc.save_after(&before), &member, now)
        .unwrap();
    let before = doc.get_heads();
    doc.put(ROOT, "pending-child", "child").unwrap();
    doc.commit();
    let child = author
        .publish_incremental("test.app", "tab:1", doc.save_after(&before), &member, now)
        .unwrap();
    receiver
        .receive_from(
            TransportOrigin::RelayDelta(cert.device_public_key),
            child,
            now,
        )
        .unwrap();
    assert_eq!(receiver.pending_count(), 1);
    let before = pending_stamp(receiver.core.pending.front().unwrap()).unwrap();
    let result = receiver
        .begin_native_publication_cut(receiver.core.authority.device_public_key, now)
        .unwrap()
        .push(
            NeighbourMsg::IdentityHello {
                cert: Box::new(cert),
                revocations: vec![],
            },
            now,
        )
        .unwrap()
        .finish(now)
        .unwrap();
    assert_eq!(result.pending_count(), 1);
    assert_eq!(
        pending_stamp(result.core.pending.front().unwrap()).unwrap(),
        before
    );
    // A malformed in-memory caller state cannot truncate the fixed baseline.
    let item = receiver.core.pending.front().unwrap().clone();
    for _ in 0..64 {
        receiver.core.pending.push_back(item.clone());
    }
    assert!(
        receiver
            .begin_native_publication_cut(receiver.core.authority.device_public_key, now)
            .is_err()
    );
}

#[test]
fn native_publication_streaming_cut_cannot_drop_rejected_deferred_input() {
    let (mut author, governor, member, cert, now) = crate::admission_tests::seeded();
    let receiver = author.clone();
    let publisher = receiver.core.authority.device_public_key;
    let mut doc = AutoCommit::load(&author.document("test.app", "tab:1").unwrap()).unwrap();
    let before = doc.get_heads();
    doc.put(ROOT, "governor-parent", "parent").unwrap();
    doc.commit();
    let parent = author
        .publish_incremental("test.app", "tab:1", doc.save_after(&before), &governor, now)
        .unwrap();
    let before = doc.get_heads();
    doc.put(ROOT, "revoked-member-child", "child").unwrap();
    doc.commit();
    let child = author
        .publish_incremental("test.app", "tab:1", doc.save_after(&before), &member, now)
        .unwrap();
    let cut = receiver
        .begin_native_publication_cut(publisher, now)
        .unwrap()
        .push(child, now)
        .unwrap()
        .push(
            NeighbourMsg::IdentityHello {
                cert: Box::new(receiver.core.authority.clone()),
                revocations: vec![revocation(cert.device_public_key)],
            },
            now,
        )
        .unwrap();
    assert!(
        cut.push(parent, now).is_err(),
        "a rejected native child disappeared from the supposedly complete cut"
    );
    assert!(
        !receiver
            .core
            .revocations
            .is_revoked(&cert.device_public_key)
    );
}

#[test]
fn native_publication_streaming_cut_requires_complete_room_history() {
    let (receiver, _, _, _, now) = crate::admission_tests::seeded();
    let publisher = receiver.core.authority.device_public_key;
    let original = receiver.durable().unwrap();
    let mut room = receiver.admitted_room().unwrap().clone();
    let before = room.get_heads();
    room.put(ROOT, "cut-room-parent", "parent").unwrap();
    room.commit();
    let parent = NeighbourMsg::SyncResp {
        room: receiver.core.room.clone(),
        changes: room
            .get_changes(&before)
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
    };
    let before = room.get_heads();
    room.put(ROOT, "cut-room-child", "child").unwrap();
    room.commit();
    let child = NeighbourMsg::SyncResp {
        room: receiver.core.room.clone(),
        changes: room
            .get_changes(&before)
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
    };
    let cut = receiver
        .begin_native_publication_cut(publisher, now)
        .unwrap()
        .push(child.clone(), now)
        .unwrap();
    assert!(
        cut.finish(now).is_err(),
        "native room orphans escaped without publisher provenance"
    );
    assert_eq!(receiver.durable().unwrap(), original);
    let complete = receiver
        .begin_native_publication_cut(publisher, now)
        .unwrap()
        .push(child, now)
        .unwrap()
        .push(parent, now)
        .unwrap()
        .finish(now)
        .unwrap();
    assert_eq!(
        complete.admitted_room().unwrap().base_heads(),
        room.base_heads()
    );
}
