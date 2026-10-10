#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
use super::tests::tree_of;
use super::*;
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use ed25519_dalek::SigningKey;
use std::time::SystemTime;
use tilo_automerge::ApplyIncoming as _;
use tilo_joined_room::testing as internals;
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::{NeighbourMsg, StatePeer, TransportOrigin};

fn edit(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    key: &SigningKey,
    now: SystemTime,
    value: u64,
) -> (OwnerCommitRequest, Vec<u8>) {
    let message = owner
        .with_current(room, |view| {
            let mut doc =
                AutoCommit::load(&view.peer().document("so.tilo.chat", "").unwrap()).unwrap();
            doc.set_actor(automerge::ActorId::from(
                key.verifying_key().to_bytes().to_vec(),
            ));
            let heads = doc.get_heads();
            doc.put(ROOT, "offline-edit", value).unwrap();
            doc.commit();
            view.peer().clone().publish_incremental(
                "so.tilo.chat",
                "",
                doc.save_after(&heads),
                key,
                now,
            )
        })
        .unwrap();
    let bytes = postcard::to_allocvec(&message).unwrap();
    let NeighbourMsg::AppDocument {
        app_id,
        record_id,
        envelope,
        ..
    } = message
    else {
        unreachable!()
    };
    (
        OwnerCommitRequest {
            room: room.to_string(),
            operation: CommitOperation::Document {
                app: app_id,
                record: record_id,
                envelope,
            },
        },
        bytes,
    )
}

#[test]
fn native_outgoing_offline_commit_preserves_exact_bytes_and_retry_across_restart() {
    let (directory, owner, room, now, key) = super::tests::test_enrolled_owner();
    let (request, expected) = edit(&owner, room, &key, now, 1);
    owner.commit_document(request.clone(), now).unwrap();
    let bytes = owner
        .with_outgoing(room, now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| entry.bytes().to_vec())
                .collect::<Vec<_>>())
        })
        .unwrap();
    assert_eq!(
        bytes,
        vec![expected.clone()],
        "local success must include exact outgoing custody"
    );
    let path = directory.path().join("state.json");
    let committed = std::fs::read(&path).unwrap();
    owner.commit_document(request, now).unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        committed,
        "exact retry cannot append or rewrite"
    );
    drop(owner);
    let mut tree = persist::load_tree_from(&path).unwrap();
    tree.persist_path = Some(path);
    let owner = open(crate::shell::state::WorkspaceTreeHandle::new(tree)).unwrap();
    let bytes = owner
        .with_outgoing(room, now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| entry.bytes().to_vec())
                .collect::<Vec<_>>())
        })
        .unwrap();
    assert_eq!(bytes, vec![expected]);
    assert_eq!(owner.outgoing_status(room, now).unwrap().pending, 1);
}

#[test]
fn native_outgoing_failed_barrier_cannot_return_success_or_publish_a_frame() {
    let (directory, owner, room, now, key) = super::tests::test_enrolled_owner();
    let path = directory.path().join("state.json");
    let before = std::fs::read(&path).unwrap();
    let (request, _) = edit(&owner, room, &key, now, 2);
    let result = tilo_files::native::atomic::test_support::with_failure(
        tilo_files::native::atomic::test_support::Checkpoint::BeforePublish,
        || owner.commit_document(request, now),
    );
    assert!(result.is_err());
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert_eq!(owner.outgoing_status(room, now).unwrap().pending, 0);
}

fn governor(owner: &NativeWorkspaceOwner, room: RoomId) -> StatePeer {
    owner
        .with_current(room, |view| Ok(view.peer().clone()))
        .unwrap()
}
fn checkpoint(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    governor: &mut StatePeer,
    now: SystemTime,
    generation: u64,
) {
    let key = SigningKey::from_bytes(&[61; 32]);
    for message in governor
        .durable_bootstrap_messages(&key, now, generation)
        .unwrap()
    {
        owner
            .receive(
                room,
                TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                message,
                now,
            )
            .unwrap();
    }
}
fn snapshot_request(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    key: &SigningKey,
    now: SystemTime,
) -> (OwnerCommitRequest, Vec<u8>) {
    let (mut request, _) = edit(owner, room, key, now, 79);
    let CommitOperation::Document {
        app,
        record,
        envelope,
    } = &mut request.operation
    else {
        unreachable!()
    };
    let mut doc = owner
        .with_current(room, |view| {
            Ok(AutoCommit::load(&view.peer().document(app, record).unwrap()).unwrap())
        })
        .unwrap();
    let collab_types::DocumentMutation::Incremental(changes) =
        collab_types::DocumentMutation::decode(&envelope.payload).unwrap()
    else {
        unreachable!()
    };
    doc.load_incoming(&changes).unwrap();
    envelope.payload = collab_types::DocumentMutation::Snapshot(doc.save())
        .encode()
        .unwrap();
    collab_types::sign_op(envelope, key).unwrap();
    let original = NeighbourMsg::AppDocument {
        peer: tilo_peer_state::peer_handle(envelope.author.as_bytes()),
        app_id: app.clone(),
        cluster_id: room.to_string(),
        record_id: record.clone(),
        envelope: envelope.clone(),
    };
    (request, postcard::to_allocvec(&original).unwrap())
}

