//! A superseded checkpoint (governed coverage strictly newer, same route and
//! author) is skipped unapplied; every other coverage mismatch keeps failing.
use super::*;
use crate::{InputDisposition, MissingPrerequisite};

fn snapshot(
    author: &mut StatePeer,
    key: &SigningKey,
    now: SystemTime,
    value: &str,
) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&author.document("test.app", "tab:1").unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    doc.put(ROOT, "value", value).unwrap();
    doc.commit();
    author
        .publish_document("test.app", "tab:1", doc.save(), key, now)
        .unwrap()
}
fn seq(message: &NeighbourMsg) -> u64 {
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        panic!("not a document")
    };
    envelope.seq
}
fn heads(peer: &StatePeer) -> Vec<automerge::ChangeHash> {
    AutoCommit::load(&peer.document("test.app", "tab:1").unwrap())
        .unwrap()
        .get_heads()
}
/// Host outbox from the double-publish bug: [snapshot N, SyncResp(room with
/// coverage N+1), snapshot N+1], plus the receiver after that whole batch.
struct DoublePublish {
    receiver: StatePeer,
    author: StatePeer,
    origin: TransportOrigin,
    older: NeighbourMsg,
    later: SystemTime,
}
fn double_publish() -> DoublePublish {
    let (mut receiver, key, _, _, now) = crate::admission_tests::seeded();
    receiver.select_admission_v5().unwrap();
    let mut author = receiver.clone();
    let later = now.checked_add(Duration::from_secs(301)).unwrap();
    let older = snapshot(&mut author, &key, later, "N");
    let newer = snapshot(&mut author, &key, later, "N+1");
    assert_eq!(seq(&newer), seq(&older).checked_add(1).unwrap());
    let room = author.room_message();
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());

    let first = receiver
        .receive_from_report(origin, older.clone(), later)
        .unwrap();
    assert_eq!(
        first.primary(),
        &InputDisposition::RetryLater(MissingPrerequisite::CheckpointCoverage)
    );
    assert_eq!(receiver.pending_count(), 1);
    // The governance drain retries N against coverage N+1: skipped, not rejected.
    let governance = receiver.receive_from_report(origin, room, later).unwrap();
    assert_eq!(governance.summary().rejected, 0);
    assert!(governance.settled().is_empty(), "superseded N was admitted");
    assert_eq!(receiver.pending_count(), 0);
    assert!(!receiver.fresh_checkpoint_for("test.app", "tab:1", later));
    // N+1 is still admitted with full verification in the same batch.
    let admitted = receiver.receive_from_report(origin, newer, later).unwrap();
    assert!(
        matches!(admitted.primary(), InputDisposition::Accepted(_)),
        "N+1 not admitted: {:?}",
        admitted.primary()
    );
    DoublePublish {
        receiver,
        author,
        origin,
        older,
        later,
    }
}

#[test]
fn superseded_checkpoint_in_double_publish_outbox_is_skipped_and_newer_admitted() {
    let DoublePublish {
        receiver, author, ..
    } = double_publish();
    assert_eq!(heads(&receiver), heads(&author));
    assert_eq!(receiver.rows(), author.rows());
    assert_eq!(receiver.replay_floors(), author.replay_floors());
}

#[test]
fn superseded_checkpoint_skip_directly_after_governance_does_not_apply() {
    // Coverage N+1 is known before N arrives: N settles as a skip (the cursor
    // may advance), and nothing from it enters state.
    let (mut receiver, key, _, _, now) = crate::admission_tests::seeded();
    receiver.select_admission_v5().unwrap();
    let mut author = receiver.clone();
    let later = now.checked_add(Duration::from_secs(301)).unwrap();
    let older = snapshot(&mut author, &key, later, "N");
    let newer = snapshot(&mut author, &key, later, "N+1");
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    receiver
        .receive_from_report(origin, author.room_message(), later)
        .unwrap();
    let rows = receiver.rows();
    let floors = receiver.replay_floors();
    let durable = receiver.durable().unwrap();
    let skipped = receiver.receive_from_report(origin, older, later).unwrap();
    assert_eq!(skipped.primary(), &InputDisposition::MetadataOnly);
    assert!(skipped.settled().is_empty());
    assert!(!skipped.summary().changed && !skipped.summary().deferred);
    assert_eq!(receiver.pending_count(), 0);
    assert_eq!(receiver.rows(), rows);
    assert_eq!(receiver.replay_floors(), floors);
    assert_eq!(receiver.durable().unwrap(), durable);
    assert!(!receiver.fresh_checkpoint_for("test.app", "tab:1", later));
    let admitted = receiver.receive_from_report(origin, newer, later).unwrap();
    assert!(matches!(admitted.primary(), InputDisposition::Accepted(_)));
    assert_eq!(heads(&receiver), heads(&author));
}

