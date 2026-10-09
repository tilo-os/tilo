use super::*;
use crate::{NeighbourMsg, SeedDocument, SeedState, StatePeer, peer_handle, seed_room};
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use collab_types::CommitNamed as _;
use collab_types::{DocumentMutation, SignedOp};
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, RevocationList, sign_device_cert};
use web_time::{Duration, SystemTime};

const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const APP: &str = "proof.app";
fn fixture() -> (SigningKey, RetentionAuthority, StatePeer, SystemTime) {
    let root = SigningKey::from_bytes(&[1; 32]);
    let key = SigningKey::from_bytes(&[2; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "proof governor".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    let now = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(2000))
        .unwrap();
    let mut peer = StatePeer::new(ROOM, cert.clone(), vec![], RevocationList::new(), now).unwrap();
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "value", "initial").unwrap();
    doc.commit_edit("fixture", "value");
    let bytes = doc.save();
    peer.install_seed(
        SeedState {
            room_changes: seed_room(ROOM, &cert, &[cert.clone()]).unwrap(),
            documents: vec![
                SeedDocument {
                    app_id: APP.into(),
                    record_id: "one".into(),
                    kind: "test".into(),
                    bytes: bytes.clone(),
                },
                SeedDocument {
                    app_id: APP.into(),
                    record_id: "two".into(),
                    kind: "test".into(),
                    bytes,
                },
            ],
        },
        &key,
        now,
    )
    .unwrap();
    (key, RetentionAuthority::new(ROOM, cert).unwrap(), peer, now)
}
fn encoded(msg: &NeighbourMsg) -> ([u8; 32], Vec<u8>) {
    (
        crate::transport::retention(msg, true).unwrap().0,
        postcard::to_allocvec(msg).unwrap(),
    )
}
fn room_proof(
    authority: &RetentionAuthority,
    msg: &NeighbourMsg,
    kind: RoomInput,
    prior: Option<&VerifiedRoom>,
) -> VerifiedRoom {
    let (channel, body) = encoded(msg);
    verify_room(
        authority,
        authority.governor().device_public_key,
        channel,
        &body,
        kind,
        prior,
    )
    .unwrap()
}
fn snapshot(room: &VerifiedRoom, msg: &NeighbourMsg) -> VerifiedSnapshot {
    let (channel, body) = encoded(msg);
    let NeighbourMsg::AppDocument { envelope, .. } = msg else {
        panic!()
    };
    verify_snapshot(room, *envelope.author.as_bytes(), channel, &body).unwrap()
}
fn signed(
    key: &SigningKey,
    mutation: &DocumentMutation,
    seq: u64,
    now: SystemTime,
) -> NeighbourMsg {
    let source = key.verifying_key().to_bytes();
    let mut envelope = SignedOp::new_unsigned(
        mutation.encode().unwrap(),
        collab_types::document_artifact_id(APP, ROOM, "one"),
        collab_types::Ed25519PublicKey::from_bytes(source),
        collab_types::document_attestation_id(APP, ROOM, "one"),
        collab_types::OpKind::Change,
        now,
        seq,
    )
    .with_claims(
        collab_types::mutation_claims(mutation, collab_types::record_unit).unwrap_or_default(),
    );
    collab_types::sign_op(&mut envelope, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: peer_handle(&source),
        app_id: APP.into(),
        cluster_id: ROOM.into(),
        record_id: "one".into(),
        envelope: Box::new(envelope),
    }
}

#[test]
fn same_heads_new_receipt_is_exact_pair_and_old_signature_is_unpaired() {
    let (key, authority, mut peer, now) = fixture();
    let first = peer.durable_bootstrap_messages(&key, now, 1).unwrap();
    let old_room = room_proof(&authority, &first[0], RoomInput::Checkpoint, None);
    let old = snapshot(&old_room, &first[1]);
    let second = peer.durable_bootstrap_messages(&key, now, 2).unwrap();
    let room = room_proof(
        &authority,
        &second[0],
        RoomInput::Checkpoint,
        Some(&old_room),
    );
    let new = snapshot(&room, &second[1]);
    assert_eq!(new.coverage().heads, old.coverage().heads);
    assert_ne!(new.receipt().receipt_id, old.receipt().receipt_id);
    assert!(new.covers_snapshot(&old));
    assert!(!old.covers_snapshot(&new));
    let (channel, body) = encoded(&first[1]);
    assert!(matches!(
        verify_snapshot(
            &room,
            authority.governor().device_public_key,
            channel,
            &body
        ),
        Err(ProofError::Unpaired)
    ));
    let (channel, body) = encoded(&first[0]);
    assert_eq!(
        room.covers_frame(authority.governor().device_public_key, channel, &body),
        CoverageDecision::Covered
    );
}