#[allow(
    clippy::too_many_lines,
    reason = "one custody scenario across restarts; each phase asserts on the durable state the \
              previous phase left behind"
)]
#[test]
fn native_outgoing_expired_incremental_and_snapshot_keep_original_until_real_durable_ack() {
    for snapshot in [false, true] {
        let (directory, owner, room, now, key) = super::tests::test_enrolled_owner();
        let mut remote = governor(&owner, room);
        let (request, original) = if snapshot {
            snapshot_request(&owner, room, &key, now)
        } else {
            edit(&owner, room, &key, now, 79)
        };
        owner.commit_document(request, now).unwrap();
        let later = now + std::time::Duration::from_secs(301);
        assert!(
            owner
                .with_outgoing(room, later, |_, outgoing| Ok(outgoing
                    .entries()
                    .next()
                    .is_none()))
                .unwrap()
        );
        checkpoint(&owner, room, &mut remote, later, 101);
        owner.reconcile_outgoing(room, &key, later).unwrap();
        owner
            .with_current(room, |view| {
                assert!(
                    internals::entries(view)?.contains(&original),
                    "renewal cannot rewrite or discard the original signed bytes"
                );
                Ok(())
            })
            .unwrap();
        let before_send = std::fs::read(directory.path().join("state.json")).unwrap();
        let outgoing = owner
            .with_outgoing(room, later, |_, outgoing| {
                Ok(outgoing
                    .entries()
                    .map(|entry| entry.bytes().to_vec())
                    .collect::<Vec<_>>())
            })
            .unwrap();
        assert_eq!(
            outgoing.len(),
            1,
            "only the independent fresh incremental is wire-admissible"
        );
        for _ in 0..2 {
            for bytes in &outgoing {
                let message: NeighbourMsg = postcard::from_bytes(bytes).unwrap();
                remote
                    .receive_from_report(
                        TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                        message,
                        later,
                    )
                    .unwrap();
            }
        }
        assert_eq!(
            std::fs::read(directory.path().join("state.json")).unwrap(),
            before_send,
            "send/duplicate is not an ACK"
        );
        assert_eq!(owner.outgoing_status(room, later).unwrap().pending, 2);
        let expected = remote.document_heads("so.tilo.chat", "");
        let governor_key = SigningKey::from_bytes(&[61; 32]);
        let acknowledgement = remote
            .durable_bootstrap_messages(&governor_key, later, 102)
            .unwrap();
        assert_eq!(
            acknowledgement.len(),
            2,
            "fixture is one actual Chat checkpoint pair"
        );
        let origin = TransportOrigin::RelayDelta(governor_key.verifying_key().to_bytes());
        owner
            .receive(room, origin, acknowledgement[0].clone(), later)
            .unwrap();
        let before_ack = std::fs::read(directory.path().join("state.json")).unwrap();
        assert!(
            tilo_files::native::atomic::test_support::with_failure(
                tilo_files::native::atomic::test_support::Checkpoint::BeforePublish,
                || owner.receive(room, origin, acknowledgement[1].clone(), later)
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(directory.path().join("state.json")).unwrap(),
            before_ack
        );
        assert_eq!(
            owner.outgoing_status(room, later).unwrap().pending,
            2,
            "retirement must cross the same barrier as its actual governor ACK"
        );
        owner
            .receive(room, origin, acknowledgement[1].clone(), later)
            .unwrap();
        assert_eq!(owner.outgoing_status(room, later).unwrap().pending, 0);
        owner
            .with_current(room, |view| {
                assert_eq!(view.peer().document_heads("so.tilo.chat", ""), expected);
                let original: NeighbourMsg = postcard::from_bytes(&original).unwrap();
                assert!(
                    view.peer()
                        .lookup_retained_local_document(&original, key.verifying_key().to_bytes())
                        .unwrap()
                        .is_some(),
                    "payload retirement cannot erase exact V5 evidence"
                );
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn native_outgoing_capacity_refuses_new_success_and_inbound_ack_can_release_space() {
    let (directory, owner, room, now, key) = super::tests::test_enrolled_owner();
    let mut remote = governor(&owner, room);
    for value in 0..internals::MAX_OUTGOING_ENTRIES {
        let (request, _) = edit(&owner, room, &key, now, value as u64);
        owner.commit_document(request, now).unwrap();
    }
    let before = std::fs::read(directory.path().join("state.json")).unwrap();
    let (request, _) = edit(&owner, room, &key, now, 90);
    assert!(owner.commit_document(request.clone(), now).is_err());
    assert_eq!(
        std::fs::read(directory.path().join("state.json")).unwrap(),
        before
    );
    let governor_key = SigningKey::from_bytes(&[61; 32]);
    let mut revoked = tilo_user_identity::RevocationEntry {
        user_public_key: remote.authority().user_public_key,
        revoked_device_public_key: [98; 32],
        revoked_at_ns: 10,
        reason: tilo_user_identity::RevocationReason::Retired,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revoked, &SigningKey::from_bytes(&[62; 32]))
        .unwrap();
    remote
        .publish_governed_revocations(&[revoked.clone()], &governor_key, now)
        .unwrap();
    owner
        .receive(
            room,
            TransportOrigin::RelayDelta(governor_key.verifying_key().to_bytes()),
            remote.room_message(),
            now,
        )
        .unwrap();
    owner
        .with_current(room, |view| {
            assert!(view.peer().revocations().contains(&revoked));
            Ok(())
        })
        .unwrap();
    assert_eq!(
        owner.outgoing_status(room, now).unwrap().pending,
        internals::MAX_OUTGOING_ENTRIES,
        "full outgoing custody must not suppress actual incoming revocation"
    );
    let frames = owner
        .with_outgoing(room, now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| entry.bytes().to_vec())
                .collect::<Vec<_>>())
        })
        .unwrap();
    for bytes in frames {
        remote
            .receive_from_report(
                TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                postcard::from_bytes(&bytes).unwrap(),
                now,
            )
            .unwrap();
    }
    checkpoint(&owner, room, &mut remote, now, 200);
    assert_eq!(owner.outgoing_status(room, now).unwrap().pending, 0);
    owner.commit_document(request, now).unwrap();
    assert_eq!(owner.outgoing_status(room, now).unwrap().pending, 1);
}

#[test]
fn native_outgoing_uncertain_append_fences_and_reopens_exact_committed_bytes() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    for point in [Checkpoint::AfterPublish, Checkpoint::BeforeCommit] {
        let (directory, owner, room, now, key) = super::tests::test_enrolled_owner();
        let (request, expected) = edit(&owner, room, &key, now, 51);
        assert!(with_failure(point, || owner.commit_document(request, now)).is_err());
        assert!(owner.with_outgoing(room, now, |_, _| Ok(())).is_err());
        assert!(owner.health().is_err());
        drop(owner);
        let path = directory.path().join("state.json");
        let mut tree = persist::load_tree_from(&path).unwrap();
        tree.persist_path = Some(path);
        let owner = open(crate::shell::state::WorkspaceTreeHandle::new(tree)).unwrap();
        assert_eq!(
            owner
                .with_outgoing(room, now, |_, outgoing| Ok(outgoing
                    .entries()
                    .map(|entry| entry.bytes().to_vec())
                    .collect::<Vec<_>>()))
                .unwrap(),
            vec![expected]
        );
    }
}

/// A member write carrying only history the room already holds, as an app
/// does when it sends its whole record after a frontier reset.
fn resend_known_history(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    key: &SigningKey,
    now: SystemTime,
) -> OwnerCommitRequest {
    let message = owner
        .with_current(room, |view| {
            let mut doc =
                AutoCommit::load(&view.peer().document("so.tilo.chat", "").unwrap()).unwrap();
            view.peer().clone().publish_incremental(
                "so.tilo.chat",
                "",
                doc.save_after(&[]),
                key,
                now,
            )
        })
        .unwrap();
    let NeighbourMsg::AppDocument {
        app_id,
        record_id,
        envelope,
        ..
    } = message
    else {
        unreachable!()
    };
    OwnerCommitRequest {
        room: room.to_string(),
        operation: CommitOperation::Document {
            app: app_id,
            record: record_id,
            envelope,
        },
    }
}

#[test]
fn native_outgoing_acknowledged_history_reuses_one_projection_until_retired() {
    let (_directory, owner, room, now, key) = super::tests::test_enrolled_owner();
    let mut remote = governor(&owner, room);
    owner
        .commit_document(resend_known_history(&owner, room, &key, now), now)
        .unwrap();
    // The governor's durable receipt already holds this history, but not the
    // write's own sequence, so the original still awaits its watermark.
    checkpoint(&owner, room, &mut remote, now, 600);
    owner.reconcile_outgoing(room, &key, now).unwrap();
    let sent = visible(&owner, room, now);
    assert_eq!(sent.len(), 2, "the original and one recovery projection");
    for _ in 0..3 {
        owner.reconcile_outgoing(room, &key, now).unwrap();
        assert_eq!(
            visible(&owner, room, now),
            sent,
            "reconciliation must not sign another retransmission of the same obligation"
        );
    }
    for bytes in &sent {
        remote
            .receive_from_report(
                TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                postcard::from_bytes(bytes).unwrap(),
                now,
            )
            .unwrap();
    }
    checkpoint(&owner, room, &mut remote, now, 601);
    owner.reconcile_outgoing(room, &key, now).unwrap();
    assert!(visible(&owner, room, now).is_empty());
    assert_eq!(owner.outgoing_status(room, now).unwrap().pending, 0);
}

#[test]
fn native_outgoing_full_offline_queue_must_recover_after_expiry() {
    let (_directory, owner, room, now, key) = super::tests::test_enrolled_owner();
    let mut remote = governor(&owner, room);
    let mut accepted = 0;
    // A capacity refusal is honest; every edit for which we did return success
    // must still have a recovery path when the connection returns after expiry.
    for value in 0..internals::MAX_OUTGOING_ENTRIES {
        let (request, _) = edit(&owner, room, &key, now, value as u64 + 100);
        if owner.commit_document(request, now).is_err() {
            break;
        }
        accepted += 1;
    }
    assert!(accepted > 0);
    let expected = owner
        .with_current(room, |view| {
            Ok(view.peer().document_heads("so.tilo.chat", ""))
        })
        .unwrap();
    let later = now + std::time::Duration::from_secs(301);
    checkpoint(&owner, room, &mut remote, later, 300);
    owner
        .reconcile_outgoing(room, &key, later)
        .expect("accepted offline edits must retain bounded capacity for authorized recovery");
    for turn in 0..internals::MAX_OUTGOING_ENTRIES {
        let frames = owner
            .with_outgoing(room, later, |_, outgoing| {
                Ok(outgoing
                    .entries()
                    .map(|entry| entry.bytes().to_vec())
                    .collect::<Vec<_>>())
            })
            .unwrap();
        for bytes in frames {
            remote
                .receive_from_report(
                    TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                    postcard::from_bytes(&bytes).unwrap(),
                    later,
                )
                .unwrap();
        }
        checkpoint(&owner, room, &mut remote, later, 301 + turn as u64);
        if owner.outgoing_status(room, later).unwrap().pending == 0 {
            break;
        }
        owner.reconcile_outgoing(room, &key, later).unwrap();
    }
    assert_eq!(owner.outgoing_status(room, later).unwrap().pending, 0);
    assert_eq!(remote.document_heads("so.tilo.chat", ""), expected);
}

fn visible(owner: &NativeWorkspaceOwner, room: RoomId, now: SystemTime) -> Vec<Vec<u8>> {
    owner
        .with_outgoing(room, now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| entry.bytes().to_vec())
                .collect())
        })
        .unwrap()
}

