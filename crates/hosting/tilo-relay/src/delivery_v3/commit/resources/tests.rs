use super::*;
use automerge::resource_limits::{NativeAllocationSet, NativeAllowance};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};

fn work(bytes: usize) -> (NativeAllowance, NativeWorkTurn) {
    let allowance = NativeAllowance::new(bytes).unwrap();
    let allocations = NativeAllocationSet::new(allowance.clone()).unwrap();
    let turn = allocations.work_turn(allowance.clone()).unwrap();
    (allowance, turn)
}
fn populated() -> DeliveryLedgerV1 {
    let mut ledger = DeliveryLedgerV1 {
        version: 1,
        store_generation: [1; 16],
        resolved_through: 24,
        replay: BTreeMap::new(),
        admissions: BTreeMap::new(),
        consumers: BTreeMap::new(),
        pins: BTreeMap::new(),
    };
    for n in 1u8..=24 {
        let reference = CommitRef {
            sequence: n.into(),
            digest: [n; 32],
        };
        let publications = vec![
            PublicationRef {
                payload: [n; 32],
                kind: Retention::Delta
            };
            17
        ];
        ledger.replay.insert(
            n.into(),
            ReplayCommit {
                owner_generation: n.into(),
                previous: (n > 1).then(|| CommitRef {
                    sequence: u64::from(n - 1),
                    digest: [n - 1; 32],
                }),
                digest: [n; 32],
                publications: publications.clone(),
                admitted_operations: vec![[n; 32]; 9],
                committed_at_ns: u64::MAX,
            },
        );
        ledger.admissions.insert(
            ReceiptKey {
                device: [n; 32],
                delivery: [n; 16],
            },
            AdmissionRecord {
                digest: [n; 32],
                operations: vec![[n; 32]; 5],
                committed: reference.clone(),
                result: DurableAdmissionKind::Accepted,
            },
        );
        ledger.consumers.insert(
            [n; 32],
            ConsumerState {
                id: [n; 16],
                device: [n; 32],
                root: [3; 32],
                authorization_view: [4; 32],
                acknowledged: Some(reference.clone()),
            },
        );
        ledger.pins.insert(
            [n; 16],
            SnapshotPin {
                consumer: [n; 16],
                authorization_view: [4; 32],
                watermark: reference,
                publications,
                expires_at_ns: u64::MAX,
            },
        );
    }
    ledger
}
// The sole allocator of this test binary observes actual requested layouts.
// Accounting is checked before allocation and before final deallocation, so
// an Arc-internal reservation that releases its own header credit too soon fails.
#[derive(Clone, Copy, Debug, Default)]
struct Meter {
    live: usize,
    peak: usize,
    unreserved: usize,
    unmatched: bool,
}
thread_local! {
    static METER: Cell<Option<Meter>> = const { Cell::new(None) };
    static CREDIT: RefCell<Option<(NativeAllowance,usize)>> = const { RefCell::new(None) };
}
fn check(m: &mut Meter) {
    let _ = CREDIT.try_with(|c| {
        if let Some((credit, baseline)) = &*c.borrow() {
            m.unreserved = m.unreserved.max(
                m.live
                    .saturating_sub(credit.retained_bytes().saturating_sub(*baseline)),
            );
        }
    });
}
fn allocate(bytes: usize) {
    let _ = METER.try_with(|cell| {
        if let Some(mut m) = cell.get() {
            m.live += bytes;
            m.peak = m.peak.max(m.live);
            check(&mut m);
            cell.set(Some(m));
        }
    });
}
fn deallocate(bytes: usize) {
    let _ = METER.try_with(|cell| {
        if let Some(mut m) = cell.get() {
            check(&mut m);
            match m.live.checked_sub(bytes) {
                Some(n) => m.live = n,
                None => m.unmatched = true,
            }
            cell.set(Some(m));
        }
    });
}
struct Measured;
// SAFETY: System receives the exact requested layouts/pointers. Observation uses
// only nonallocating thread-local scalar counters and native atomic counters.
unsafe impl GlobalAlloc for Measured {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        allocate(l.size());
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        allocate(l.size());
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        deallocate(l.size());
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        allocate(n);
        let result = unsafe { System.realloc(p, l, n) };
        deallocate(l.size());
        result
    }
}
#[global_allocator]
static ALLOCATOR: Measured = Measured;
fn measured<T>(allowance: &NativeAllowance, f: impl FnOnce() -> T) -> (T, Meter) {
    CREDIT.with(|c| *c.borrow_mut() = Some((allowance.clone(), allowance.retained_bytes())));
    METER.with(|m| m.set(Some(Meter::default())));
    let result = f();
    let meter = METER.with(|m| m.take().unwrap());
    CREDIT.with(|c| *c.borrow_mut() = None);
    assert_eq!(
        meter.unreserved, 0,
        "allocation or final deallocation outlived credit: {meter:?}"
    );
    assert!(!meter.unmatched, "unmeasured allocation freed: {meter:?}");
    (result, meter)
}
#[test]
fn commit_resources_ledger_decode_fork_and_error_allocations_are_precharged() {
    let ledger = populated();
    let bytes = ledger.encode().unwrap();
    let (credit, turn) = work(8 * 1024 * 1024);
    let baseline = credit.retained_bytes();
    let (held, m) = measured(&credit, || HeldDeliveryLedger::decode(&bytes, &turn));
    let held = held.unwrap();
    assert!(m.peak > 10_000);
    assert_eq!(held.ledger(), &ledger);
    let (fork, m) = measured(&credit, || held.fork(&turn));
    let fork = fork.unwrap();
    assert!(m.peak > 10_000);
    assert_eq!(fork.ledger(), &ledger);
    assert_ne!(
        held.ledger().replay[&1].publications.as_ptr(),
        fork.ledger().replay[&1].publications.as_ptr()
    );
    drop(held);
    drop(fork);
    assert_eq!(credit.retained_bytes(), baseline);
    let mut invalid = ledger;
    invalid.consumers.get_mut(&[24; 32]).unwrap().id = [1; 16];
    let bytes = postcard::to_allocvec(&invalid).unwrap();
    let (result, m) = measured(&credit, || HeldDeliveryLedger::decode(&bytes, &turn));
    assert!(matches!(
        result,
        Err(ResourceError::Commit(CommitError::Invalid(
            "consumer identity"
        )))
    ));
    assert_eq!(m.live, 0);
    assert_eq!(credit.retained_bytes(), baseline);
}
#[test]
fn commit_resources_pool_keeps_original_credit_through_fork_pin_and_final_allocation() {
    let mut pool = PublicationPool::new(1 << 20).unwrap();
    let id = pool.intern(&vec![37; 64 * 1024]).unwrap();
    let bytes = pool.encode(1 << 20).unwrap();
    let (credit, turn) = work(2 << 20);
    let baseline = credit.retained_bytes();
    let (_, m) = measured(&credit, || {
        let held = HeldPublicationPool::decode(&bytes, 1 << 20, &turn).unwrap();
        let pointer = held.pool().resolve(&id).unwrap().as_ptr();
        let backing = held.pool().retained_allocation_bytes(&id).unwrap();
        let fork = held.fork(&turn).unwrap();
        assert_eq!(fork.pool().resolve(&id).unwrap().as_ptr(), pointer);
        let pin = fork.pool().pin(&id).unwrap();
        drop(held);
        drop(fork);
        assert_eq!(credit.retained_bytes() - baseline, backing);
        assert_eq!(pin.bytes().as_ptr(), pointer);
        assert_eq!(pin.bytes().len(), 64 * 1024);
        drop(pin);
        assert_eq!(credit.retained_bytes(), baseline);
    });
    assert_eq!(m.live, 0);
}
#[test]
fn commit_resources_pool_fork_pays_only_map_and_pin_survives_original_turn() {
    let mut pool = PublicationPool::new(1 << 20).unwrap();
    let id = pool.intern(&vec![8; 256 * 1024]).unwrap();
    let bytes = pool.encode(1 << 20).unwrap();
    let (original, turn) = work(2 << 20);
    let held = HeldPublicationPool::decode(&bytes, 1 << 20, &turn).unwrap();
    let (next, next_turn) = work(32 * 1024);
    let (fork, m) = measured(&next, || held.fork(&next_turn));
    let fork = fork.unwrap();
    assert!(m.peak < 8192);
    assert!(m.peak > 0);
    let pin = fork.pool().pin(&id).unwrap();
    let pointer = pin.bytes().as_ptr();
    drop(held);
    drop(turn);
    original.finish_turn();
    drop(fork);
    drop(next_turn);
    next.finish_turn();
    let retained = original.retained_bytes();
    assert!(retained > 256 * 1024);
    assert_eq!(pin.bytes().as_ptr(), pointer);
    drop(pin);
    assert!(original.retained_bytes() < retained - 256 * 1024);
}
#[test]
fn commit_resources_exhaustion_refuses_without_heap_or_partial_credit() {
    let ledger = populated().encode().unwrap();
    let mut pool = PublicationPool::new(1024).unwrap();
    pool.intern(b"retained").unwrap();
    let pool = pool.encode(2048).unwrap();
    let (credit, turn) = work(64 * 1024);
    let occupied = turn.reserve(credit.available_bytes()).unwrap();
    let (result, m) = measured(&credit, || HeldDeliveryLedger::decode(&ledger, &turn));
    assert!(matches!(result, Err(ResourceError::Capacity)));
    assert_eq!(m.peak, 0);
    let (result, m) = measured(&credit, || HeldPublicationPool::decode(&pool, 1024, &turn));
    assert!(matches!(result, Err(ResourceError::Capacity)));
    assert_eq!(m.peak, 0);
    drop(occupied);
}
fn error_key(error: CommitError) -> (&'static str, &'static str) {
    match error {
        CommitError::Invalid(s) => ("invalid", s),
        CommitError::Quota => ("quota", ""),
        CommitError::Codec => ("codec", ""),
        CommitError::MissingPayload => ("missing", ""),
    }
}
fn resource_error_key(error: ResourceError) -> (&'static str, &'static str) {
    match error {
        ResourceError::Commit(e) => error_key(e),
        ResourceError::Capacity => ("capacity", ""),
        ResourceError::Unaccounted => ("unaccounted", ""),
    }
}
#[test]
fn commit_resources_preserves_legacy_canonical_and_error_semantics() {
    let (credit, turn) = work(16 << 20);
    let ledger = populated();
    let canonical = ledger.encode().unwrap();
    let mut cases = vec![canonical.clone(), canonical[..canonical.len() - 1].to_vec()];
    let mut unknown = canonical.clone();
    unknown[0] = 2;
    cases.push(unknown);
    let mut trailing = canonical.clone();
    trailing.push(0);
    cases.push(trailing);
    // Store generation is fixed sixteen bytes; a nonminimal high-water varint is rejected.
    let mut nonminimal = canonical.clone();
    nonminimal.splice(17..18, [0x98, 0]);
    cases.push(nonminimal);
    for bytes in cases {
        let old = DeliveryLedgerV1::decode(&bytes);
        let (new, _) = measured(&credit, || HeldDeliveryLedger::decode(&bytes, &turn));
        match (old, new) {
            (Ok(a), Ok(b)) => assert_eq!(&a, b.ledger()),
            (Err(a), Err(b)) => assert_eq!(error_key(a), resource_error_key(b)),
            _ => panic!("ledger compatibility changed"),
        }
    }
    let mut pool = PublicationPool::new(1024).unwrap();
    pool.intern(b"literal payload").unwrap();
    let canonical = pool.encode(2048).unwrap();
    let mut duplicate = vec![1, 2];
    duplicate.extend_from_slice(&canonical[2..]);
    duplicate.extend_from_slice(&canonical[2..]);
    let mut corrupt = canonical.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    let mut unknown = canonical.clone();
    unknown[0] = 2;
    let mut trailing = canonical.clone();
    trailing.push(0);
    for bytes in [canonical, duplicate, corrupt, unknown, trailing] {
        let old = PublicationPool::decode(&bytes, 1024);
        let (new, _) = measured(&credit, || HeldPublicationPool::decode(&bytes, 1024, &turn));
        match (old, new) {
            (Ok(a), Ok(b)) => assert_eq!(a.encode(2048).unwrap(), b.pool().encode(2048).unwrap()),
            (Err(a), Err(b)) => assert_eq!(error_key(a), resource_error_key(b)),
            _ => panic!("pool compatibility changed"),
        }
    }
}

