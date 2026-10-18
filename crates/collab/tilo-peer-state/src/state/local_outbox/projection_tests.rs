use super::*;
use crate::InputDisposition;
use crate::MissingPrerequisite;
use crate::TransportOrigin;
use automerge::AutoCommit;
use automerge::ROOT;
use tilo_user_identity::RevocationList;

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: its setup and assertions stay together"
)]
fn local_projection_full_original_queue_repeated_expiry_stays_bounded_and_converges() {
    let (mut governor, mut member, governor_key, key, cert, now) = fixture();
    let originals = (0..64)
        .map(|i| local_edit(&mut member, &key, now, i == 0, &format!("offline-{i}")))
        .collect::<Vec<_>>();
    let original_bytes = postcard::to_allocvec(&originals).unwrap();
    let reservation = member
        .outbox()
        .reserve_local_recovery(&originals, None, key.verifying_key().to_bytes())
        .unwrap();
    assert_eq!(reservation.entries(), 1);
    let mut projection = Vec::new();
    let mut first_projection = None;
    for attempt in 1..=12 {
        let later = now + Duration::from_secs(301 * attempt);
        checkpoint(&mut governor, &mut member, &governor_key, later, false);
        let old = projection.clone();
        let before_projection = member.clone();
        projection = member
            .outbox_mut()
            .reconcile_local_recovery_projection(&originals, None, &old, &key, later)
            .unwrap();
        assert_eq!(projection.len(), 1);
        assert!(
            member
                .admissions_since(&before_projection)
                .unwrap()
                .is_empty(),
            "renewal introduces no local exact admission"
        );
        assert!(
            member.durable().unwrap().len()
                <= before_projection.durable().unwrap().len() + reservation.max_peer_growth_bytes()
        );
        let before_echo = member.durable().unwrap();
        let pending = member.core.pending.len();
        for _ in 0..4 {
            let echo = member
                .receive_from_report(
                    TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                    projection[0].clone(),
                    later,
                )
                .unwrap();
            assert!(matches!(
                echo.primary(),
                InputDisposition::RetryLater(MissingPrerequisite::ExactHistoricalEvidence)
            ));
        }
        assert_eq!(member.core.pending.len(), pending);
        assert_eq!(
            member.durable().unwrap(),
            before_echo,
            "strict echoes neither defer nor append evidence"
        );
        assert!(publication_size(&projection).unwrap() <= reservation.max_encoded_bytes());
        if attempt == 1 {
            first_projection = Some(projection[0].clone());
        }
        if !old.is_empty() {
            assert_ne!(projection, old);
        }
        assert_eq!(postcard::to_allocvec(&originals).unwrap(), original_bytes);
        member
            .outbox()
            .validate_local_recovery_projection(
                &originals,
                None,
                &projection,
                key.verifying_key().to_bytes(),
            )
            .unwrap();
        let room = member.room().to_owned();
        let authority = member.authority().clone();
        member = StatePeer::restore(
            &member.durable().unwrap(),
            &room,
            authority,
            vec![cert.clone()],
            RevocationList::new(),
            later,
        )
        .unwrap();
        member
            .outbox()
            .validate_local_recovery_projection(
                &originals,
                None,
                &projection,
                key.verifying_key().to_bytes(),
            )
            .unwrap();
    }
    let later = now + Duration::from_secs(301 * 12);
    assert!(
        governor
            .receive_from_report(
                TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                first_projection.unwrap(),
                later
            )
            .is_err()
    );
    governor
        .receive_from_report(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            projection[0].clone(),
            later,
        )
        .unwrap();
    checkpoint(&mut governor, &mut member, &governor_key, later, true);
    assert!(originals.iter().all(|message| {
        member
            .outbox()
            .retained_local_document_retired(message, key.verifying_key().to_bytes())
            .unwrap()
    }));
    assert!(
        member
            .outbox_mut()
            .reconcile_local_recovery_projection(&originals, None, &projection, &key, later)
            .unwrap()
            .is_empty()
    );
    assert_eq!(member.rows(), governor.rows());
    assert_eq!(postcard::to_allocvec(&originals).unwrap(), original_bytes);
}