#[test]
fn native_outgoing_replaces_repeated_expiry_without_new_originals_or_exact_admissions() {
    let (directory, mut owner, room, now, key) = super::tests::test_enrolled_owner();
    let mut remote = governor(&owner, room);
    let base = governor(&owner, room);
    let (request, original) = edit(&owner, room, &key, now, 321);
    owner.commit_document(request, now).unwrap();
    let path = directory.path().join("state.json");
    let mut previous = None;
    for turn in 1..=5 {
        let later = now + std::time::Duration::from_secs(301 * turn);
        checkpoint(&owner, room, &mut remote, later, 400 + turn);
        owner.reconcile_outgoing(room, &key, later).unwrap();
        let frames = visible(&owner, room, later);
        assert_eq!(frames.len(), 1);
        if let Some(previous) = previous {
            assert_ne!(frames[0], previous);
        }
        previous = Some(frames[0].clone());
        owner
            .with_current(room, |view| {
                assert_eq!(internals::entries(view)?.len(), 1);
                assert_eq!(internals::entries(view)?[0], original);
                assert_eq!(internals::projection(view)?.len(), 1);
                assert_eq!(
                    view.peer()
                        .admissions_since(&base)
                        .map_err(|error| error.to_string())?
                        .iter()
                        .filter(|admission| admission.author() == key.verifying_key().to_bytes())
                        .count(),
                    1,
                    "pure retransmission must not allocate new local exact receipts"
                );
                let projected: NeighbourMsg = postcard::from_bytes(&frames[0]).unwrap();
                assert!(
                    view.peer()
                        .lookup_retained_local_document(&projected, key.verifying_key().to_bytes())
                        .unwrap()
                        .is_none()
                );
                Ok(())
            })
            .unwrap();
        let retained_peer = owner
            .with_current(room, |view| Ok(view.peer().clone()))
            .unwrap();
        drop(owner);
        let mut tree = persist::load_tree_from(&path).unwrap();
        tree.persist_path = Some(path.clone());
        owner = open(crate::shell::state::WorkspaceTreeHandle::new(tree)).unwrap();
        owner
            .with_current(room, |view| {
                // Durable watermarks originate in a HashMap. Restoring may
                // change their serialized order without changing any floor.
                assert_eq!(
                    view.peer().replay_floors(),
                    retained_peer.replay_floors(),
                    "restart must preserve every exact reserved sequence floor"
                );
                assert!(
                    view.peer()
                        .admissions_since(&retained_peer)
                        .map_err(|error| error.to_string())?
                        .is_empty(),
                    "restart must neither lose nor add exact admission evidence"
                );
                assert!(
                    retained_peer
                        .admissions_since(view.peer())
                        .map_err(|error| error.to_string())?
                        .is_empty(),
                    "restart must preserve exact admission evidence in both directions"
                );
                assert_eq!(internals::entries(view)?[0], original);
                assert_eq!(internals::projection(view)?, frames);
                Ok(())
            })
            .unwrap();
        assert!(
            visible(&owner, room, later).is_empty(),
            "restore cannot invent live governor checkpoint freshness"
        );
        assert_eq!(owner.outgoing_status(room, later).unwrap().blocked, 2);
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one custody scenario across restarts; each phase asserts on the durable state the \
              previous phase left behind"
)]
#[test]
fn native_outgoing_projection_replacement_barrier_and_late_echo_preserve_custody() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    let (directory, owner, room, now, key) = super::tests::test_enrolled_owner();
    let mut remote = governor(&owner, room);
    let (request, original) = edit(&owner, room, &key, now, 400);
    owner.commit_document(request, now).unwrap();
    let first = now + std::time::Duration::from_secs(301);
    checkpoint(&owner, room, &mut remote, first, 500);
    owner.reconcile_outgoing(room, &key, first).unwrap();
    let old = visible(&owner, room, first)[0].clone();
    let later = first + std::time::Duration::from_secs(301);
    checkpoint(&owner, room, &mut remote, later, 501);
    let path = directory.path().join("state.json");
    let before = std::fs::read(&path).unwrap();
    let peer_before = owner
        .with_current(room, |view| view.peer().durable())
        .unwrap();
    assert!(
        with_failure(Checkpoint::BeforePublish, || owner
            .reconcile_outgoing(room, &key, later))
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        owner
            .with_current(room, |view| view.peer().durable())
            .unwrap(),
        peer_before
    );
    owner
        .with_current(room, |view| {
            assert_eq!(internals::projection(view)?[0], old);
            assert_eq!(internals::entries(view)?[0], original);
            Ok(())
        })
        .unwrap();
    owner.reconcile_outgoing(room, &key, later).unwrap();
    let fresh = visible(&owner, room, later)[0].clone();
    assert_ne!(old, fresh);
    let before_echo = owner
        .with_current(room, |view| view.peer().durable())
        .unwrap();
    for bytes in [&old, &fresh] {
        let result = owner.receive(
            room,
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            postcard::from_bytes(bytes).unwrap(),
            later,
        );
        if bytes == &old {
            // Freshness precedes replay lookup: this superseded projection
            // is 301 seconds old and must remain rejected after renewal.
            assert_eq!(
                result.unwrap_err(),
                "op timestamp is outside attestation validity window"
            );
        } else {
            assert!(matches!(
                result.unwrap().primary(),
                tilo_peer_state::InputDisposition::RetryLater(_)
            ));
        }
        assert_eq!(
            owner
                .with_current(room, |view| view.peer().durable())
                .unwrap(),
            before_echo,
            "strict late echo must not grow peer, pending data or exact evidence"
        );
    }
    for reverse in [false, true] {
        let mut receiver = remote.clone();
        let packets = if reverse {
            [&fresh, &old]
        } else {
            [&old, &fresh]
        };
        for bytes in packets {
            let before = receiver.durable().unwrap();
            let result = receiver.receive_from_report(
                TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                postcard::from_bytes(bytes).unwrap(),
                later,
            );
            if bytes == &old {
                assert_eq!(
                    result.unwrap_err(),
                    tilo_peer_state::AdmissionError::from(
                        "op timestamp is outside attestation validity window"
                    )
                );
                assert_eq!(receiver.durable().unwrap(), before);
            } else {
                result.unwrap();
            }
        }
        assert_eq!(
            receiver.document_heads("so.tilo.chat", ""),
            owner
                .with_current(room, |view| Ok(view
                    .peer()
                    .document_heads("so.tilo.chat", "")))
                .unwrap()
        );
    }
    // A subsequent user edit uses the actual committed floor, not an old SDK basis.
    let (request, next_original) = edit(&owner, room, &key, later, 401);
    owner.commit_document(request, later).unwrap();
    owner
        .with_current(room, |view| {
            assert_eq!(internals::entries(view)?, vec![original, next_original]);
            Ok(())
        })
        .unwrap();
}