#[test]
fn authority_rejects_invalid_cert_and_noncanonical_room() {
    let (_, authority, _, _) = fixture();
    assert!(
        RetentionAuthority::new(
            "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA",
            authority.governor().clone()
        )
        .is_err()
    );
    let mut cert = authority.governor().clone();
    cert.signature[0] ^= 1;
    assert!(RetentionAuthority::new(ROOM, cert).is_err());
}

#[test]
fn mixed_receipt_generations_pair_route_by_route_and_delta_needs_prior() {
    let (key, authority, mut peer, now) = fixture();
    let first = peer.durable_bootstrap_messages(&key, now, 1).unwrap();
    let old_room = room_proof(&authority, &first[0], RoomInput::Checkpoint, None);
    let before = peer.clone();
    let mut doc = AutoCommit::load(&peer.document(APP, "one").unwrap()).unwrap();
    doc.put(ROOT, "value", "edited").unwrap();
    let msg = signed(
        &key,
        &DocumentMutation::Snapshot(doc.save()),
        9_000_000_000_000_000_000,
        now,
    );
    let NeighbourMsg::AppDocument { envelope, .. } = msg else {
        panic!()
    };
    peer.admit_local_app_document(APP, "one", *envelope, now)
        .unwrap();
    let tail = peer
        .durable_mutation_messages(&before, &key, now, 2)
        .unwrap();
    assert_eq!(tail.len(), 2);
    let (channel, body) = encoded(&tail[0]);
    assert!(
        verify_room(
            &authority,
            authority.governor().device_public_key,
            channel,
            &body,
            RoomInput::Checkpoint,
            None
        )
        .is_err()
    );
    let room = room_proof(&authority, &tail[0], RoomInput::Delta, Some(&old_room));
    let changed = snapshot(&room, &tail[1]);
    let untouched = snapshot(&room, &first[2]);
    assert_eq!(changed.receipt().generation, 2);
    assert_eq!(untouched.receipt().generation, 1);
    assert_eq!(changed.coverage().sequence, 9_000_000_000_000_000_001);
}

#[test]
fn signed_route_crypto_and_strict_body_fail_closed() {
    let (key, authority, mut peer, now) = fixture();
    let batch = peer.durable_bootstrap_messages(&key, now, 1).unwrap();
    let room = room_proof(&authority, &batch[0], RoomInput::Checkpoint, None);
    let source = authority.governor().device_public_key;
    let (channel, body) = encoded(&batch[1]);
    assert!(verify_snapshot(&room, [99; 32], channel, &body).is_err());
    assert!(verify_snapshot(&room, source, [99; 32], &body).is_err());
    let mut trailing = body;
    trailing.push(0);
    assert!(verify_snapshot(&room, source, channel, &trailing).is_err());
    for case in 0..8 {
        let mut bad = batch[1].clone();
        let NeighbourMsg::AppDocument {
            peer,
            app_id,
            cluster_id,
            record_id,
            envelope,
        } = &mut bad
        else {
            panic!()
        };
        match case {
            0 => *peer ^= 1,
            1 => *app_id = "wrong".into(),
            2 => *cluster_id = "wrong".into(),
            3 => *record_id = "wrong".into(),
            4 => envelope.seq += 1, // invalid signature
            5 => {
                envelope.payload.push(0);
                collab_types::sign_op(envelope, &key).unwrap();
            }
            6 => {
                envelope.payload = DocumentMutation::Snapshot(vec![1, 2, 3]).encode().unwrap();
                collab_types::sign_op(envelope, &key).unwrap();
            }
            _ => {
                envelope.encryption = Some(collab_types::EncryptionMetadata::mls_application(
                    1,
                    collab_types::document_channel_id(APP, ROOM, "one"),
                ));
                envelope.payload = vec![1, 2, 3];
                collab_types::sign_op(envelope, &key).unwrap();
            }
        }
        assert!(
            verify_snapshot(
                &room,
                source,
                channel,
                &postcard::to_allocvec(&bad).unwrap()
            )
            .is_err(),
            "case {case}"
        );
    }
}