#[test]
fn local_projection_legacy_history_does_not_require_invented_original_signatures() {
    let (mut governor, mut member, governor_key, key, _, now) = fixture();
    let _missing = local_edit(&mut member, &key, now, true, "legacy-projection");
    let history = member.clone();
    let original_history = history.durable().unwrap();
    let reservation = member
        .outbox()
        .reserve_local_recovery(&[], Some(&history), key.verifying_key().to_bytes())
        .unwrap();
    let later = now + Duration::from_secs(301);
    checkpoint(&mut governor, &mut member, &governor_key, later, false);
    let projection = member
        .outbox_mut()
        .reconcile_local_recovery_projection(&[], Some(&history), &[], &key, later)
        .unwrap();
    assert_eq!(projection.len(), reservation.entries());
    assert!(publication_size(&projection).unwrap() <= reservation.max_encoded_bytes());
    governor
        .receive_from_report(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            projection[0].clone(),
            later,
        )
        .unwrap();
    checkpoint(&mut governor, &mut member, &governor_key, later, true);
    assert!(
        member
            .outbox()
            .retained_local_history_retired(&history)
            .unwrap()
    );
    assert_eq!(history.durable().unwrap(), original_history);
}

#[test]
fn local_projection_reserved_floor_survives_restore_and_next_original_cannot_reuse_it() {
    let (mut governor, mut member, governor_key, key, cert, now) = fixture();
    let first = local_edit(&mut member, &key, now, false, "original-before");
    let later = now + Duration::from_secs(301);
    checkpoint(&mut governor, &mut member, &governor_key, later, false);
    let projection = member
        .outbox_mut()
        .reconcile_local_recovery_projection(std::slice::from_ref(&first), None, &[], &key, later)
        .unwrap();
    let NeighbourMsg::AppDocument {
        envelope: reserved, ..
    } = &projection[0]
    else {
        panic!()
    };
    let room = member.room().to_owned();
    let authority = member.authority().clone();
    member = StatePeer::restore(
        &member.durable().unwrap(),
        &room,
        authority,
        vec![cert],
        RevocationList::new(),
        later,
    )
    .unwrap();
    let second = local_edit(&mut member, &key, later, false, "original-after");
    let NeighbourMsg::AppDocument { envelope: next, .. } = &second else {
        panic!()
    };
    let next_sequence = next.seq;
    assert!(next_sequence > reserved.seq);
    assert!(
        member
            .outbox()
            .lookup_retained_local_document(&first, key.verifying_key().to_bytes())
            .unwrap()
            .is_some()
    );
    assert!(
        member
            .outbox()
            .lookup_retained_local_document(&second, key.verifying_key().to_bytes())
            .unwrap()
            .is_some()
    );
    let before = member.durable().unwrap();
    assert!(
        member
            .outbox_mut()
            .reconcile_local_recovery_projection(
                &[first.clone(), second.clone()],
                None,
                &projection,
                &key,
                later
            )
            .is_err(),
        "restore does not reconstruct fresh governor basis"
    );
    assert_eq!(member.durable().unwrap(), before);
    checkpoint(&mut governor, &mut member, &governor_key, later, false);
    let before = member.clone();
    let refreshed = member
        .outbox_mut()
        .reconcile_local_recovery_projection(&[first, second], None, &projection, &key, later)
        .unwrap();
    assert!(member.admissions_since(&before).unwrap().is_empty());
    let NeighbourMsg::AppDocument {
        envelope: refreshed,
        ..
    } = &refreshed[0]
    else {
        panic!()
    };
    assert!(refreshed.seq > next_sequence);
}