#[test]
fn native_outgoing_uncertain_projection_reopens_published_floor_and_reserve_tamper_refuses() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    for point in [Checkpoint::AfterPublish, Checkpoint::BeforeCommit] {
        let (directory, owner, room, now, key) = super::tests::test_enrolled_owner();
        let mut remote = governor(&owner, room);
        let (request, original) = edit(&owner, room, &key, now, 451);
        owner.commit_document(request, now).unwrap();
        let later = now + std::time::Duration::from_secs(301);
        checkpoint(&owner, room, &mut remote, later, 600);
        assert!(with_failure(point, || owner.reconcile_outgoing(room, &key, later)).is_err());
        assert!(owner.health().is_err());
        assert!(owner.with_outgoing(room, later, |_, _| Ok(())).is_err());
        drop(owner);
        let path = directory.path().join("state.json");
        let mut tree = persist::load_tree_from(&path).unwrap();
        let published = internals::stored(&tree.native_authority, room).unwrap();
        let durable_peer = published.peer.clone();
        let retained_peer = StatePeer::restore(
            &durable_peer,
            &room.to_string(),
            published.governor.clone(),
            vec![published.device.clone()],
            tilo_user_identity::RevocationList::new(),
            later,
        )
        .unwrap();
        let projection = published.projection[0].clone();
        tree.persist_path = Some(path.clone());
        let owner = open(crate::shell::state::WorkspaceTreeHandle::new(tree)).unwrap();
        assert!(
            visible(&owner, room, later).is_empty(),
            "uncertain publication preserves custody, not transient governor freshness"
        );
        assert_eq!(owner.outgoing_status(room, later).unwrap().blocked, 2);
        owner
            .with_current(room, |view| {
                // The stored authority bytes stay exact; reconstructed
                // HashMap watermark ordering is not a durable invariant.
                assert_eq!(internals::durable_peer(view), durable_peer);
                assert_eq!(view.peer().replay_floors(), retained_peer.replay_floors());
                assert!(
                    view.peer()
                        .admissions_since(&retained_peer)
                        .map_err(|error| error.to_string())?
                        .is_empty()
                );
                assert!(
                    retained_peer
                        .admissions_since(view.peer())
                        .map_err(|error| error.to_string())?
                        .is_empty()
                );
                assert_eq!(internals::entries(view)?[0], original);
                assert_eq!(internals::projection(view)?.len(), 1);
                assert_eq!(internals::projection(view)?[0], projection);
                Ok(())
            })
            .unwrap();
        drop(owner);
        let mut encoded: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let reservation = &mut encoded["native_authority"]["rooms"][room.to_string()]["publication"]
            ["reservation"];
        reservation["state_bytes"] = serde_json::json!(0);
        std::fs::write(&path, serde_json::to_vec_pretty(&encoded).unwrap()).unwrap();
        let mut tree = persist::load_tree_from(&path).unwrap();
        tree.persist_path = Some(path);
        assert!(
            open(crate::shell::state::WorkspaceTreeHandle::new(tree)).is_err(),
            "restore must derive capacity proof from actual obligations"
        );
    }
}

