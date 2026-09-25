use super::*;
use crate::native_footprint_vendor_tests::assert_precharged;
use automerge::resource_limits::{NativeScalar, NativeSelection, NativeValue};
use automerge::{ActorId, AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use collab_types::Ed25519PublicKey;
use collab_types::caps::{CapOverride, Scope, Subject, Verb};

const APP: &str = "test.app";
const RECORD: &str = "tab:1";

fn with_restored(
    source: &StatePeer,
    now: SystemTime,
    run: impl FnOnce(&BoundedStatePeer, &PeerWork<'_>, &NativeAllowance, &NativeAllowance),
) {
    let bytes = source.durable().unwrap();
    let resident = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let peer = {
        let mut turn = registry
            .begin_turn(resident.clone(), PeerLimits::default())
            .unwrap();
        let mut work = turn.work().unwrap();
        BoundedStatePeer::restore(
            &bytes,
            source.room(),
            source.authority(),
            &[],
            &RevocationList::new(),
            now,
            &mut work,
        )
        .unwrap()
    };
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let work = turn.work().unwrap();
    run(&peer, &work, &current, &resident);
}

fn document(source: &mut StatePeer) -> &mut AutoCommit {
    &mut source
        .core
        .documents
        .iter_mut()
        .find(|(route, _)| route.app == APP && route.record == RECORD)
        .unwrap()
        .1
        .doc
}

#[test]
fn exact_admitted_document_lends_real_content_and_complete_unchanged_heads() {
    let (mut source, _, _, cert, now) = crate::admission_tests::seeded();
    let expected_heads = document(&mut source).base_heads();
    with_restored(&source, now, |peer, work, current, resident| {
        let resident_before = resident.retained_bytes();
        let current_before = current.retained_bytes();
        let peer_read = peer.read(work).unwrap();
        assert_precharged(current, || {
            let view = peer_read
                .document_read(cert.device_public_key, APP, RECORD, now)
                .unwrap();
            assert_eq!((view.app(), view.record()), (APP, RECORD));
            assert_eq!(view.heads(), expected_heads);
            assert_eq!(view.read().pending_ops(), 0);
            let count = view
                .read()
                .visit_property(view.root(), "value", NativeSelection::Winner, |value, _| {
                    assert!(matches!(
                        value,
                        NativeValue::Scalar(NativeScalar::Str("native"))
                    ));
                    Ok::<_, ()>(())
                })
                .unwrap();
            assert_eq!(count, 1);
            let again = peer_read
                .document_read(cert.device_public_key, APP, RECORD, now)
                .unwrap();
            assert_eq!(again.heads(), view.heads());
            assert_eq!(again.read().pending_ops(), 0);
        });
        assert_eq!(current.retained_bytes(), current_before);
        assert_eq!(resident.retained_bytes(), resident_before);
    });
}

#[test]
fn route_lookup_is_exact_and_permission_precedes_missing_route_observation() {
    let (source, _, _, cert, now) = crate::admission_tests::seeded();
    with_restored(&source, now, |peer, work, current, _| {
        let read = peer.read(work).unwrap();
        assert_precharged(current, || {
            for (app, record) in [(APP, ""), (APP, "tab:2"), ("other.app", RECORD)] {
                assert!(matches!(
                    read.document_read(cert.device_public_key, app, record, now),
                    Err(PeerDocumentReadError::Missing)
                ));
            }
            for (app, record) in [(APP, RECORD), ("other.app", "missing")] {
                assert!(matches!(
                    read.document_read([0x77; 32], app, record, now),
                    Err(PeerDocumentReadError::Peer(BoundedPeerError::Invalid(
                        BoundedPeerInvalid::Certificate
                    )))
                ));
            }
        });
    });
}

#[test]
fn current_membership_expiry_is_rechecked_on_each_document_read() {
    let (mut source, _, _, cert, now) = crate::admission_tests::seeded();
    let room = source.core.room_doc.as_mut().unwrap();
    room.put(ROOT, "owner", "different owner").unwrap();
    let caps = room.get(ROOT, "capabilities").unwrap().unwrap().1;
    let keys: Vec<_> = room.keys(&caps).collect();
    for key in keys {
        room.delete(&caps, key).unwrap();
    }
    let cap = CapOverride::grant(
        Subject::User(Ed25519PublicKey::from_bytes(cert.user_public_key)),
        Scope::Workspace,
        Verb::Read,
    )
    .with_expiry(crate::state::now_secs(now) + 1);
    room.put(
        &caps,
        "member",
        automerge::ScalarValue::Bytes(serde_json::to_vec(&cap).unwrap()),
    )
    .unwrap();
    room.commit();
    assert!(
        source
            .check_document_read(cert.device_public_key, APP, RECORD, now)
            .is_ok()
    );
    with_restored(&source, now, |peer, work, current, _| {
        let read = peer.read(work).unwrap();
        assert_precharged(current, || {
            assert!(
                read.document_read(cert.device_public_key, APP, RECORD, now)
                    .is_ok()
            );
            let expired = now + Duration::from_secs(2);
            assert!(matches!(
                read.document_read(cert.device_public_key, APP, RECORD, expired),
                Err(PeerDocumentReadError::Peer(BoundedPeerError::Invalid(
                    BoundedPeerInvalid::Governance
                )))
            ));
        });
    });
}

#[test]
fn encrypted_admitted_document_is_unavailable_even_when_caller_has_read_permission() {
    let (mut source, _, _, cert, now) = crate::admission_tests::seeded();
    source
        .core
        .documents
        .values_mut()
        .next()
        .unwrap()
        .encryption_required = true;
    with_restored(&source, now, |peer, work, current, _| {
        let read = peer.read(work).unwrap();
        assert!(
            read.document_read_allowed(cert.device_public_key, APP, RECORD, now)
                .unwrap()
        );
        assert_precharged(current, || {
            assert!(matches!(
                read.document_read(cert.device_public_key, APP, RECORD, now),
                Err(PeerDocumentReadError::Encrypted)
            ));
        });
    });
}

#[test]
fn retained_real_orphan_cannot_be_read_as_a_complete_document() {
    let (mut source, _, _, cert, now) = crate::admission_tests::seeded();
    let mut history = AutoCommit::new();
    history.put(ROOT, "value", "first").unwrap();
    history.commit();
    let first = history.base_heads();
    history.put(ROOT, "value", "second").unwrap();
    history.commit();
    let mut orphan = AutoCommit::new();
    orphan
        .load_incremental(&history.save_after(&first))
        .unwrap();
    assert_eq!(ReadDoc::get_missing_deps(&orphan, &[]).len(), 1);
    *document(&mut source) = orphan;
    with_restored(&source, now, |peer, work, current, _| {
        let read = peer.read(work).unwrap();
        assert_precharged(current, || {
            assert!(matches!(
                read.document_read(cert.device_public_key, APP, RECORD, now),
                Err(PeerDocumentReadError::Incomplete)
            ));
        });
    });
}

#[test]
fn complete_frontier_accepts_sixty_four_and_refuses_sixty_five_without_prefix_or_commit() {
    for count in [64u8, 65] {
        let (mut source, _, _, cert, now) = crate::admission_tests::seeded();
        let base = document(&mut source).clone();
        let mut combined = base.clone();
        for actor in 1..=count {
            let mut branch = base.clone();
            branch.set_actor(ActorId::from(vec![actor]));
            branch.put(ROOT, "concurrent", i64::from(actor)).unwrap();
            branch.commit();
            combined.merge(&mut branch).unwrap();
        }
        let expected = combined.base_heads();
        assert_eq!(expected.len(), usize::from(count));
        *document(&mut source) = combined;
        with_restored(&source, now, |peer, work, current, resident| {
            let read = peer.read(work).unwrap();
            let before = current.retained_bytes();
            let resident_before = resident.retained_bytes();
            assert_precharged(current, || {
                let result = read.document_read(cert.device_public_key, APP, RECORD, now);
                if count == 64 {
                    let view = result.unwrap();
                    assert_eq!(view.heads().len(), 64);
                    for head in &expected {
                        assert_eq!(
                            view.heads().iter().filter(|actual| *actual == head).count(),
                            1
                        );
                    }
                    assert_eq!(view.read().pending_ops(), 0);
                } else {
                    assert!(matches!(
                        result,
                        Err(PeerDocumentReadError::Peer(BoundedPeerError::Resource(
                            NativeResourceError::Collection
                        )))
                    ));
                }
            });
            assert_eq!(current.retained_bytes(), before);
            assert_eq!(resident.retained_bytes(), resident_before);
        });
    }
}

#[test]
fn current_turn_exhaustion_and_seal_refuse_even_with_a_previous_peer_read() {
    let (source, _, _, cert, now) = crate::admission_tests::seeded();
    with_restored(&source, now, |peer, work, current, resident| {
        let read = peer.read(work).unwrap();
        let resident_before = resident.retained_bytes();
        let held = work.reserve_output(current.available_bytes()).unwrap();
        assert_precharged(current, || {
            assert!(matches!(
                read.document_read(cert.device_public_key, APP, RECORD, now),
                Err(PeerDocumentReadError::Peer(BoundedPeerError::Resource(
                    NativeResourceError::WorkBytes
                )))
            ));
        });
        drop(held);
        assert!(
            read.document_read(cert.device_public_key, APP, RECORD, now)
                .is_ok()
        );
        current.finish_turn();
        assert!(matches!(
            read.document_read(cert.device_public_key, APP, RECORD, now),
            Err(PeerDocumentReadError::Peer(BoundedPeerError::Resource(
                NativeResourceError::WorkBytes
            )))
        ));
        assert_eq!(resident.retained_bytes(), resident_before);
    });
}