#[test]
fn local_projection_lost_reply_then_durable_governor_ack_retires_before_resigning() {
    let (mut governor, mut member, governor_key, key, _, now) = fixture();
    let original = local_edit(&mut member, &key, now, true, "lost-reply");
    let later = now + Duration::from_secs(301);
    checkpoint(&mut governor, &mut member, &governor_key, later, false);
    let projection = member
        .outbox_mut()
        .reconcile_local_recovery_projection(
            std::slice::from_ref(&original),
            None,
            &[],
            &key,
            later,
        )
        .unwrap();
    governor
        .receive_from_report(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            projection[0].clone(),
            later,
        )
        .unwrap();
    let reconnected = later + Duration::from_secs(301);
    checkpoint(&mut governor, &mut member, &governor_key, reconnected, true);
    let before = member.durable().unwrap();
    assert!(
        member
            .outbox()
            .retained_local_document_retired(&original, key.verifying_key().to_bytes())
            .unwrap()
    );
    assert!(
        member
            .outbox_mut()
            .reconcile_local_recovery_projection(&[original], None, &projection, &key, reconnected)
            .unwrap()
            .is_empty()
    );
    assert_eq!(member.durable().unwrap(), before);
}

#[test]
fn local_projection_historical_validation_rejects_unrelated_real_signed_history_and_tampering() {
    let (mut governor, mut member, governor_key, key, _, now) = fixture();
    let original = local_edit(&mut member, &key, now, false, "obligation");
    let later = now + Duration::from_secs(301);
    checkpoint(&mut governor, &mut member, &governor_key, later, false);
    let first = member
        .outbox_mut()
        .reconcile_local_recovery_projection(
            std::slice::from_ref(&original),
            None,
            &[],
            &key,
            later,
        )
        .unwrap();
    let unrelated = local_edit(&mut member, &key, later, false, "unrelated");
    let unrelated_projection = member
        .outbox_mut()
        .reconcile_local_recovery_projection(&[unrelated], None, &[], &key, later)
        .unwrap();
    assert!(
        member
            .outbox()
            .validate_local_recovery_projection(
                std::slice::from_ref(&original),
                None,
                &unrelated_projection,
                key.verifying_key().to_bytes()
            )
            .is_err()
    );
    assert!(
        member
            .outbox()
            .validate_local_recovery_projection(
                std::slice::from_ref(&original),
                None,
                std::slice::from_ref(&original),
                key.verifying_key().to_bytes()
            )
            .is_err(),
        "an original receipt cannot be reclassified as a dispensable projection"
    );
    for damage in 0..4 {
        let mut bad = first.clone();
        let NeighbourMsg::AppDocument {
            envelope,
            record_id,
            ..
        } = &mut bad[0]
        else {
            panic!()
        };
        match damage {
            0 => envelope.signature.0[0] ^= 1,
            1 => {
                envelope.seq = u64::MAX;
                collab_types::sign_op(envelope, &key).unwrap();
            }
            2 => *record_id = "wrong-record".into(),
            _ => {
                envelope.kind = OpKind::Redaction;
                collab_types::sign_op(envelope, &key).unwrap();
            }
        }
        assert!(
            member
                .outbox()
                .validate_local_recovery_projection(
                    std::slice::from_ref(&original),
                    None,
                    &bad,
                    key.verifying_key().to_bytes()
                )
                .is_err()
        );
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: its setup and assertions stay together"
)]
fn local_projection_current_document_restriction_blocks_sign_and_send_but_preserves_custody() {
    let (mut governor, mut member, governor_key, key, _, now) = fixture();
    let original = local_edit(&mut member, &key, now, false, "policy-bound");
    let later = now + Duration::from_secs(301);
    checkpoint(&mut governor, &mut member, &governor_key, later, false);
    let projection = member
        .outbox_mut()
        .reconcile_local_recovery_projection(
            std::slice::from_ref(&original),
            None,
            &[],
            &key,
            later,
        )
        .unwrap();
    member
        .outbox()
        .authorize_local_recovery_projection(
            std::slice::from_ref(&original),
            None,
            &projection,
            key.verifying_key().to_bytes(),
            later,
        )
        .unwrap();
    let NeighbourMsg::SyncResp { changes, room } = governor.room_message() else {
        panic!()
    };
    let mut room_doc = crate::room::candidate(None, &changes).unwrap();
    let before = room_doc.get_heads();
    room_doc.set_actor(automerge::ActorId::from(
        governor_key.verifying_key().to_bytes().to_vec(),
    ));
    let caps = room_doc.get(ROOT, "grants").unwrap().unwrap().1;
    let person = member
        .permissions(key.verifying_key().to_bytes(), later)
        .unwrap()
        .person;
    // The structural owner cannot be restricted: hand the room to someone
    // else and keep this person as an editor, then restrict them.
    room_doc.put(ROOT, "owner", "another-person").unwrap();
    let members = room_doc.get(ROOT, "members").unwrap().unwrap().1;
    room_doc
        .put(
            &members,
            person.as_str(),
            automerge::ScalarValue::Bytes(serde_json::to_vec(&collab_types::Role::Editor).unwrap()),
        )
        .unwrap();
    let restriction = tilo_action_policy::Grant::new(
        "projection-write-denial",
        tilo_action_policy::Subject::Person(person.clone()),
        tilo_action_policy::TreePath::room(member.room()).app("test.app"),
        tilo_action_policy::KindSet::UNDOABLE.union(tilo_action_policy::KindSet::OUTSIDE),
        tilo_action_policy::Answer::Deny,
        tilo_action_policy::Subject::Role(collab_types::Role::Owner),
    );
    room_doc
        .put(
            &caps,
            "projection-write-denial",
            automerge::ScalarValue::Bytes(serde_json::to_vec(&restriction).unwrap()),
        )
        .unwrap();
    let changes = room_doc
        .get_changes(&before)
        .into_iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect();
    governor
        .receive_from_report(
            TransportOrigin::RelayDelta(governor_key.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp { changes, room },
            later,
        )
        .unwrap();
    member
        .receive_from_report(
            TransportOrigin::RelayDelta(governor_key.verifying_key().to_bytes()),
            governor.room_message(),
            later,
        )
        .unwrap();
    let before = member.durable().unwrap();
    assert!(
        member
            .permissions(key.verifying_key().to_bytes(), later)
            .is_ok(),
        "membership alone is insufficient"
    );
    member
        .outbox()
        .validate_local_recovery_projection(
            std::slice::from_ref(&original),
            None,
            &projection,
            key.verifying_key().to_bytes(),
        )
        .unwrap();
    member
        .outbox()
        .reserve_local_recovery(
            std::slice::from_ref(&original),
            None,
            key.verifying_key().to_bytes(),
        )
        .unwrap();
    assert!(
        member
            .outbox()
            .authorize_local_recovery_projection(
                std::slice::from_ref(&original),
                None,
                &projection,
                key.verifying_key().to_bytes(),
                later
            )
            .is_err()
    );
    assert!(
        member
            .outbox_mut()
            .reconcile_local_recovery_projection(&[original], None, &projection, &key, later)
            .is_err()
    );
    assert_eq!(member.durable().unwrap(), before);
}

#[test]
fn local_projection_revocation_preserves_historical_proof_and_blocks_send_and_renewal() {
    let (mut governor, mut member, governor_key, key, cert, now) = fixture();
    let original = local_edit(&mut member, &key, now, false, "revoked-projection");
    let later = now + Duration::from_secs(301);
    checkpoint(&mut governor, &mut member, &governor_key, later, false);
    let projection = member
        .outbox_mut()
        .reconcile_local_recovery_projection(
            std::slice::from_ref(&original),
            None,
            &[],
            &key,
            later,
        )
        .unwrap();
    let mut revocation = tilo_user_identity::RevocationEntry {
        user_public_key: cert.user_public_key,
        revoked_device_public_key: cert.device_public_key,
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revocation, &SigningKey::from_bytes(&[1; 32]))
        .unwrap();
    member
        .receive_from_report(
            TransportOrigin::UntrustedPresence(governor_key.verifying_key().to_bytes()),
            NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(governor.authority().clone()),
                revocations: vec![revocation],
            },
            later,
        )
        .unwrap();
    let before = member.durable().unwrap();
    member
        .outbox()
        .validate_local_recovery_projection(
            std::slice::from_ref(&original),
            None,
            &projection,
            key.verifying_key().to_bytes(),
        )
        .unwrap();
    assert!(
        member
            .outbox()
            .authorize_local_recovery_projection(
                std::slice::from_ref(&original),
                None,
                &projection,
                key.verifying_key().to_bytes(),
                later
            )
            .is_err()
    );
    assert!(
        member
            .outbox_mut()
            .reconcile_local_recovery_projection(&[original], None, &projection, &key, later)
            .is_err()
    );
    assert_eq!(member.durable().unwrap(), before);
}