#[test]
fn higher_sequence_and_generation_do_not_cover_an_independent_snapshot() {
    let (key, authority, mut peer, now) = fixture();
    let first = peer.durable_bootstrap_messages(&key, now, 1).unwrap();
    let room = room_proof(&authority, &first[0], RoomInput::Checkpoint, None);
    let prior = snapshot(&room, &first[1]);
    // Same room/key/visible value, independent Automerge history.
    let (_, _, mut fork, _) = fixture();
    fork.durable_bootstrap_messages(&key, now, 1).unwrap();
    let later = fork.durable_bootstrap_messages(&key, now, 2).unwrap();
    let room = room_proof(&authority, &later[0], RoomInput::Checkpoint, None);
    let candidate = snapshot(&room, &later[1]);
    assert!(candidate.coverage().sequence > prior.coverage().sequence);
    assert!(candidate.receipt().generation > prior.receipt().generation);
    assert!(!candidate.covers_snapshot(&prior));
}

fn room_document(msg: &NeighbourMsg) -> AutoCommit {
    let NeighbourMsg::SyncResp { changes, .. } = msg else {
        panic!()
    };
    crate::room::candidate(None, changes).unwrap()
}
fn room_message(doc: &mut AutoCommit) -> NeighbourMsg {
    NeighbourMsg::SyncResp {
        room: ROOM.into(),
        changes: doc
            .get_changes(&[])
            .iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect(),
    }
}

#[test]
fn room_checksum_corruption_is_unknown_for_checkpoint_delta_and_coverage() {
    let (key, authority, mut peer, now) = fixture();
    let baseline = peer.durable_bootstrap_messages(&key, now, 1).unwrap();
    let prior = room_proof(&authority, &baseline[0], RoomInput::Checkpoint, None);
    let mut doc = room_document(&baseline[0]);
    doc.put(ROOT, "checksum_probe", "compressible".repeat(1024))
        .unwrap();
    let mut change = doc.get_last_local_change().unwrap();
    let raw = change.raw_bytes().to_vec();
    let compressed = change.bytes().to_vec();
    let source = authority.governor().device_public_key;
    let mut outcomes = Vec::new();
    for (kind, bytes) in [(1, raw), (2, compressed)] {
        assert_eq!(bytes[8], kind, "exercise raw and compressed Change chunks");
        assert!(matches!(
            automerge::Automerge::load(&bytes),
            Err(automerge::AutomergeError::MissingDeps)
        ));
        let delta = NeighbourMsg::SyncResp {
            room: ROOM.into(),
            changes: vec![bytes.clone()],
        };
        let (channel, body) = encoded(&delta);
        let complete = room_proof(
            &authority,
            &room_message(&mut doc),
            RoomInput::Checkpoint,
            Some(&prior),
        );
        let valid_delta = verify_room(
            &authority,
            source,
            channel,
            &body,
            RoomInput::Delta,
            Some(&prior),
        )
        .unwrap();
        assert_eq!(valid_delta.heads(), complete.heads());
        assert_eq!(
            complete.covers_frame(source, channel, &body),
            CoverageDecision::Covered
        );
        for offset in 4..8 {
            let mut corrupt = bytes.clone();
            corrupt[offset] ^= 1;
            let mut checkpoint = baseline[0].clone();
            let NeighbourMsg::SyncResp { changes, .. } = &mut checkpoint else {
                panic!()
            };
            changes.push(corrupt.clone());
            let (_, checkpoint_body) = encoded(&checkpoint);
            let delta = NeighbourMsg::SyncResp {
                room: ROOM.into(),
                changes: vec![corrupt],
            };
            let (_, delta_body) = encoded(&delta);
            outcomes.push((
                kind,
                offset,
                matches!(
                    verify_room(
                        &authority,
                        source,
                        channel,
                        &checkpoint_body,
                        RoomInput::Checkpoint,
                        None
                    ),
                    Err(ProofError::Unknown)
                ),
                matches!(
                    verify_room(
                        &authority,
                        source,
                        channel,
                        &delta_body,
                        RoomInput::Delta,
                        Some(&prior)
                    ),
                    Err(ProofError::Unknown)
                ),
                complete.covers_frame(source, channel, &delta_body),
            ));
        }
    }
    assert!(
        outcomes
            .iter()
            .all(|(_, _, checkpoint, delta, covered)| *checkpoint
                && *delta
                && *covered == CoverageDecision::Unknown),
        "corrupt chunk outcomes (kind, checksum byte, checkpoint rejected, delta rejected, coverage): {outcomes:?}"
    );
}

