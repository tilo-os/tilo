use super::*;
use crate::{AdmissionError, InputDisposition, MissingPrerequisite};

fn incremental(
    author: &mut StatePeer,
    key: &SigningKey,
    now: SystemTime,
    name: &str,
) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&author.document("test.app", "tab:1").unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(ROOT, name, name).unwrap();
    doc.commit_edit("fixture", "fixture", &person(author, key));
    author
        .publication_mut()
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), key, now)
        .unwrap()
}
use crate::fixture::person;
fn restore(peer: &StatePeer, now: SystemTime) -> StatePeer {
    StatePeer::restore(
        &peer.durable().unwrap(),
        peer.workspace_id(),
        peer.owner_certificates()[0].clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap()
}

tilo_test_env::scenario! {
    fn delivery_admission_admits_a_write_once_and_its_exact_retry;

    phase delivery_admission_selection_is_explicit_and_exact_retry_survives_restore;
    {
        let (mut receiver, _, key, _, now) = seeded();
        let mut author = receiver.clone();
        let message = incremental(&mut author, &key, now, "A");
        let origin = crate::fixture::origin(&key);
        let before = receiver.durable().unwrap();
        assert!(matches!(
            receiver.receive_from_report(origin, message.clone(), now),
            Err(AdmissionError::WriterSelectionRequired)
        ));
        assert_eq!(receiver.durable().unwrap(), before);
        receiver.select_exact_admissions().unwrap();
        let report = receiver
            .receive_from_report(origin, message.clone(), now)
            .unwrap();
        let InputDisposition::Accepted(first) = report.primary() else {
            panic!("not accepted: {:?}", report.primary())
        };
        let first = first.clone();
        assert_eq!(report.settled(), std::slice::from_ref(&first));
        let rows = receiver.rows();
        let later = now + Duration::from_secs(301);
        let mut receiver = restore(&receiver, later);
        assert!(receiver.exact_admissions_selected());
        assert_eq!(receiver.rows(), rows);
        let duplicate = receiver
            .receive_from_report(origin, message, later)
            .unwrap();
        assert_eq!(duplicate.primary(), &InputDisposition::Duplicate(first));
        assert!(duplicate.settled().is_empty());
        assert_eq!(receiver.rows(), rows);
    }

    phase delivery_admission_same_sequence_changed_body_conflicts_unknown_floor_does_not_duplicate;
    {
        let (mut receiver, _, key, _, now) = seeded();
        let mut author = receiver.clone();
        let message = incremental(&mut author, &key, now, "A");
        let origin = crate::fixture::origin(&key);
        let mut legacy = receiver.clone();
        legacy.receive_from(origin, message.clone(), now).unwrap();
        legacy = restore(&legacy, now);
        legacy.select_exact_admissions().unwrap();
        let unknown = legacy
            .receive_from_report(origin, message.clone(), now)
            .unwrap();
        assert_eq!(
            unknown.primary(),
            &InputDisposition::RetryLater(MissingPrerequisite::ExactHistoricalEvidence)
        );
        receiver.select_exact_admissions().unwrap();
        receiver
            .receive_from_report(origin, message.clone(), now)
            .unwrap();
        let before = receiver.durable().unwrap();
        let mut changed = message.clone();
        if let NeighbourMsg::AppDocument { envelope, .. } = &mut changed {
            envelope.timestamp += Duration::from_secs(1);
            collab_types::sign_op(envelope, &key).unwrap();
        }
        let mut floor_only = receiver.clone();
        // Two bodies signed under one number are the key on two machines: refused by name.
        assert!(matches!(
            receiver.receive_from_report(origin, changed.clone(), now),
            Err(AdmissionError::CopiedDevice(copied))
                if copied.device == key.verifying_key().to_bytes()
        ));
        assert_eq!(receiver.durable().unwrap(), before);
        // Compatibility: legacy receive keeps its old floor/duplicate semantics.
        assert!(
            floor_only
                .receive_from(origin, changed, now)
                .unwrap()
                .duplicate
        );
        let mut different_payload = incremental(&mut author, &key, now, "B");
        if let (
            NeighbourMsg::AppDocument {
                envelope: original, ..
            },
            NeighbourMsg::AppDocument {
                envelope: changed, ..
            },
        ) = (&message, &mut different_payload)
        {
            changed.seq = original.seq;
            collab_types::sign_op(changed, &key).unwrap();
        }
        assert!(matches!(
            receiver.receive_from_report(origin, different_payload.clone(), now),
            Err(AdmissionError::CopiedDevice(_))
        ));
        assert_eq!(
            legacy
                .receive_from_report(origin, different_payload, now)
                .unwrap()
                .primary(),
            &InputDisposition::RetryLater(MissingPrerequisite::ExactHistoricalEvidence)
        );
    }

    phase delivery_admission_new_same_heads_envelope_is_accepted;
    {
        let (mut receiver, _, key, _, now) = seeded();
        let mut author = receiver.clone();
        let first = incremental(&mut author, &key, now, "A");
        let mut second = first.clone();
        if let NeighbourMsg::AppDocument { envelope, .. } = &mut second {
            envelope.seq += 1;
            collab_types::sign_op(envelope, &key).unwrap();
        }
        let origin = crate::fixture::origin(&key);
        receiver.select_exact_admissions().unwrap();
        let first = receiver.receive_from_report(origin, first, now).unwrap();
        let rows = receiver.rows();
        let second = receiver.receive_from_report(origin, second, now).unwrap();
        assert!(matches!(second.primary(), InputDisposition::Accepted(_)));
        assert_ne!(first.primary(), second.primary());
        assert_eq!(rows, receiver.rows());
        assert!(!second.summary().changed);
        assert!(second.summary().metadata_changed);
    }

    phase delivery_admission_metadata_and_unsupported_families_have_no_operation_receipt;
    {
        let (mut receiver, key, _, _, now) = seeded();
        receiver.select_exact_admissions().unwrap();
        let origin = crate::fixture::origin(&key);
        let message = receiver.workspace_message();
        let report = receiver.receive_from_report(origin, message, now).unwrap();
        assert_eq!(report.primary(), &InputDisposition::MetadataOnly);
        let request = NeighbourMsg::SyncReq {
            since: vec![],
            workspace: receiver.workspace_id().into(),
            invite_id: None,
            nonce: 0,
        };
        let before = receiver.durable().unwrap();
        assert_eq!(
            receiver
                .receive_from_report(origin, request, now)
                .unwrap()
                .primary(),
            &InputDisposition::UnsupportedFamily
        );
        assert_eq!(receiver.durable().unwrap(), before);
    }

    phase delivery_admission_exact_retry_rechecks_signature_route_and_current_revocation;
    {
        let (mut receiver, owner_copy, key, _, now) = seeded();
        let mut author = receiver.clone();
        let message = incremental(&mut author, &key, now, "A");
        let origin = crate::fixture::origin(&key);
        receiver.select_exact_admissions().unwrap();
        receiver
            .receive_from_report(origin, message.clone(), now)
            .unwrap();
        let mut forged = message.clone();
        if let NeighbourMsg::AppDocument { envelope, .. } = &mut forged {
            envelope.signature = collab_types::Signature::from_bytes([0; 64]);
        }
        assert!(
            receiver
                .lookup_exact_document(origin, &forged, now)
                .is_err()
        );
        let mut wrong_route = message.clone();
        if let NeighbourMsg::AppDocument { document_id, .. } = &mut wrong_route {
            *document_id = "tab:2".into();
        }
        assert!(
            receiver
                .lookup_exact_document(origin, &wrong_route, now)
                .is_err()
        );
        assert!(
            receiver
                .lookup_exact_document(
                    TransportOrigin::UntrustedPresence(key.verifying_key().to_bytes()),
                    &message,
                    now
                )
                .is_err()
        );
        let root = SigningKey::from_bytes(&[1; 32]);
        let mut revocation = tilo_user_identity::RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: key.verifying_key().to_bytes(),
            revoked_at_ns: 1,
            reason: tilo_user_identity::RevocationReason::Lost,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut revocation, &root).unwrap();
        receiver
            .governance_mut()
            .publish_member_revocations(&[revocation], &owner_copy, now)
            .unwrap();
        let later = now + Duration::from_secs(301);
        // The write was taken before its device was removed, so the cut of
        // the removal keeps it: it is the workspace's history, and sent again
        // it is the duplicate it always was, never a new admission.
        assert!(matches!(
            receiver
                .receive_from_report(origin, message, later)
                .unwrap()
                .primary(),
            InputDisposition::Duplicate(_)
        ));
        // What the device signs after its removal is refused.
        let after = incremental(&mut author, &key, later, "B");
        assert!(
            receiver
                .receive_from_report(origin, after, later)
                .unwrap_err()
                .to_string()
                .contains("revoked")
        );
    }

    phase delivery_admission_local_candidate_delta_includes_all_routes_and_refuses_evidence_loss;
    {
        let (mut base, key, _, _, now) = seeded();
        base.select_exact_admissions().unwrap();
        let mut candidate = base.clone();
        let bytes = candidate.document("test.app", "tab:1").unwrap();
        candidate
            .publication_mut()
            .publish_document("test.app", "tab:1", bytes.clone(), &key, now)
            .unwrap();
        candidate
            .publication_mut()
            .publish_document("test.app", "tab:2", bytes, &key, now)
            .unwrap();
        let delta = candidate.admissions_since(&base).unwrap();
        assert_eq!(delta.len(), 2);
        assert_eq!(
            delta
                .iter()
                .map(super::super::admission::DocumentAdmission::record)
                .collect::<Vec<_>>(),
            ["tab:1", "tab:2"]
        );
        assert!(base.admissions_since(&candidate).is_err());
        let restored = restore(&candidate, now);
        assert_eq!(restored.admissions_since(&base).unwrap(), delta);
    }

    phase delivery_admission_operation_id_names_the_literal_signed_bytes;
    {
        let (mut receiver, _, key, _, now) = seeded();
        let mut author = receiver.clone();
        let message = incremental(&mut author, &key, now, "literal-A");
        let bytes = collab_types::canonical_bytes(&message).unwrap();
        let decoded: NeighbourMsg = collab_types::canonical::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(collab_types::canonical_bytes(&decoded).unwrap(), bytes);
        let NeighbourMsg::AppDocument { envelope, .. } = &decoded else {
            panic!("unexpected message shape")
        };
        let op = collab_types::canonical_bytes(envelope.as_ref()).unwrap();
        collab_types::verify_op(envelope.as_ref()).unwrap();
        receiver.select_exact_admissions().unwrap();
        let origin = crate::fixture::origin(&key);
        let report = receiver.receive_from_report(origin, decoded, now).unwrap();
        let InputDisposition::Accepted(admitted) = report.primary() else {
            panic!("message rejected: {:?}", report.primary())
        };
        assert_eq!(
            hex::encode(admitted.operation_id()),
            blake3::hash(&op).to_hex().as_str(),
            "the operation id names no literal canonical SignedOp bytes"
        );
    }
}