#[test]
fn local_projection_reservation_bounds_actual_binary_and_decimal_floor_serialization() {
    for with_original in [false, true] {
        let (_, mut member, _, key, _, now) = fixture();
        let originals = if with_original {
            vec![local_edit(&mut member, &key, now, false, "cost")]
        } else {
            Vec::new()
        };
        let history = member.clone();
        let reservation = member
            .outbox()
            .reserve_local_recovery(&originals, Some(&history), key.verifying_key().to_bytes())
            .unwrap();
        let before = member.durable().unwrap();
        // Cost-only ceiling shape: never signed, admitted, or passed as authority.
        let mut ceiling = member.clone();
        for (app, record, _) in reservation.routes() {
            ceiling
                .core
                .documents
                .get_mut(&Route {
                    app: app.into(),
                    record: record.into(),
                })
                .unwrap()
                .channel
                .last_seq_per_author
                .insert(
                    Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
                    u64::MAX,
                );
        }
        let after = ceiling.durable().unwrap();
        assert_eq!(
            after.len() - before.len(),
            reservation.max_peer_growth_bytes()
        );
        let digits = |bytes: &[u8]| {
            bytes
                .iter()
                .map(|b| {
                    if *b >= 100 {
                        3
                    } else if *b >= 10 {
                        2
                    } else {
                        1
                    }
                })
                .sum::<usize>()
        };
        assert_eq!(
            digits(&after) - digits(&before),
            reservation.max_peer_decimal_growth()
        );
    }
}