#[test]
fn commit_resources_transferred_custody_forks_actual_state_and_denies_legacy_backing() {
    let expected = populated();
    let bytes = expected.encode().unwrap();
    let (credit, turn) = work(8 << 20);
    let held = HeldDeliveryLedger::decode(&bytes, &turn).unwrap();
    let (ledger, custody) = held.into_parts();
    let (fork, _) = measured(&credit, || custody.fork(&ledger, &turn));
    assert_eq!(fork.unwrap().ledger(), &expected);
    let mut pool = PublicationPool::new(1024).unwrap();
    let id = pool.intern(b"payload").unwrap();
    let held = HeldPublicationPool::decode(&pool.encode(2048).unwrap(), 1024, &turn).unwrap();
    let (actual, map_custody) = held.into_parts();
    let (fork, _) = measured(&credit, || map_custody.fork(&actual, &turn));
    let fork = fork.unwrap();
    assert_eq!(
        fork.pool().resolve(&id).unwrap().as_ptr(),
        actual.resolve(&id).unwrap().as_ptr()
    );
    let (legacy, m) = measured(&credit, || map_custody.fork(&pool, &turn));
    assert!(matches!(legacy, Err(ResourceError::Unaccounted)));
    assert_eq!(m.peak, 0);
    // The actual Cloud holder uses this same State-before-custody order.
    drop(ledger);
    drop(custody);
    drop(actual);
    drop(map_custody);
}
#[test]
fn commit_resources_partial_pool_decode_failure_drops_backings_before_credit() {
    let mut pool = PublicationPool::new(1 << 20).unwrap();
    pool.intern(&vec![1; 64 * 1024]).unwrap();
    pool.intern(&vec![2; 64 * 1024]).unwrap();
    let bytes = pool.encode(1 << 20).unwrap();
    let (credit, turn) = work(100 * 1024);
    let baseline = credit.retained_bytes();
    let (result, m) = measured(&credit, || {
        HeldPublicationPool::decode(&bytes, 1 << 20, &turn)
    });
    assert!(matches!(result, Err(ResourceError::Capacity)));
    assert!(
        m.peak > 64 * 1024,
        "failure must happen after a real backing allocation"
    );
    assert_eq!(m.live, 0);
    assert_eq!(credit.retained_bytes(), baseline);
}

