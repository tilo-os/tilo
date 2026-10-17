//! A copy of a peer costs what its admissions change, not the room.
use super::*;
use crate::TransportOrigin;
use crate::admission_tests::copy_on_write::{incremental, two_records};

/// A copy of the peer shares every part of it; a write copies only the
/// record it joins, and leaves the roster, receipts and every other record
/// shared with the original.
#[test]
fn a_write_copies_only_the_record_it_changes() {
    let (committed, key, now) = two_records();
    let mut author = committed.clone();
    let message = incremental(&mut author, &key, now);
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    let route = |record: &str| Route {
        app: "test.app".into(),
        record: record.into(),
    };
    let mut candidate = committed.clone();
    assert!(Shared::ptr_eq(
        &candidate.core.documents,
        &committed.core.documents
    ));
    candidate.receive_from(origin, message, now).unwrap();
    let shared = |record: &str| {
        backend::StoredDocument::ptr_eq(
            &candidate.core.documents[&route(record)],
            &committed.core.documents[&route(record)],
        )
    };
    assert!(!shared("tab:1"), "the written record is the copy's own");
    assert!(shared("tab:2"), "the untouched record is still shared");
    assert!(Shared::ptr_eq(
        &candidate.core.certificates,
        &committed.core.certificates
    ));
    assert!(Shared::ptr_eq(
        &candidate.core.receipts,
        &committed.core.receipts
    ));
    // What the write admitted is all the candidate holds beyond `committed`.
    let admitted = candidate.admissions_since(&committed).unwrap();
    assert_eq!(admitted.len(), 1);
    assert_eq!(admitted[0].record(), "tab:1");
    assert!(committed.admissions_since(&candidate).is_err());
}

/// The durable parts are the whole encoding cut apart. Asked again, a part
/// nothing changed is the same allocation; after a write only the written
/// record's part and the governance around it are new, and the parts
/// restore what the whole bytes restore.
#[test]
fn durable_parts_reuse_what_a_write_did_not_change() {
    let (committed, key, now) = two_records();
    let mut author = committed.clone();
    let message = incremental(&mut author, &key, now);
    let origin = TransportOrigin::RelayDelta(key.verifying_key().to_bytes());
    let before = committed.durable_parts().unwrap();
    assert_eq!(before.concat(), committed.durable_whole().unwrap());
    assert_eq!(committed.durable().unwrap(), before.concat());
    let mut candidate = committed;
    candidate.receive_from(origin, message, now).unwrap();
    let after = candidate.durable_parts().unwrap();
    assert_eq!(after.concat(), candidate.durable_whole().unwrap());
    // The governance head, then each record in route order: tab:1, tab:2.
    assert_eq!(after.len(), before.len());
    assert!(
        std::sync::Arc::ptr_eq(&before[2], &after[2]),
        "the untouched record is not encoded again"
    );
    assert!(
        !std::sync::Arc::ptr_eq(&before[1], &after[1]),
        "the written record is"
    );
    assert_ne!(before[1], after[1]);
    let restored = StatePeer::restore(
        &after.concat(),
        candidate.room(),
        candidate.authority().clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    for record in ["tab:1", "tab:2"] {
        assert_eq!(
            restored.document("test.app", record),
            candidate.document("test.app", record)
        );
    }
}
