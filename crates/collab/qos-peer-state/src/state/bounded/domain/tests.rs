use super::*;
use crate::native_footprint_vendor_tests::assert_precharged;
fn restore(
    registry: &mut PeerRegistry,
    current: NativeAllowance,
    peer: &StatePeer,
    now: SystemTime,
) -> BoundedStatePeer {
    let bytes = peer.durable().unwrap();
    let mut turn = registry.begin_turn(current, PeerLimits::default()).unwrap();
    let mut work = turn.work().unwrap();
    BoundedStatePeer::restore(
        &bytes,
        peer.room(),
        peer.authority(),
        &[],
        &RevocationList::new(),
        now,
        &mut work,
    )
    .unwrap()
}
#[test]
fn peer_domain_rejects_foreign_empty_and_nonempty_peer_reads_and_accepts_later_original_turns() {
    let (source, _, _, _, now) = crate::admission_tests::seeded();
    let empty = StatePeer::new(
        source.room(),
        source.authority().clone(),
        source.certificates(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    let initial = NativeAllowance::new(4096).unwrap();
    let mut registry = assert_precharged(&initial, || PeerRegistry::new(initial.clone())).unwrap();
    initial.finish_turn();
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let peer = restore(&mut registry, current.clone(), &source, now);
    let empty_peer = restore(
        &mut registry,
        NativeAllowance::new(64 * 1024 * 1024).unwrap(),
        &empty,
        now,
    );
    let mut foreign = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let mut foreign_turn = foreign
        .begin_turn(
            NativeAllowance::new(64 * 1024 * 1024).unwrap(),
            PeerLimits::default(),
        )
        .unwrap();
    let foreign_work = foreign_turn.work().unwrap();
    for target in [&peer, &empty_peer] {
        assert!(target.read(&foreign_work).is_err());
    }
    let later = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut turn = registry
        .begin_turn(later.clone(), PeerLimits::default())
        .unwrap();
    let work = turn.work().unwrap();
    assert_precharged(&later, || {
        let read = peer.read(&work).unwrap();
        assert_eq!(read.room(), source.room());
        assert_eq!(
            read.authority_device(),
            source.authority().device_public_key
        );
        assert!(read.document_count() > 0);
        let read = empty_peer.read(&work).unwrap();
        assert_eq!(read.document_count(), 0);
    });
}
#[test]
fn peer_domain_retains_original_brand_credit_after_registry_drop_until_empty_peer_drop() {
    let (source, _, _, _, now) = crate::admission_tests::seeded();
    let empty = StatePeer::new(
        source.room(),
        source.authority().clone(),
        source.certificates(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    let initial = NativeAllowance::new(4096).unwrap();
    let baseline = initial.retained_bytes();
    let mut registry = assert_precharged(&initial, || PeerRegistry::new(initial.clone())).unwrap();
    initial.finish_turn();
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let peer = restore(&mut registry, current.clone(), &empty, now);
    drop(registry);
    assert!(
        initial.retained_bytes() > baseline,
        "actual original domain credit follows the peer"
    );
    assert_eq!(peer.core.room, empty.room());
    drop(peer);
    assert_eq!(initial.retained_bytes(), baseline);
}
#[test]
fn peer_domain_repeated_work_calls_share_one_normalized_counter() {
    let (source, _, _, _, now) = crate::admission_tests::seeded();
    let bytes = source.durable().unwrap();
    let durable: super::super::super::DurableV4 = postcard::from_bytes(&bytes).unwrap();
    let mut total = 0;
    let probe = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let probe_set = NativeAllocationSet::new(probe.clone()).unwrap();
    let probe_turn = probe_set.work_turn(probe).unwrap();
    for input in durable.base.room_bytes.iter().map(Vec::as_slice).chain(
        durable
            .base
            .documents
            .iter()
            .map(|doc| doc.bytes.as_slice()),
    ) {
        total += probe_turn
            .normalize_input(input, &automerge::resource_limits::NativeLimits::default())
            .unwrap()
            .shape()
            .normalized_bytes();
    }
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut limits = PeerLimits::default();
    limits.normalized_bytes = total * 2 - 1;
    let mut turn = registry.begin_turn(current.clone(), limits).unwrap();
    let first = {
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
    let mut work = turn.work().unwrap();
    assert!(matches!(
        assert_precharged(&current, || BoundedStatePeer::restore(
            &bytes,
            source.room(),
            source.authority(),
            &[],
            &RevocationList::new(),
            now,
            &mut work
        )),
        Err(BoundedPeerError::Resource(
            NativeResourceError::NormalizedBytes
        ))
    ));
    assert!(first.read(&work).is_ok());
}

#[test]
fn peer_domain_startup_and_turn_refusals_precede_allocation_and_preserve_credit() {
    let initial = NativeAllowance::new(4096).unwrap();
    let held = initial.reserve(initial.available_bytes() - 1).unwrap();
    let before = initial.retained_bytes();
    assert!(matches!(
        assert_precharged(&initial, || PeerRegistry::new(initial.clone())),
        Err(BoundedPeerError::Resource(NativeResourceError::WorkBytes))
    ));
    assert_eq!(initial.retained_bytes(), before);
    drop(held);
    let mut registry = PeerRegistry::new(initial).unwrap();
    let current = NativeAllowance::new(4096).unwrap();
    let before = current.retained_bytes();
    let mut limits = PeerLimits::default();
    limits.normalized_bytes += 1;
    assert!(matches!(
        assert_precharged(&current, || registry.begin_turn(current.clone(), limits)),
        Err(BoundedPeerError::Resource(
            NativeResourceError::NormalizedBytes
        ))
    ));
    assert_eq!(current.retained_bytes(), before);
    current.finish_turn();
    assert!(matches!(
        registry.begin_turn(current, PeerLimits::default()),
        Err(BoundedPeerError::Resource(NativeResourceError::WorkBytes))
    ));
}

#[test]
fn peer_domain_sealed_empty_read_and_work_fail_and_turn_drop_seals_current_credit() {
    let (source, _, _, _, now) = crate::admission_tests::seeded();
    let empty = StatePeer::new(
        source.room(),
        source.authority().clone(),
        source.certificates(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let peer = restore(&mut registry, current.clone(), &empty, now);
    assert!(
        current.reserve(0).is_err(),
        "dropping PeerTurn ends new work"
    );
    let later = NativeAllowance::new(4096).unwrap();
    let mut turn = registry
        .begin_turn(later.clone(), PeerLimits::default())
        .unwrap();
    let work = turn.work().unwrap();
    assert!(peer.read(&work).is_ok());
    later.finish_turn();
    assert!(matches!(
        peer.read(&work),
        Err(BoundedPeerError::Resource(NativeResourceError::WorkBytes))
    ));
    drop(work);
    assert!(matches!(
        turn.work(),
        Err(BoundedPeerError::Resource(NativeResourceError::WorkBytes))
    ));
}

#[test]
fn peer_domain_native_and_outer_credit_survives_turn_and_registry_until_last_peer_drop() {
    let (source, _, _, _, now) = crate::admission_tests::seeded();
    let initial = NativeAllowance::new(4096).unwrap();
    let initial_baseline = initial.retained_bytes();
    let mut registry = PeerRegistry::new(initial.clone()).unwrap();
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let current_baseline = current.retained_bytes();
    let peer = restore(&mut registry, current.clone(), &source, now);
    drop(registry);
    initial.finish_turn();
    assert!(initial.retained_bytes() > initial_baseline);
    assert!(current.retained_bytes() > current_baseline);
    assert!(!peer.core.documents.is_empty());
    drop(peer);
    assert_eq!(initial.retained_bytes(), initial_baseline);
    assert_eq!(current.retained_bytes(), current_baseline);
}

#[test]
fn peer_domain_outer_output_spends_current_turn_and_keeps_credit_after_turn_and_registry_drop() {
    let initial = NativeAllowance::new(4096).unwrap();
    let mut registry = PeerRegistry::new(initial.clone()).unwrap();
    let initial_before = initial.retained_bytes();
    let current = NativeAllowance::new(4096).unwrap();
    let baseline = current.retained_bytes();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let work = turn.work().unwrap();
    let remaining = current.available_bytes();
    let output = assert_precharged(&current, || work.reserve_output(remaining)).unwrap();
    assert!(matches!(
        work.reserve_output(1),
        Err(BoundedPeerError::Resource(NativeResourceError::WorkBytes))
    ));
    assert_eq!(initial.retained_bytes(), initial_before);
    assert_eq!(current.available_bytes(), 0);
    drop(work);
    drop(turn);
    drop(registry);
    assert!(current.reserve(0).is_err());
    assert_eq!(current.retained_bytes(), baseline + remaining);
    drop(output);
    assert_eq!(current.retained_bytes(), baseline);
}