#[test]
fn commit_resources_literal_empty_and_maximum_vector_remain_accepted() {
    let literal = [
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0,
    ];
    let (credit, turn) = work(16 << 20);
    let (held, m) = measured(&credit, || HeldDeliveryLedger::decode(&literal, &turn));
    let held = held.unwrap();
    assert_eq!(held.ledger().encode().unwrap(), literal);
    assert_eq!(m.peak, 0);
    let literal_pool = [1, 0];
    let (held, m) = measured(&credit, || {
        HeldPublicationPool::decode(&literal_pool, 1, &turn)
    });
    let held = held.unwrap();
    assert_eq!(held.pool().encode(2).unwrap(), literal_pool);
    assert_eq!(m.peak, 0);
    let mut ledger = DeliveryLedgerV1::decode(&literal).unwrap();
    ledger.resolved_through = u64::MAX;
    ledger.replay.insert(
        u64::MAX,
        ReplayCommit {
            owner_generation: u64::MAX,
            previous: None,
            digest: [2; 32],
            publications: vec![PublicationRef {
                payload: [3; 32],
                kind: Retention::Delta,
            }],
            admitted_operations: vec![[4; 32]; MAX_REFERENCES],
            committed_at_ns: u64::MAX,
        },
    );
    let bytes = ledger.encode().unwrap();
    let (held, _) = measured(&credit, || HeldDeliveryLedger::decode(&bytes, &turn));
    assert_eq!(held.unwrap().ledger(), &ledger);
    ledger
        .replay
        .get_mut(&u64::MAX)
        .unwrap()
        .admitted_operations
        .push([4; 32]);
    let invalid = postcard::to_allocvec(&ledger).unwrap();
    let (result, m) = measured(&credit, || HeldDeliveryLedger::decode(&invalid, &turn));
    assert!(matches!(
        result,
        Err(ResourceError::Commit(CommitError::Codec))
    ));
    assert_eq!(m.peak, 0);
}

