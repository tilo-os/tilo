use crate::native_footprint_vendor_tests::assert_precharged;
use crate::{BoundedStatePeer, PeerLimits, PeerRegistry, StatePeer};
use automerge::resource_limits::NativeAllowance;
use tilo_user_identity::RevocationList;
fn allowance() -> NativeAllowance {
    NativeAllowance::new(64 * 1024 * 1024).unwrap()
}
#[test]
fn real_signed_v5_exact_membership_matches_legacy_empty_baseline_without_allocating() {
    let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
    let v4 = peer.durable().unwrap();
    peer.select_admission_v5().unwrap();
    let empty_v5 = peer.durable().unwrap();
    peer.publish_document(
        "test.app",
        "tab:1",
        peer.document("test.app", "tab:1").unwrap(),
        &key,
        now,
    )
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
    let operations: Vec<_> = expected.iter().map(|v| v.operation_id()).collect();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    for (bytes, selected, has_operations) in [
        (v4, false, false),
        (empty_v5, true, false),
        (v5, true, true),
    ] {
        let current = allowance();
        let mut turn = registry
            .begin_turn(current.clone(), PeerLimits::default())
            .unwrap();
        let mut work = turn.work().unwrap();
        let restored = BoundedStatePeer::restore(
            &bytes,
            peer.room(),
            peer.authority(),
            &[],
            &RevocationList::new(),
            now,
            &mut work,
        )
        .unwrap();
        let exhausted = work.reserve_output(current.available_bytes()).unwrap();
        let read = assert_precharged(&current, || restored.read(&work)).unwrap();
        assert_eq!(
            assert_precharged(&current, || read.admission_v5_selected()),
            selected
        );
        for id in &operations {
            assert_eq!(
                assert_precharged(&current, || read.contains_admitted_operation(id)),
                has_operations
            );
        }
        assert!(!assert_precharged(&current, || read.contains_admitted_operation(&[0; 32])));
        drop(exhausted);
    }
}
#[test]
fn exact_facts_require_actual_peer_domain_and_current_read_turn() {
    let (mut peer, _, _, _, now) = crate::admission_tests::seeded();
    peer.select_admission_v5().unwrap();
    let bytes = peer.durable().unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let current = allowance();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let mut work = turn.work().unwrap();
    let restored = BoundedStatePeer::restore(
        &bytes,
        peer.room(),
        peer.authority(),
        &[],
        &RevocationList::new(),
        now,
        &mut work,
    )
    .unwrap();
    let mut foreign = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let mut other_turn = foreign
        .begin_turn(allowance(), PeerLimits::default())
        .unwrap();
    let other = other_turn.work().unwrap();
    assert!(restored.read(&other).is_err());
    assert!(restored.read(&work).unwrap().admission_v5_selected());
    current.finish_turn();
    assert!(restored.read(&work).is_err());
}