#[test]
fn room_proof_rejects_foreign_missing_dependencies_suffix_and_regressing_metadata() {
    let (key, authority, mut peer, now) = fixture();
    let first = peer.durable_bootstrap_messages(&key, now, 1).unwrap();
    let old = room_proof(&authority, &first[0], RoomInput::Checkpoint, None);
    let second = peer.durable_bootstrap_messages(&key, now, 2).unwrap();
    let current = room_proof(&authority, &second[0], RoomInput::Checkpoint, Some(&old));
    let (channel, body) = encoded(&first[0]);
    let source = authority.governor().device_public_key;
    assert!(
        verify_room(
            &authority,
            source,
            channel,
            &body,
            RoomInput::Checkpoint,
            Some(&current)
        )
        .is_err()
    );
    assert!(
        verify_room(
            &authority,
            [99; 32],
            channel,
            &body,
            RoomInput::Checkpoint,
            None
        )
        .is_err()
    );
    assert!(
        verify_room(
            &authority,
            source,
            [99; 32],
            &body,
            RoomInput::Checkpoint,
            None
        )
        .is_err()
    );
    for case in 0..4 {
        let mut msg = first[0].clone();
        let NeighbourMsg::SyncResp { room, changes } = &mut msg else {
            panic!()
        };
        match case {
            0 => *room = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into(),
            1 => {
                changes.remove(0);
            }
            2 => changes[0].push(0),
            _ => changes.clear(),
        }
        assert!(
            verify_room(
                &authority,
                source,
                channel,
                &postcard::to_allocvec(&msg).unwrap(),
                RoomInput::Checkpoint,
                None
            )
            .is_err(),
            "case {case}"
        );
    }
    let mut doc = room_document(&first[0]);
    doc.put(ROOT, "room", "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb")
        .unwrap();
    let (_, body) = encoded(&room_message(&mut doc));
    assert!(
        verify_room(
            &authority,
            source,
            channel,
            &body,
            RoomInput::Checkpoint,
            None
        )
        .is_err()
    );
}

#[test]
fn exact_coverage_receipt_hash_heads_watermarks_required() {
    let (key, authority, mut peer, now) = fixture();
    let batch = peer.durable_bootstrap_messages(&key, now, 1).unwrap();
    let (app_channel, app_body) = encoded(&batch[1]);
    for case in 0..9 {
        let mut doc = room_document(&batch[0]);
        let mut coverage = crate::coverage::entries(&doc)
            .unwrap()
            .remove(&crate::coverage::route_key(APP, "one"))
            .unwrap();
        let mut receipts = crate::durability::entries(&doc).unwrap();
        match case {
            0 => {
                receipts.clear();
            }
            1 => {
                coverage.payload_hash[0] ^= 1;
            }
            2 => {
                coverage.heads.clear();
            }
            3 => {
                coverage.watermarks.clear();
            }
            4 => {
                coverage.room = "wrong".into();
            }
            5 => {
                coverage.author = [99; 32];
            }
            6 => {
                receipts.values_mut().next().unwrap().receipt_id = "wrong".into();
            }
            _ => {}
        }
        if (1..=5).contains(&case) {
            crate::coverage::put(&mut doc, coverage.clone()).unwrap();
            receipts.insert(
                crate::coverage::route_key(APP, "one"),
                DurableReceipt::new(coverage, 1).unwrap(),
            );
        }
        crate::durability::put(&mut doc, receipts).unwrap();
        if case >= 7 {
            // A trailing byte on one stored route value.
            let field = if case == 7 {
                crate::coverage::FIELD_V2
            } else {
                crate::durability::FIELD_V2
            };
            let route = crate::coverage::route_key(APP, "one");
            let (_, routes) = automerge::ReadDoc::get(&doc, ROOT, field).unwrap().unwrap();
            let Some((automerge::Value::Scalar(value), _)) =
                automerge::ReadDoc::get(&doc, &routes, route.as_str()).unwrap()
            else {
                panic!("stored route value")
            };
            let automerge::ScalarValue::Bytes(mut bytes) = value.into_owned() else {
                panic!("route bytes")
            };
            bytes.push(0);
            doc.put(
                &routes,
                route.as_str(),
                automerge::ScalarValue::Bytes(bytes),
            )
            .unwrap();
            doc.commit_edit("fixture", "fixture");
        }
        let (channel, body) = encoded(&room_message(&mut doc));
        if let Ok(room) = verify_room(
            &authority,
            authority.governor().device_public_key,
            channel,
            &body,
            RoomInput::Checkpoint,
            None,
        ) {
            assert!(
                verify_snapshot(
                    &room,
                    authority.governor().device_public_key,
                    app_channel,
                    &app_body
                )
                .is_err(),
                "case {case}"
            );
        }
    }
}