#[test]
fn commit_resources_concurrent_last_pins_release_original_credit_once() {
    let mut pool = PublicationPool::new(1024).unwrap();
    let id = pool.intern(b"shared across threads").unwrap();
    let bytes = pool.encode(2048).unwrap();
    let (credit, turn) = work(64 * 1024);
    let held = HeldPublicationPool::decode(&bytes, 1024, &turn).unwrap();
    let a = held.pool().pin(&id).unwrap();
    let b = held.pool().pin(&id).unwrap();
    let backing = held.pool().retained_allocation_bytes(&id).unwrap();
    drop(held);
    drop(turn);
    credit.finish_turn();
    let before = credit.retained_bytes();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let other = barrier.clone();
    let thread = std::thread::spawn(move || {
        other.wait();
        assert_eq!(a.bytes(), b"shared across threads");
        drop(a);
    });
    barrier.wait();
    assert_eq!(b.bytes(), b"shared across threads");
    drop(b);
    thread.join().unwrap();
    assert_eq!(credit.retained_bytes(), before - backing);
}

#[test]
fn commit_resources_peer_budget_uses_original_ledger_for_every_concrete_entry() {
    let ledger = populated();
    let encoded = ledger.encode().unwrap();
    let mut legacy = PublicationPool::new(1 << 20).unwrap();
    let id = legacy.intern(&vec![19; 64 * 1024]).unwrap();
    let bytes = legacy.encode(1 << 20).unwrap();
    let credit = NativeAllowance::new(8 << 20).unwrap();
    let mut registry = qos_peer_state::PeerRegistry::new(credit.clone()).unwrap();
    let mut turn = registry
        .begin_turn(credit.clone(), qos_peer_state::PeerLimits::default())
        .unwrap();
    let work = turn.work().unwrap();
    let budget = ResourceBudget::from_peer(&work);
    let baseline = credit.retained_bytes();
    let (held, m) = measured(&credit, || HeldDeliveryLedger::decode_in(&encoded, &budget));
    let held = held.unwrap();
    assert!(m.peak > 0);
    assert_eq!(held.ledger(), &ledger);
    assert!(credit.retained_bytes() > baseline);
    let (fork, m) = measured(&credit, || held.fork_in(&budget));
    assert!(m.peak > 0);
    assert_eq!(fork.unwrap().ledger(), &ledger);
    let parts = held.into_parts();
    let (fork, m) = measured(&credit, || parts.1.fork_in(&parts.0, &budget));
    assert!(m.peak > 0);
    assert_eq!(fork.unwrap().ledger(), &ledger);
    drop(parts);
    let (held, m) = measured(&credit, || {
        HeldPublicationPool::decode_in(&bytes, 1 << 20, &budget)
    });
    let held = held.unwrap();
    assert!(m.peak > 64 * 1024);
    let pointer = held.pool().resolve(&id).unwrap().as_ptr();
    let (fork, m) = measured(&credit, || held.fork_in(&budget));
    let fork = fork.unwrap();
    assert!(m.peak < 8192);
    assert_eq!(fork.pool().resolve(&id).unwrap().as_ptr(), pointer);
    drop(fork);
    let parts = held.into_parts();
    let (fork, m) = measured(&credit, || parts.1.fork_in(&parts.0, &budget));
    let fork = fork.unwrap();
    assert!(m.peak < 8192);
    assert_eq!(fork.pool().resolve(&id).unwrap().as_ptr(), pointer);
    let (denied, m) = measured(&credit, || parts.1.fork_in(&legacy, &budget));
    assert!(matches!(denied, Err(ResourceError::Unaccounted)));
    assert_eq!(m.peak, 0);
    drop(fork);
    drop(parts);
    assert_eq!(credit.retained_bytes(), baseline);
}