#[test]
fn native_outgoing_projection_history_substitution_cannot_restore() {
    let (directory, owner, room, now, key) = super::tests::test_enrolled_owner();
    let mut remote = governor(&owner, room);
    let (request, _) = edit(&owner, room, &key, now, 470);
    owner.commit_document(request, now).unwrap();
    let later = now + std::time::Duration::from_secs(301);
    checkpoint(&owner, room, &mut remote, later, 700);
    owner.reconcile_outgoing(room, &key, later).unwrap();
    let old = visible(&owner, room, later)[0].clone();
    let NeighbourMsg::AppDocument {
        envelope: old_envelope,
        ..
    } = postcard::from_bytes::<NeighbourMsg>(&old).unwrap()
    else {
        unreachable!()
    };
    let (mut request, _) = edit(&owner, room, &key, later, 999);
    let CommitOperation::Document { envelope, .. } = &mut request.operation else {
        unreachable!()
    };
    envelope.seq = old_envelope.seq;
    collab_types::sign_op(envelope, &key).unwrap();
    let bytes = internals::prepare(&request).unwrap();
    let message = postcard::from_bytes(&bytes).unwrap();
    let (timestamp, channel, incremental) = internals::metadata(&message).unwrap();
    drop(owner);
    let path = directory.path().join("state.json");
    let mut encoded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let entry =
        &mut encoded["native_authority"]["rooms"][room.to_string()]["publication"]["projection"][0];
    entry["id"] = serde_json::to_value(*blake3::hash(&bytes).as_bytes()).unwrap();
    entry["bytes"] = serde_json::to_value(bytes).unwrap();
    entry["binding"] = serde_json::to_value(request.binding().unwrap()).unwrap();
    entry["timestamp_ns"] = serde_json::json!(timestamp);
    entry["channel"] = serde_json::to_value(channel).unwrap();
    entry["incremental"] = serde_json::json!(incremental);
    std::fs::write(&path, serde_json::to_vec_pretty(&encoded).unwrap()).unwrap();
    let mut tree = persist::load_tree_from(&path).unwrap();
    tree.persist_path = Some(path);
    assert!(
        open(crate::shell::state::WorkspaceTreeHandle::new(tree)).is_err(),
        "valid local signature and reserved sequence cannot substitute uncommitted history"
    );
}

