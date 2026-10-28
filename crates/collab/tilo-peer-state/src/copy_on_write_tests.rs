//! One admitted document costs one document: a staged candidate shares every
//! other document with the committed peer, and its durable snapshot saves
//! only what the admission changed.
use super::*;
use crate::cached_document::take_copies_and_saves;
use crate::{AdmissionError, InputDisposition};

const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

/// A native peer holding a small record and a large untouched one.
pub(crate) fn two_records() -> (StatePeer, SigningKey, SystemTime) {
    let (a, ac, b, bc, now) = fixtures();
    let mut native = StatePeer::new(
        ROOM,
        ac.clone(),
        vec![ac.clone(), bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let document = |value: String| {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "value", value).unwrap();
        doc.commit_edit("fixture", "value");
        doc.save()
    };
    let seed = |record: &str, bytes| SeedDocument {
        app_id: "test.app".into(),
        record_id: record.into(),
        kind: "test".into(),
        bytes,
    };
    native
        .install_seed(
            SeedState {
                room_changes: seed_room(ROOM, &ac, &[ac.clone(), bc]).unwrap(),
                documents: vec![
                    seed("tab:1", document("native".into())),
                    seed("tab:2", document("large ".repeat(16 * 1024))),
                ],
            },
            &a,
            now,
        )
        .unwrap();
    native.select_admission_v5().unwrap();
    (native, b, now)
}

pub(crate) fn incremental(
    author: &mut StatePeer,
    key: &SigningKey,
    now: SystemTime,
) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&author.document("test.app", "tab:1").unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(ROOT, "typed", "a").unwrap();
    doc.commit_edit("fixture", "typed");
    author
        .publication_mut()
        .publish_incremental("test.app", "tab:1", doc.save_after(&heads), key, now)
        .unwrap()
}

#[test]
fn admitting_one_document_copies_and_saves_only_that_document() {
    let (committed, key, now) = two_records();
    let mut author = committed.clone();
    let message = incremental(&mut author, &key, now);
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    let before = committed.durable().unwrap();
    // As the owner admits: the committed view stays readable while a staged
    // candidate (here two clones deep) takes the message.
    let view = committed.clone();
    let mut candidate = view.clone();
    take_copies_and_saves();
    let report = candidate.receive_from_report(origin, message, now).unwrap();
    assert!(matches!(report.primary(), InputDisposition::Accepted(_)));
    assert_eq!(
        take_copies_and_saves(),
        (1, 0),
        "admission copies the one document it changes"
    );
    let durable = candidate.durable().unwrap();
    assert_eq!(
        take_copies_and_saves(),
        (0, 1),
        "the durable snapshot saves only the changed document again"
    );
    assert_eq!(committed.durable().unwrap(), before);
    assert_eq!(view.durable().unwrap(), before);
    assert_eq!(take_copies_and_saves(), (0, 0));
    assert_eq!(
        candidate.document("test.app", "tab:2"),
        committed.document("test.app", "tab:2")
    );
    assert_ne!(
        candidate.document("test.app", "tab:1"),
        committed.document("test.app", "tab:1")
    );
    let restored = StatePeer::restore(
        &durable,
        candidate.room(),
        candidate.authority().clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    // Byte equality would also compare per-author watermark order, which
    // durable() takes from a hash map; the saved documents are what is shared.
    assert_eq!(restored.rows(), candidate.rows());
    for record in ["tab:1", "tab:2"] {
        assert_eq!(
            restored.document("test.app", record),
            candidate.document("test.app", record)
        );
    }
}

#[test]
fn refused_message_leaves_state_identical() {
    let (mut receiver, key, now) = two_records();
    let mut author = receiver.clone();
    let message = incremental(&mut author, &key, now);
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    let before = receiver.durable().unwrap();
    let mut forged = message.clone();
    if let NeighbourMsg::AppDocument { envelope, .. } = &mut forged {
        envelope.payload.push(0);
    }
    take_copies_and_saves();
    assert!(receiver.receive_from_report(origin, forged, now).is_err());
    assert_eq!(receiver.durable().unwrap(), before);
    assert_eq!(take_copies_and_saves(), (0, 0));
    let mut conflicting = message.clone();
    receiver.receive_from_report(origin, message, now).unwrap();
    let admitted = receiver.durable().unwrap();
    assert_ne!(admitted, before);
    take_copies_and_saves();
    if let NeighbourMsg::AppDocument { envelope, .. } = &mut conflicting {
        envelope.timestamp += Duration::from_secs(1);
        collab_types::sign_op(envelope, &key).unwrap();
    }
    assert!(matches!(
        receiver.receive_from_report(origin, conflicting, now),
        Err(AdmissionError::ConflictingSequence)
    ));
    assert_eq!(receiver.durable().unwrap(), admitted);
    assert_eq!(
        take_copies_and_saves(),
        (0, 0),
        "a refused message neither copies nor saves a document"
    );
}