#[test]
fn local_projection_partial_ack_rebuilds_within_reduced_remaining_reservation() {
    let (mut governor, mut member, governor_key, key, _, now) = fixture();
    let mut branch = AutoCommit::load(&member.document("test.app", "tab:1").unwrap()).unwrap();
    let base = branch.get_heads();
    branch.set_actor(automerge::ActorId::from(vec![71; 32]));
    branch.put(ROOT, "small-independent", true).unwrap();
    branch.commit_edit("fixture", "small-independent");
    // Large even compressed: the projection compresses its history.
    let large_text = (0..1200_u64).fold(String::new(), |mut text, n| {
        use std::fmt::Write as _;
        let _ = write!(text, "{:x}", n.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        text
    });
    let large = local_edit(&mut member, &key, now, true, &large_text);
    let small = member
        .publish_incremental("test.app", "tab:1", branch.save_after(&base), &key, now)
        .unwrap();
    // Governor has only the large branch; receipt is deliberately withheld.
    let large_history = member
        .publication_history(
            &member
                .retained_document(&large, key.verifying_key().to_bytes())
                .unwrap(),
        )
        .unwrap();
    // A genuine same-change Incremental is independently signed/admitted locally
    // before sending; the member Snapshot itself stays immutable custody.
    let actual = member
        .publish_incremental("test.app", "tab:1", large_history.save(), &key, now)
        .unwrap();
    governor
        .receive_from_report(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            actual,
            now,
        )
        .unwrap();
    let later = now + Duration::from_secs(301);
    checkpoint(&mut governor, &mut member, &governor_key, later, false);
    let projection = member
        .outbox_mut()
        .reconcile_local_recovery_projection(
            &[large.clone(), small.clone()],
            None,
            &[],
            &key,
            later,
        )
        .unwrap();
    checkpoint(&mut governor, &mut member, &governor_key, later, true);
    assert!(
        member
            .outbox()
            .retained_local_document_retired(&large, key.verifying_key().to_bytes())
            .unwrap()
    );
    assert!(
        !member
            .outbox()
            .retained_local_document_retired(&small, key.verifying_key().to_bytes())
            .unwrap()
    );
    let reserve = member
        .outbox()
        .reserve_local_recovery(
            std::slice::from_ref(&small),
            None,
            key.verifying_key().to_bytes(),
        )
        .unwrap();
    let replacement = member
        .outbox_mut()
        .reconcile_local_recovery_projection(&[small], None, &projection, &key, later)
        .unwrap();
    assert!(publication_size(&replacement).unwrap() <= reserve.max_encoded_bytes());
    assert_ne!(replacement, projection);
}

#[test]
fn local_projection_late_previous_and_replacement_converge_in_either_order() {
    for old_first in [false, true] {
        let (mut governor, mut member, governor_key, key, _, now) = fixture();
        let first = local_edit(&mut member, &key, now, false, "old-effect");
        let later = now + Duration::from_secs(301);
        checkpoint(&mut governor, &mut member, &governor_key, later, false);
        let old = member
            .outbox_mut()
            .reconcile_local_recovery_projection(
                std::slice::from_ref(&first),
                None,
                &[],
                &key,
                later,
            )
            .unwrap();
        let second = local_edit(&mut member, &key, later, false, "new-effect");
        let current = member
            .outbox_mut()
            .reconcile_local_recovery_projection(
                &[first.clone(), second.clone()],
                None,
                &old,
                &key,
                later,
            )
            .unwrap();
        let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
        if old_first {
            assert!(matches!(
                governor
                    .receive_from_report(origin, old[0].clone(), later)
                    .unwrap()
                    .primary(),
                InputDisposition::Accepted(_)
            ));
        }
        assert!(matches!(
            governor
                .receive_from_report(origin, current[0].clone(), later)
                .unwrap()
                .primary(),
            InputDisposition::Accepted(_)
        ));
        let late = governor
            .receive_from_report(origin, old[0].clone(), later)
            .unwrap();
        assert!(if old_first {
            matches!(late.primary(), InputDisposition::Duplicate(_))
        } else {
            matches!(
                late.primary(),
                InputDisposition::RetryLater(MissingPrerequisite::ExactHistoricalEvidence)
            )
        });
        checkpoint(&mut governor, &mut member, &governor_key, later, true);
        for original in [first, second] {
            assert!(
                member
                    .outbox()
                    .retained_local_document_retired(&original, key.verifying_key().to_bytes())
                    .unwrap()
            );
        }
        assert_eq!(member.rows(), governor.rows());
    }
}

#[test]
fn local_projection_legacy_history_cannot_regress_a_genuine_reserved_floor() {
    let (mut governor, mut member, governor_key, key, _, now) = fixture();
    let original = local_edit(&mut member, &key, now, false, "floor-obligation");
    let later = now + Duration::from_secs(301);
    checkpoint(&mut governor, &mut member, &governor_key, later, false);
    let mut later_history = member.clone();
    later_history
        .outbox_mut()
        .reconcile_local_recovery_projection(&[original], None, &[], &key, later)
        .unwrap();
    assert!(later_history.admissions_since(&member).unwrap().is_empty());
    assert!(
        member
            .outbox()
            .reserve_local_recovery(&[], Some(&later_history), key.verifying_key().to_bytes())
            .is_err(),
        "real history with a later reserved floor cannot be recovered into a regressed peer"
    );
}