#[test]
fn commit_resources_peer_budget_exhaustion_has_no_second_counter_or_heap() {
    let encoded = populated().encode().unwrap();
    let mut legacy = PublicationPool::new(1024).unwrap();
    legacy.intern(b"original payload").unwrap();
    let bytes = legacy.encode(2048).unwrap();
    let credit = NativeAllowance::new(8 << 20).unwrap();
    let mut registry = qos_peer_state::PeerRegistry::new(credit.clone()).unwrap();
    let mut turn = registry
        .begin_turn(credit.clone(), qos_peer_state::PeerLimits::default())
        .unwrap();
    let work = turn.work().unwrap();
    let budget = ResourceBudget::from_peer(&work);
    let ledger = HeldDeliveryLedger::decode_in(&encoded, &budget).unwrap();
    let pool = HeldPublicationPool::decode_in(&bytes, 1024, &budget).unwrap();
    // Occupy the exact same ledger through the real PeerWork API; no adapter
    // counter can replenish it or fund even a fork after this reservation.
    let occupied = work.reserve_output(credit.available_bytes()).unwrap();
    assert_eq!(credit.available_bytes(), 0);
    let (a, m) = measured(&credit, || HeldDeliveryLedger::decode_in(&encoded, &budget));
    assert!(matches!(a, Err(ResourceError::Capacity)));
    assert_eq!(m.peak, 0);
    let (a, m) = measured(&credit, || {
        HeldPublicationPool::decode_in(&bytes, 1024, &budget)
    });
    assert!(matches!(a, Err(ResourceError::Capacity)));
    assert_eq!(m.peak, 0);
    let (a, m) = measured(&credit, || ledger.fork_in(&budget));
    assert!(matches!(a, Err(ResourceError::Capacity)));
    assert_eq!(m.peak, 0);
    let (a, m) = measured(&credit, || pool.fork_in(&budget));
    assert!(matches!(a, Err(ResourceError::Capacity)));
    assert_eq!(m.peak, 0);
    let ledger = ledger.into_parts();
    let pool = pool.into_parts();
    let (a, m) = measured(&credit, || ledger.1.fork_in(&ledger.0, &budget));
    assert!(matches!(a, Err(ResourceError::Capacity)));
    assert_eq!(m.peak, 0);
    let (a, m) = measured(&credit, || pool.1.fork_in(&pool.0, &budget));
    assert!(matches!(a, Err(ResourceError::Capacity)));
    assert_eq!(m.peak, 0);
    drop(occupied);
    let extra = work.reserve_output(1).unwrap();
    drop(extra);
}