#[test]
fn native_outgoing_admission_reserves_actual_encoded_replacement_before_receipt() {
    let (_directory, owner, room, now, key) = super::tests::test_enrolled_owner();
    let mut remote = governor(&owner, room);
    let (request, _) = snapshot_request(&owner, room, &key, now);
    owner.commit_document(request, now).unwrap();
    let reserved = publication::required_tree_bytes(&tree_of(&owner).read()).unwrap();
    let later = now + std::time::Duration::from_secs(301);
    checkpoint(&owner, room, &mut remote, later, 800);
    // Governor input is a separate real state change; measure its committed reserve.
    let reserved_after_checkpoint =
        publication::required_tree_bytes(&tree_of(&owner).read()).unwrap();
    assert!(reserved > 0 && reserved_after_checkpoint <= store::MAX_STATE_BYTES);
    owner.reconcile_outgoing(room, &key, later).unwrap();
    owner
        .with_current(room, internals::check_actual_projection_bound)
        .unwrap();
    let actual = serde_json::to_vec_pretty(&*tree_of(&owner).read())
        .unwrap()
        .len();
    assert!(
        actual <= reserved_after_checkpoint,
        "replacement and its floor must fit the storage reserved before signing"
    );
}

#[test]
fn native_outgoing_encoded_recovery_reserve_refuses_before_original_success() {
    let (directory, owner, room, now, key) = super::tests::test_enrolled_owner();
    // Incompressible text makes the actual native history large while staying
    // inside the original wire cap. The candidate itself fits; its mandatory
    // future recovery representation does not fit the authority envelope.
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut random = 0x1234_5678_u32;
    let text: String = (0..1024 * 1024)
        .map(|_| {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            alphabet[(random & 63) as usize] as char
        })
        .collect();
    let message = owner
        .with_current(room, |view| {
            let mut doc =
                AutoCommit::load(&view.peer().document("so.tilo.chat", "").unwrap()).unwrap();
            let heads = doc.get_heads();
            doc.put(ROOT, "large-offline", text).unwrap();
            doc.commit();
            view.peer().clone().publish_incremental(
                "so.tilo.chat",
                "",
                doc.save_after(&heads),
                &key,
                now,
            )
        })
        .unwrap();
    let request = internals::request(&message).unwrap();
    let outgoing = internals::prepare(&request).unwrap();
    assert!(outgoing.len() < internals::MAX_OUTGOING_BYTES);
    let mut candidate = owner.with_current(room, |view| Ok(view.clone())).unwrap();
    let CommitOperation::Document {
        app,
        record,
        envelope,
    } = request.operation.clone()
    else {
        unreachable!()
    };
    internals::peer_mut(&mut candidate)
        .admit_local_app_document(&app, &record, *envelope, now)
        .unwrap();
    internals::append_local(&mut candidate, outgoing).unwrap();
    internals::seal(&mut candidate).unwrap();
    let mut prospective = tree_of(&owner).read().clone();
    internals::insert_room(&mut prospective.native_authority, room, candidate);
    store::preflight(&prospective.native_authority, store::MAX_AUTHORITY_BYTES).unwrap();
    assert!(
        tilo_joined_room::preflight_storage(&prospective.native_authority).is_err(),
        "the refusal must come from the required recovery reserve"
    );
    let path = directory.path().join("state.json");
    let before = std::fs::read(&path).unwrap();
    assert!(owner.commit_document(request, now).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(owner.outgoing_status(room, now).unwrap().pending, 0);
}

#[test]
fn native_outgoing_sanitized_foreign_title_cannot_consume_recovery_reserve() {
    use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};
    use std::{cell::Cell, rc::Rc};
    let (directory, owner, room, now, key) = super::tests::test_enrolled_owner();
    let (request, _) = edit(&owner, room, &key, now, 911);
    let mut candidate = owner.with_current(room, |view| Ok(view.clone())).unwrap();
    let CommitOperation::Document {
        app,
        record,
        envelope,
    } = request.operation.clone()
    else {
        unreachable!()
    };
    internals::peer_mut(&mut candidate)
        .admit_local_app_document(&app, &record, *envelope, now)
        .unwrap();
    internals::append_local(&mut candidate, internals::prepare(&request).unwrap()).unwrap();
    internals::seal(&mut candidate).unwrap();
    internals::bump_generation(&mut candidate);
    let group_index = {
        let handle = tree_of(&owner);
        let mut tree = handle.write();
        let groups = &mut tree.desktop.workspaces[0].strips[0].window_groups;
        let index = groups.len();
        groups.push(WindowGroup {
            id: 990_000,
            tabs: vec![Tab {
                id: 990_001,
                title: String::new(),
                tile: TileContent::Foreign {
                    app_id: "org.example.Editor".into(),
                    surface_id: 0xfeed_beef,
                },
            }],
            active_tab: 990_001,
            width: WindowWidth::Normal,
            maximized: false,
        });
        index
    };
    let mut prospective = tree_of(&owner).read().clone();
    internals::insert_room(&mut prospective.native_authority, room, candidate);
    internals::bump_revision(&mut prospective.native_authority);
    // Use the actual sanitizer/serializer on the small tree to derive this
    // fixture. Adding ASCII title text changes only title and durable label.
    let small = persist::encode_tree(&prospective).unwrap();
    let sanitized: crate::shell::state::WorkspaceTree = serde_json::from_slice(&small).unwrap();
    let reserved = publication::required_tree_bytes(&sanitized).unwrap() - small.len();
    assert!(reserved > 2);
    let title_len = (store::MAX_STATE_BYTES - small.len() - reserved / 2) / 2;
    let final_encoded_len = small.len() + 2 * title_len;
    assert!(final_encoded_len < store::MAX_STATE_BYTES);
    assert!(final_encoded_len + reserved > store::MAX_STATE_BYTES);
    let title = "t".repeat(title_len);
    prospective.desktop.workspaces[0].strips[0].window_groups[group_index].tabs[0].title =
        title.clone();
    tree_of(&owner).write().desktop.workspaces[0].strips[0].window_groups[group_index].tabs[0]
        .title = title;
    publication::preflight_tree(&prospective)
        .expect("the live Foreign shape still has promised spare space");
    assert!(
        persist::encode_tree(&prospective).is_err(),
        "the old final full-file cap accepted sanitized bytes that consumed the recovery reserve"
    );
    let path = directory.path().join("state.json");
    let before = std::fs::read(&path).unwrap();
    let checkpoints = Rc::new(Cell::new(0usize));
    let observed = checkpoints.clone();
    let result = tilo_files::native::atomic::test_support::with_observer(
        move |_| observed.set(observed.get() + 1),
        || owner.commit_document(request, now),
    );
    assert!(
        result.is_err(),
        "no receipt may precede the exact durable representation check"
    );
    assert_eq!(
        checkpoints.get(),
        0,
        "refusal must occur before the file writer stages data"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(owner.outgoing_status(room, now).unwrap().pending, 0);
    owner.health().unwrap();
}

#[test]
fn native_document_checkpoint_invalidates_old_authority_snapshot_without_grant_change() {
    let (_directory, owner, room, now, key) = super::tests::test_enrolled_owner();
    let tree = tree_of(&owner);
    let (witness, before, capabilities) = {
        let tree = tree.read();
        (
            tree.native_runtime.authority_witness(),
            tree.native_authority.clone(),
            tree.desktop
                .workspaces
                .iter()
                .find(|slot| slot.room == room)
                .expect("the enrolled owner must project its real room")
                .capabilities
                .clone(),
        )
    };
    assert_eq!(witness.authority_status(&before, room), Ok(()));
    let (request, _) = edit(&owner, room, &key, now, 1);
    owner.commit_document(request, now).unwrap();
    let after = tree.read();
    assert_eq!(
        after
            .desktop
            .workspaces
            .iter()
            .find(|slot| slot.room == room)
            .unwrap()
            .capabilities,
        capabilities,
        "a document checkpoint does not withdraw accepted filesystem grants",
    );
    assert_eq!(
        witness.authority_status(&before, room),
        Err(store::AuthorityRejection::StaleAcceptedSnapshot),
        "the original whole-authority snapshot must not renew current policy",
    );
    assert!(!witness.authority_is_current(&before, room));
    assert_eq!(
        witness.authority_status(&after.native_authority, room),
        Ok(())
    );
    assert!(witness.authority_is_current(&after.native_authority, room));
}

#[test]
fn accepted_checkpoint_export_repairs_an_earlier_heartbeat_projection() {
    let (_directory, owner, room, now, key) = super::tests::test_enrolled_owner();
    let tree = tree_of(&owner);
    let person = tree
        .read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room == room)
        .and_then(|workspace| workspace.owner.clone())
        .expect("the certified enrolled room must retain its actual owner");
    crate::shell::launcher::action_authority::Export::of(&tree.read()).publish();
    let (_lease, path) =
        crate::shell::launcher::action_authority::bind_for_test(room, "so.tilo.chat", &person)
            .unwrap();
    let baseline: collab_types::caps::action_authority::AppActionAuthorityProjection =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(baseline.owner.as_deref(), Some(person.as_str()));

    let writer = crate::shell::durable_writer::global();
    let (first_started_tx, first_started_rx) = std::sync::mpsc::sync_channel(0);
    let (release_first_tx, release_first_rx) = std::sync::mpsc::sync_channel(0);
    writer
        .submit_ordered(move || {
            let _ = first_started_tx.send(());
            let _ = release_first_rx.recv();
        })
        .unwrap();
    first_started_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    crate::shell::launcher::action_authority::tick(
        std::time::Instant::now() + std::time::Duration::from_secs(2),
    );
    let (second_started_tx, second_started_rx) = std::sync::mpsc::sync_channel(0);
    let (release_second_tx, release_second_rx) = std::sync::mpsc::sync_channel(0);
    writer
        .submit_ordered(move || {
            let _ = second_started_tx.send(());
            let _ = release_second_rx.recv();
        })
        .unwrap();

    let (request, _) = edit(&owner, room, &key, now, 2);
    let committed = owner.commit_document(request, now);
    let _ = release_first_tx.send(());
    let second_started = second_started_rx.recv_timeout(std::time::Duration::from_secs(3));
    let intermediate = second_started.as_ref().ok().map(|()| {
        serde_json::from_slice::<
            collab_types::caps::action_authority::AppActionAuthorityProjection,
        >(&std::fs::read(&path).unwrap())
        .unwrap()
    });
    let _ = release_second_tx.send(());
    let flushed = writer.flush_timeout(std::time::Duration::from_secs(3));

    committed.unwrap();
    second_started.unwrap();
    let intermediate = intermediate.unwrap();
    assert_eq!(intermediate.owner, None);
    assert!(intermediate.capabilities.is_empty());
    assert!(flushed, "the accepted export must cross the writer barrier");
    let repaired: collab_types::caps::action_authority::AppActionAuthorityProjection =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(
        baseline.same_policy(&repaired),
        "the final accepted export must restore the unchanged Read policy"
    );
}