#[test]
fn causal_tail_coverage_is_distinct_from_floor_safety_and_foreign_retirement() {
    let (key, authority, mut peer, now) = fixture();
    let batch = peer.durable_bootstrap_messages(&key, now, 1).unwrap();
    let room = room_proof(&authority, &batch[0], RoomInput::Checkpoint, None);
    let proof = snapshot(&room, &batch[1]);
    let mut branch = AutoCommit::load(&peer.document(APP, "one").unwrap()).unwrap();
    let heads = branch.get_heads();
    branch.put(ROOT, "uncovered", true).unwrap();
    branch.commit_edit("fixture", "uncovered");
    let bytes: Vec<u8> = branch
        .get_changes(&heads)
        .iter()
        .flat_map(|c| c.raw_bytes().iter().copied())
        .collect();
    let floor = proof.coverage().sequence;
    for (seq, expected) in [
        (floor, FloorSafety::Blocked),
        (floor + 1, FloorSafety::Safe),
    ] {
        let (channel, body) = encoded(&signed(
            &key,
            &DocumentMutation::Incremental(bytes.clone()),
            seq,
            now,
        ));
        assert_eq!(
            proof.classify_tail(authority.governor().device_public_key, channel, &body),
            TailDecision {
                coverage: CoverageDecision::NotCovered,
                floor: expected
            }
        );
    }
    let existing: Vec<u8> = AutoCommit::load(&peer.document(APP, "one").unwrap())
        .unwrap()
        .get_changes(&[])
        .iter()
        .flat_map(|c| c.raw_bytes().iter().copied())
        .collect();
    let (channel, body) = encoded(&signed(
        &key,
        &DocumentMutation::Incremental(existing.clone()),
        floor,
        now,
    ));
    assert_eq!(
        proof.classify_tail(authority.governor().device_public_key, channel, &body),
        TailDecision {
            coverage: CoverageDecision::Covered,
            floor: FloorSafety::Safe
        }
    );
    let foreign = SigningKey::from_bytes(&[3; 32]);
    let (channel, body) = encoded(&signed(
        &foreign,
        &DocumentMutation::Incremental(existing),
        1,
        now,
    ));
    let result = proof.classify_tail(foreign.verifying_key().to_bytes(), channel, &body);
    assert_ne!(result.coverage, CoverageDecision::Covered);
    assert_eq!(result.floor, FloorSafety::Safe);
    let mut bad = body;
    bad.push(0);
    assert_eq!(
        proof
            .classify_tail(foreign.verifying_key().to_bytes(), channel, &bad)
            .floor,
        FloorSafety::Unknown
    );
}

#[test]
fn historical_room_proof_preserves_effect_receipts_and_exact_governor_roster() {
    let (key, authority, mut peer, now) = fixture();
    let batch = peer.durable_bootstrap_messages(&key, now, 1).unwrap();
    let source = authority.governor().device_public_key;
    let mut doc = room_document(&batch[0]);
    let receipts = doc
        .put_object(ROOT, "effect_receipts_v1", automerge::ObjType::Map)
        .unwrap();
    doc.put(
        &receipts,
        "invocation",
        automerge::ScalarValue::Bytes(b"{\"effect\":1}".to_vec()),
    )
    .unwrap();
    let prior = room_proof(
        &authority,
        &room_message(&mut doc),
        RoomInput::Checkpoint,
        None,
    );
    for case in 0..5 {
        let mut next = doc.clone();
        match case {
            0 => {
                next.delete(&receipts, "invocation").unwrap();
            }
            1 => {
                next.put(
                    &receipts,
                    "invocation",
                    automerge::ScalarValue::Bytes(b"{\"effect\":2}".to_vec()),
                )
                .unwrap();
            }
            2 => {
                next.put(
                    &receipts,
                    "invocation",
                    automerge::ScalarValue::Bytes(b"invalid json".to_vec()),
                )
                .unwrap();
            }
            3 | 4 => {
                let identities = automerge::ReadDoc::get(&next, ROOT, "peer_identities_v1")
                    .unwrap()
                    .unwrap()
                    .1;
                if case == 3 {
                    next.delete(identities, hex::encode(source)).unwrap();
                } else {
                    next.put(
                        identities,
                        hex::encode(source),
                        automerge::ScalarValue::Bytes(b"{}".to_vec()),
                    )
                    .unwrap();
                }
            }
            _ => panic!("unexpected fixture shape"),
        }
        let (channel, body) = encoded(&room_message(&mut next));
        assert!(
            verify_room(
                &authority,
                source,
                channel,
                &body,
                RoomInput::Checkpoint,
                Some(&prior)
            )
            .is_err(),
            "case {case}"
        );
    }
}

