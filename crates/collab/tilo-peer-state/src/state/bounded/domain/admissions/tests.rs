use crate::{BoundedStatePeer, PeerLimits, StatePeer};
use tilo_user_identity::RevocationList;
#[test]
fn real_signed_v5_exact_membership_matches_legacy_empty_baseline() {
    let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
    let v4 = peer.durable().unwrap();
    peer.select_admission_v5().unwrap();
    let empty_v5 = peer.durable().unwrap();
    let current_document = peer.document("test.app", "tab:1").unwrap();
    peer.publication_mut()
        .publish_document("test.app", "tab:1", current_document, &key, now)
        .unwrap();
    let v5 = peer.durable().unwrap();
    let empty = StatePeer::new(
        peer.room(),
        peer.authority().clone(),
        vec![peer.authority().clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let expected = peer.admissions_since(&empty).unwrap();
    assert!(!expected.is_empty());
    let operations: Vec<_> = expected
        .iter()
        .map(crate::admission::DocumentAdmission::operation_id)
        .collect();
    for (bytes, selected, has_operations) in [
        (v4, false, false),
        (empty_v5, true, false),
        (v5, true, true),
    ] {
        let restored = BoundedStatePeer::restore(
            &bytes,
            peer.room(),
            peer.authority(),
            &[],
            &RevocationList::new(),
            now,
            &PeerLimits::default(),
        )
        .unwrap();
        let read = restored.read();
        assert_eq!(read.admission_v5_selected(), selected);
        for id in &operations {
            assert_eq!(read.contains_admitted_operation(id), has_operations);
        }
        assert!(!read.contains_admitted_operation(&[0; 32]));
    }
}