tilo_test_env::scenario! {
    fn delivery_admission_keeps_pending_retained_and_sealed_history_in_their_place;

    phase delivery_admission_primary_is_separate_from_pending_drain;
    {
        let (mut receiver, _, key, _, now) = seeded();
        let mut author = receiver.clone();
        let first = incremental(&mut author, &key, now, "A");
        let second = incremental(&mut author, &key, now, "B");
        let origin = crate::fixture::origin(&key);
        receiver.select_exact_admissions().unwrap();
        let deferred = receiver
            .receive_from_report(origin, second.clone(), now)
            .unwrap();
        assert_eq!(
            deferred.primary(),
            &InputDisposition::RetryLater(MissingPrerequisite::CausalDependencies)
        );
        assert!(deferred.settled().is_empty());
        receiver = restore(&receiver, now);
        let absent_key = SigningKey::from_bytes(&[9; 32]);
        let mut unrelated = second.clone();
        if let NeighbourMsg::AppDocument { peer, envelope, .. } = &mut unrelated {
            *peer = peer_handle(&absent_key.verifying_key().to_bytes());
            envelope.author =
                collab_types::Ed25519PublicKey::from_bytes(absent_key.verifying_key().to_bytes());
            collab_types::sign_op(envelope, &absent_key).unwrap();
        }
        let waiting = receiver
            .receive_from_report(
                crate::fixture::origin(&absent_key),
                unrelated,
                now,
            )
            .unwrap();
        assert_eq!(
            waiting.primary(),
            &InputDisposition::RetryLater(MissingPrerequisite::Certificate)
        );
        let admitted = receiver.receive_from_report(origin, first, now).unwrap();
        assert!(matches!(admitted.primary(), InputDisposition::Accepted(_)));
        assert_eq!(admitted.settled().len(), 2);
        assert!(
            admitted.summary().deferred,
            "unrelated pending item remains separate from accepted primary"
        );
        assert_eq!(receiver.pending_count(), 1);
        assert!(matches!(
            receiver
                .receive_from_report(origin, second, now)
                .unwrap()
                .primary(),
            InputDisposition::Duplicate(_)
        ));
    }

    phase delivery_admission_review_unindexed_retained_input_is_history_live_input_must_be_fresh;
    {
        let (receiver, _, key, _, now) = seeded();
        let mut author = receiver.clone();
        let message = incremental(&mut author, &key, now, "old-unindexed");
        let source = key.verifying_key().to_bytes();
        let later = now + Duration::from_secs(301);
        let retained = TransportOrigin::RetainedServerDelta(source);
        // A retained frame is its author's history: admitted as of its own
        // timestamp, through either entry point.
        let mut legacy = receiver.clone();
        assert!(
            legacy
                .receive_from(retained, message.clone(), later)
                .unwrap()
                .changed
        );
        let mut strict = receiver.clone();
        strict.select_exact_admissions().unwrap();
        let rows = strict.rows();
        let report = strict
            .receive_from_report(retained, message.clone(), later)
            .unwrap();
        assert!(matches!(report.primary(), InputDisposition::Accepted(_)));
        assert_ne!(strict.rows(), rows);
        assert!(
            strict
                .lookup_exact_document(retained, &message, later)
                .unwrap()
                .is_some()
        );
        // The same frame arriving live is too old to be a live write.
        let mut live = receiver;
        live.select_exact_admissions().unwrap();
        let rows = live.rows();
        assert!(
            live.receive_from_report(TransportOrigin::ServerDelta(source), message, later)
                .is_err(),
            "a live frame beyond the clock skew was admitted"
        );
        assert_eq!(live.rows(), rows);
    }

    phase delivery_admission_review_strict_drain_keeps_retained_pending_history_held;
    {
        let (mut receiver, authority, key, _, now) = seeded();
        let mut author = receiver.clone();
        let _first = incremental(&mut author, &key, now, "A");
        let second = incremental(&mut author, &key, now, "B");
        let origin = TransportOrigin::RetainedServerDelta(key.verifying_key().to_bytes());
        // Queue through the legacy retained path to exercise persisted provenance.
        assert!(
            receiver
                .receive_from(origin, second.clone(), now)
                .unwrap()
                .deferred
        );
        receiver = restore(&receiver, now);
        receiver.select_exact_admissions().unwrap();
        let later = now + Duration::from_secs(301);
        let workspace = receiver.workspace_message();
        let report = receiver
            .receive_from_report(
                crate::fixture::origin(&authority),
                workspace,
                later,
            )
            .unwrap();
        assert_eq!(
            report.summary().rejected,
            0,
            "retained pending history is not expired by its age"
        );
        assert_eq!(
            receiver.pending_count(),
            1,
            "it still waits for the write it builds on"
        );
        assert!(report.settled().is_empty());
        assert!(
            receiver
                .lookup_exact_document(origin, &second, later)
                .unwrap()
                .is_none()
        );
    }

    phase a_sealed_record_takes_its_cleartext_base_and_no_cleartext_after_the_seal;
    {
        let (receiver, _, key, _, now) = seeded();
        let mut author = receiver.clone();
        let source = key.verifying_key().to_bytes();
        let mut here = receiver;
        here.select_exact_admissions().unwrap();
        // The record's cleartext writes before it sealed are its base, however
        // they arrive: replayed from what the host kept, or relayed as written.
        for (name, origin) in [
            ("kept", TransportOrigin::RetainedServerDelta(source)),
            ("relayed", TransportOrigin::ServerDelta(source)),
        ] {
            let base = incremental(&mut author, &key, now, name);
            here.receive_from_report(origin, base, now).unwrap();
        }
        assert!(
            here.seal_document("test.app", "tab:1"),
            "the record is held here and sealed"
        );
        // After the seal a cleartext write is a downgrade, whether the host
        // relays it as written or sends it as what its store kept.
        for (name, origin) in [
            ("relayed-after", TransportOrigin::ServerDelta(source)),
            ("kept-after", TransportOrigin::RetainedServerDelta(source)),
        ] {
            let after = incremental(&mut author, &key, now, name);
            assert!(
                here.receive_from_report(origin, after, now).is_err(),
                "a cleartext write after the seal is refused: {name}"
            );
        }
        let doc = AutoCommit::load(&here.document("test.app", "tab:1").unwrap()).unwrap();
        for name in ["kept", "relayed"] {
            assert!(
                doc.get(ROOT, name).unwrap().is_some(),
                "the base holds {name}"
            );
        }
        for name in ["relayed-after", "kept-after"] {
            assert!(doc.get(ROOT, name).unwrap().is_none(), "{name} was applied");
        }
    }
}