#[test]
fn commit_resources_peer_budget_pin_keeps_original_credit_after_operation_seals() {
    let mut legacy = PublicationPool::new(1 << 20).unwrap();
    let id = legacy.intern(&vec![91; 64 * 1024]).unwrap();
    let bytes = legacy.encode(1 << 20).unwrap();
    let credit = NativeAllowance::new(2 << 20).unwrap();
    let mut registry = qos_peer_state::PeerRegistry::new(credit.clone()).unwrap();
    let mut turn = registry
        .begin_turn(credit.clone(), qos_peer_state::PeerLimits::default())
        .unwrap();
    let (pin, backing, pointer) = {
        let work = turn.work().unwrap();
        let budget = ResourceBudget::from_peer(&work);
        let held = HeldPublicationPool::decode_in(&bytes, 1 << 20, &budget).unwrap();
        let pointer = held.pool().resolve(&id).unwrap().as_ptr();
        let backing = held.pool().retained_allocation_bytes(&id).unwrap();
        let fork = held.fork_in(&budget).unwrap();
        let pin = fork.pool().pin(&id).unwrap();
        drop(held);
        drop(fork);
        (pin, backing, pointer)
    };
    // The immutable budget/work borrow ended; the same PeerTurn remains usable.
    let next_work = turn.work().unwrap();
    let extra = next_work.reserve_output(7).unwrap();
    drop(extra);
    drop(next_work);
    drop(turn);
    assert_eq!(credit.available_bytes(), 0);
    let retained = credit.retained_bytes();
    assert_eq!(pin.bytes().as_ptr(), pointer);
    assert_eq!(pin.bytes().len(), 64 * 1024);
    drop(pin);
    assert_eq!(credit.retained_bytes(), retained - backing);
}