#[test]
fn superseded_checkpoint_lower_coverage_is_never_skipped() {
    let (mut receiver, key, _, _, now) = crate::admission_tests::seeded();
    receiver.select_admission_v5().unwrap();
    let mut author = receiver.clone();
    let later = now.checked_add(Duration::from_secs(301)).unwrap();
    let covered = snapshot(&mut author, &key, later, "N");
    let room = author.room_message();
    let ahead = snapshot(&mut author, &key, later, "N+1");
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    receiver.receive_from_report(origin, room, later).unwrap();
    let route = Route {
        app: "test.app".into(),
        record: "tab:1".into(),
    };
    let source = key.verifying_key().to_bytes();
    // Coverage N governs; a snapshot at N+1 is ahead of coverage, not superseded.
    assert!(
        !receiver
            .superseded_checkpoint(&route, &source, seq(&ahead))
            .unwrap()
    );
    assert!(
        !receiver
            .superseded_checkpoint(&route, &source, seq(&covered))
            .unwrap()
    );
    // A different author, or a different route, is never a supersession.
    assert!(!receiver.superseded_checkpoint(&route, &[9; 32], 0).unwrap());
    let other = Route {
        app: "test.app".into(),
        record: "tab:2".into(),
    };
    assert!(!receiver.superseded_checkpoint(&other, &source, 0).unwrap());
    let rows = receiver.rows();
    let report = receiver.receive_from_report(origin, ahead, later).unwrap();
    assert_eq!(
        report.primary(),
        &InputDisposition::RetryLater(MissingPrerequisite::CheckpointCoverage)
    );
    assert_eq!(receiver.rows(), rows);
}

#[test]
fn superseded_checkpoint_same_sequence_different_digest_stays_an_error() {
    let (mut receiver, key, _, _, now) = crate::admission_tests::seeded();
    receiver.select_admission_v5().unwrap();
    let mut author = receiver.clone();
    let mut forger = receiver.clone();
    let later = now.checked_add(Duration::from_secs(301)).unwrap();
    let genuine = snapshot(&mut author, &key, later, "genuine");
    let counterfeit = snapshot(&mut forger, &key, later, "forged");
    assert_eq!(seq(&genuine), seq(&counterfeit));
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    receiver
        .receive_from_report(origin, author.room_message(), later)
        .unwrap();
    let rows = receiver.rows();
    let durable = receiver.durable().unwrap();
    assert!(
        receiver
            .receive_from_report(origin, counterfeit, later)
            .is_err()
    );
    assert_eq!(receiver.rows(), rows);
    assert_eq!(receiver.durable().unwrap(), durable);
    let admitted = receiver
        .receive_from_report(origin, genuine, later)
        .unwrap();
    assert!(matches!(admitted.primary(), InputDisposition::Accepted(_)));
}

#[test]
fn superseded_checkpoint_after_newer_admitted_is_skipped_idempotently() {
    let DoublePublish {
        mut receiver,
        author,
        origin,
        older,
        later,
    } = double_publish();
    let rows = receiver.rows();
    let floors = receiver.replay_floors();
    let durable = receiver.durable().unwrap();
    for _ in 0..2 {
        let report = receiver
            .receive_from_report(origin, older.clone(), later)
            .unwrap();
        assert_eq!(report.primary(), &InputDisposition::MetadataOnly);
        assert!(report.settled().is_empty());
        assert!(!report.summary().changed && !report.summary().deferred);
        assert_eq!(receiver.pending_count(), 0);
        assert_eq!(receiver.rows(), rows);
        assert_eq!(receiver.replay_floors(), floors);
        assert_eq!(receiver.durable().unwrap(), durable);
    }
    // Legacy receive agrees: a settled duplicate-like skip, never an error.
    assert!(
        receiver
            .receive_from(origin, older, later)
            .unwrap()
            .duplicate
    );
    assert_eq!(heads(&receiver), heads(&author));
}