#[test]
fn incoming_snapshot_obsolescence_needs_exact_signed_route_integrity_and_selected_floor() {
    let (key, authority, mut peer, now) = fixture();
    let first = peer.durable_bootstrap_messages(&key, now, 1).unwrap();
    let second = peer.durable_bootstrap_messages(&key, now, 2).unwrap();
    let room = room_proof(&authority, &second[0], RoomInput::Checkpoint, None);
    let selected = snapshot(&room, &second[1]);
    let (channel, body) = encoded(&first[1]);
    let source = authority.governor().device_public_key;
    assert!(selected.covers_incoming_snapshot(source, channel, &body));
    let NeighbourMsg::AppDocument { envelope, .. } = &first[1] else {
        panic!()
    };
    let mutation = DocumentMutation::decode(&envelope.payload).unwrap();
    let newer = signed(&key, &mutation, selected.coverage().sequence + 1, now);
    assert!(!selected.covers_incoming_snapshot(source, channel, &encoded(&newer).1));
    assert!(!selected.covers_incoming_snapshot([99; 32], channel, &body));
    assert!(!selected.covers_incoming_snapshot(source, [99; 32], &body));
    let mut bad = body;
    *bad.last_mut().unwrap() ^= 1;
    assert!(!selected.covers_incoming_snapshot(source, channel, &bad));
    let DocumentMutation::Snapshot(mut bytes) = mutation else {
        panic!()
    };
    bytes.push(1);
    let malformed = signed(&key, &DocumentMutation::Snapshot(bytes), envelope.seq, now);
    assert!(!selected.covers_incoming_snapshot(source, channel, &encoded(&malformed).1));
}

#[test]
fn hostile_tail_is_rejected_without_in_process_decode() {
    let (key, authority, mut peer, now) = fixture();
    let batch = peer.durable_bootstrap_messages(&key, now, 1).unwrap();
    let room = room_proof(&authority, &batch[0], RoomInput::Checkpoint, None);
    let proof = snapshot(&room, &batch[1]);
    let source = authority.governor().device_public_key;
    let seq = proof.coverage().sequence + 1;
    let mut branch = AutoCommit::load(&peer.document(APP, "one").unwrap()).unwrap();
    let heads = branch.get_heads();
    branch.put(ROOT, "next", true).unwrap();
    branch.commit_edit("fixture", "next");
    let valid = branch.save_after(&heads);
    assert_eq!(valid[8], 1, "the tail is one change chunk");

    // A change header declaring a body of 2^40 bytes that never arrives.
    let mut huge = vec![0x85, 0x6f, 0x4a, 0x83, 0, 0, 0, 0, 1];
    huge.extend([0x80, 0x80, 0x80, 0x80, 0x80, 0x20]);
    huge.extend([0xff; 16]);
    // Well framed, so only an Automerge decode (in the isolate) can refuse these.
    let mut checksum = valid.clone();
    checksum[4] ^= 1;
    let mut body = valid.clone();
    *body.last_mut().unwrap() ^= 0xff;

    let unknown = TailDecision {
        coverage: CoverageDecision::Unknown,
        floor: FloorSafety::Unknown,
    };
    let (channel, accepted) = encoded(&signed(
        &key,
        &DocumentMutation::Incremental(valid),
        seq,
        now,
    ));
    for (case, hostile) in [("huge", huge), ("checksum", checksum), ("body", body)] {
        let (_, hostile) = encoded(&signed(
            &key,
            &DocumentMutation::Incremental(hostile),
            seq,
            now,
        ));
        assert_eq!(
            proof.classify_tail(source, channel, &hostile),
            unknown,
            "{case}"
        );
        assert!(
            !proof.tail_ready_after_snapshot(source, channel, &hostile),
            "{case}"
        );
        let mut cursor = proof.ordered_tail_readiness();
        assert!(!cursor.append(source, channel, &hostile), "{case}");
        // A refused increment leaves the cursor where it was.
        assert!(cursor.append(source, channel, &accepted), "{case}");
    }
}
