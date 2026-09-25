use super::*;

pub(super) mod allocations;

// A lazy keeper mutex must fail this actual callback observation on macOS.
#[test]
fn native_keeper_callbacks_allocate_nothing_after_admission() {
    let (ingress, _receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let initial = pool.available_bytes();
    let allowance = pool.native_allowance().unwrap();
    let reservation = allowance.reserve(128).unwrap();
    let (_, observed) = allocations::allocations_only(|| {
        for _ in 0..32 {
            allowance.finish_turn();
        }
        drop(reservation);
        drop(allowance);
    });
    assert_eq!(observed.allocations, 0, "callback allocated: {observed:?}");
    assert_eq!(pool.available_bytes(), initial);
}

// Omitting any concrete keeper allocation from HostBytes retention must fail,
// even if all final permits are eventually refunded and coarse ranges pass.
#[test]
fn native_keeper_all_retained_allocations_remain_funded() {
    let (ingress, _receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    // The shared pool's own admission lock is unrelated to keeper construction.
    drop(pool.try_acquire_bulk(1).unwrap());
    let initial = pool.available_bytes();
    let ((retained, withheld), observed) = allocations::measure(&pool, || {
        let allowance = pool.native_allowance().unwrap();
        allowance.finish_turn();
        let retained = allocations::snapshot().live;
        let withheld = initial - pool.available_bytes();
        drop(allowance);
        (retained, withheld)
    });
    assert!(
        !observed.underfunded,
        "unfunded actual backing: {observed:?}"
    );
    assert!(
        !observed.unmatched_free,
        "measurement omitted an allocation"
    );
    assert_eq!(
        retained, withheld,
        "retained measured backing needs real credit"
    );
    assert_eq!(observed.live, 0, "all observed backing must be freed");
    assert_eq!(pool.available_bytes(), initial);
}

// Shared real allowance callbacks must not initialize backing on worker threads.
#[test]
fn native_keeper_concurrent_callbacks_allocate_nothing() {
    let (ingress, _receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let initial = pool.available_bytes();
    let allowance = pool.native_allowance().unwrap();
    let start = std::sync::Barrier::new(8);
    let observed = std::thread::scope(|scope| {
        let workers = (0..8)
            .map(|_| {
                let allowance = allowance.clone();
                let reservation = allowance.reserve(128).unwrap();
                let start = &start;
                scope.spawn(move || {
                    start.wait();
                    allocations::allocations_only(|| {
                        for _ in 0..32 {
                            allowance.finish_turn();
                        }
                        drop(reservation);
                        drop(allowance);
                    })
                    .1
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(
        observed.iter().all(|stats| stats.allocations == 0),
        "shared callbacks allocated: {observed:?}"
    );
    drop(allowance);
    assert_eq!(pool.available_bytes(), initial);
}

// Refusing insufficient actual credit must happen before either keeper allocation.
#[test]
fn native_keeper_short_credit_refuses_before_allocation() {
    let (ingress, _receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let initial = pool.available_bytes();
    let credit = pool.try_acquire_bulk(1).unwrap();
    let (result, observed) = allocations::allocations_only(|| HostNativeCredit::new(credit));
    assert!(matches!(result, Err(NativeResourceError::WorkBytes)));
    assert_eq!(observed.allocations, 0);
    assert_eq!(pool.available_bytes(), initial);
}

// Stale/overflow notices must neither grow credit nor return still-live backing.
#[test]
fn native_keeper_stale_and_overflow_shrink_keep_backing_funded() {
    let (ingress, _receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    drop(pool.try_acquire_bulk(1).unwrap());
    let initial = pool.available_bytes();
    let ((first, stale, overflow, final_held, backing), observed) =
        allocations::measure(&pool, || {
            let keeper = HostNativeCredit::new(pool.try_acquire_bulk(4096).unwrap()).unwrap();
            let backing = allocations::snapshot().live;
            keeper.shrink_to(128);
            let first = initial - pool.available_bytes();
            keeper.shrink_to(256);
            let stale = initial - pool.available_bytes();
            keeper.shrink_to(usize::MAX);
            let overflow = initial - pool.available_bytes();
            keeper.shrink_to(0);
            let final_held = initial - pool.available_bytes();
            allocations::snapshot();
            keeper.release();
            (first, stale, overflow, final_held, backing)
        });
    assert_eq!(first, backing + 128);
    assert_eq!(stale, first);
    assert_eq!(overflow, first);
    assert_eq!(final_held, backing);
    assert!(
        !observed.underfunded,
        "backing returned early: {observed:?}"
    );
    assert!(!observed.unmatched_free);
    assert_eq!(observed.live, 0);
    assert_eq!(pool.available_bytes(), initial);
}

// Fails if construction cannot bind the real ingress credit to native lifetime.
#[test]
fn actual_ingress_credit_stays_reserved_until_native_allowance_is_finished() {
    let (ingress, _receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let initial = pool.available_bytes();
    let allowance =
        host_allowance(&pool).expect("real ingress credit must bind to native allowance");
    assert_eq!(initial - pool.available_bytes(), 64 * 1024 * 1024);
    allowance.finish_turn();
    assert!(initial - pool.available_bytes() < 1024);
    assert!(initial - pool.available_bytes() > 0);
    drop(allowance);
    assert_eq!(pool.available_bytes(), initial);
}

// Real allocated backing, with destruction ordered before its actual native
// reservation. This exercises embedding custody, not native qualification.
struct Backing {
    bytes: Vec<u8>,
    _reservation: ui::automerge::resource_limits::NativeReservation,
}
impl Backing {
    fn new(turn: &mut OwnerNativeTurn<'_>, bytes: usize) -> Self {
        let reservation = turn.native().1.reserve(bytes).unwrap();
        Self {
            bytes: vec![0x5a; bytes],
            _reservation: reservation,
        }
    }
}

// Fails if finishing returns retained backing credit, or allows new reservation.
#[test]
fn sealed_allowance_keeps_reservations_and_releases_each_drop() {
    let (ingress, _receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let initial = pool.available_bytes();
    let allowance = host_allowance(&pool).unwrap();
    let escaped = allowance.clone();
    let first = allowance.reserve(4096).unwrap();
    let second = allowance.reserve(8192).unwrap();
    allowance.finish_turn();
    let held = initial - pool.available_bytes();
    assert!(held >= 12_288 && held < 13_312);
    assert!(matches!(
        escaped.reserve(1),
        Err(NativeResourceError::WorkBytes)
    ));
    allowance.finish_turn();
    assert_eq!(initial - pool.available_bytes(), held);
    drop(first);
    assert_eq!(initial - pool.available_bytes(), held - 4096);
    drop(allowance);
    drop(escaped);
    assert!(pool.available_bytes() < initial);
    drop(second);
    assert_eq!(pool.available_bytes(), initial);
}

// Fails on an accidental owner-wide64MiB cap, or early old-backing credit return.
#[test]
fn successive_turns_retain_more_than_one_turn_limit_on_the_same_pool() {
    let (ingress, _receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let initial = pool.available_bytes();
    let mut budget = OwnerNativeBudget::new(&pool).unwrap();
    let baseline = initial - pool.available_bytes();
    assert!(baseline > 0 && baseline < 1024);
    let mut first_turn = budget.prepare_turn(&pool).unwrap();
    assert_eq!(
        initial - pool.available_bytes(),
        baseline + 64 * 1024 * 1024
    );
    let first = Backing::new(&mut first_turn, 40 * 1024 * 1024);
    drop(first_turn);
    let retained_first = initial - pool.available_bytes();
    assert!(retained_first >= 40 * 1024 * 1024);
    assert!(retained_first < 41 * 1024 * 1024);
    let mut second_turn = budget.prepare_turn(&pool).unwrap();
    assert_eq!(
        initial - pool.available_bytes(),
        retained_first + 64 * 1024 * 1024
    );
    let second = Backing::new(&mut second_turn, 40 * 1024 * 1024);
    drop(second_turn);
    let retained_both = initial - pool.available_bytes();
    assert!(retained_both >= 80 * 1024 * 1024);
    assert!(retained_both < 81 * 1024 * 1024);
    assert_eq!((first.bytes[0], second.bytes[0]), (0x5a, 0x5a));
    drop(budget);
    assert!(initial - pool.available_bytes() >= 80 * 1024 * 1024);
    drop(first);
    assert!(initial - pool.available_bytes() >= 40 * 1024 * 1024);
    assert!(initial - pool.available_bytes() < 41 * 1024 * 1024);
    drop(second);
    assert_eq!(pool.available_bytes(), initial);
}

// Fails if cancellation doesn't seal aliases or returns an escaped allocation's credit.
#[test]
fn cancellation_seals_aliases_and_retains_real_backing() {
    let (ingress, _receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let initial = pool.available_bytes();
    let mut budget = OwnerNativeBudget::new(&pool).unwrap();
    let mut backing = None;
    let mut alias = None;
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut turn = budget.prepare_turn(&pool).unwrap();
        backing = Some(Backing::new(&mut turn, 32 * 1024));
        alias = Some(turn.allowance().clone());
        panic!("cancel actual native turn");
    }));
    assert!(panic.is_err());
    assert!(matches!(
        alias.as_ref().unwrap().reserve(1),
        Err(NativeResourceError::WorkBytes)
    ));
    assert!(initial - pool.available_bytes() >= 32 * 1024);
    assert!(initial - pool.available_bytes() < 33 * 1024);
    drop(budget);
    drop(alias);
    assert_eq!(backing.as_ref().unwrap().bytes[0], 0x5a);
    drop(backing);
    assert_eq!(pool.available_bytes(), initial);
}

// Fails if a new pool is substituted or bulk work consumes the control floor.
#[test]
fn pool_mismatch_and_bulk_pressure_refuse_without_leaking() {
    let (ingress, _receiver) = crate::https::ingress_channel();
    let (other, _other_receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let foreign = other.host_bytes();
    let mut budget = OwnerNativeBudget::new(&pool).unwrap();
    let baseline = pool.available_bytes();
    let foreign_baseline = foreign.available_bytes();
    assert!(matches!(
        budget.prepare_turn(&foreign),
        Err(NativeResourceError::Unaccounted)
    ));
    assert_eq!(pool.available_bytes(), baseline);
    assert_eq!(foreign.available_bytes(), foreign_baseline);
    let pressure = pool
        .try_acquire_bulk(baseline - crate::host_bytes::CONTROL_RESERVE_BYTES)
        .unwrap();
    assert!(matches!(
        budget.prepare_turn(&pool),
        Err(NativeResourceError::WorkBytes)
    ));
    assert_eq!(
        pool.available_bytes(),
        crate::host_bytes::CONTROL_RESERVE_BYTES
    );
    let control = pool.try_acquire_control(1).unwrap();
    drop(control);
    drop(pressure);
    assert_eq!(pool.available_bytes(), baseline);
    drop(budget.prepare_turn(&pool).unwrap());
    assert_eq!(pool.available_bytes(), baseline);
}

// Actual native clone uses the stable private owner registry and rejects another domain.
#[test]
fn native_clone_uses_same_registry_across_turns_and_rejects_another_owner() {
    use ui::automerge::{AutoCommit, ROOT, transaction::Transactable};
    let (ingress, _receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let initial = pool.available_bytes();
    let mut budget = OwnerNativeBudget::new(&pool).unwrap();
    let mut other = OwnerNativeBudget::new(&pool).unwrap();
    let mut source = AutoCommit::new();
    source.put(ROOT, "value", "real bounded clone").unwrap();
    let mut first_turn = budget.prepare_turn(&pool).unwrap();
    let (registry, work) = first_turn.native();
    let first = source.clone_bounded(registry, work).unwrap();
    drop(first_turn);
    let mut next_turn = budget.prepare_turn(&pool).unwrap();
    let (registry, work) = next_turn.native();
    let second = first.clone_bounded(registry, work).unwrap();
    drop(next_turn);
    let mut foreign_turn = other.prepare_turn(&pool).unwrap();
    let (registry, work) = foreign_turn.native();
    assert!(matches!(
        first.clone_bounded(registry, work),
        Err(NativeResourceError::Unaccounted)
    ));
    drop(foreign_turn);
    drop(budget);
    drop(other);
    assert!(pool.available_bytes() < initial);
    drop(first);
    drop(second);
    assert_eq!(pool.available_bytes(), initial);
    // source is deliberately unqualified; clone custody is not readiness.
    assert_eq!(source.pending_ops(), 1);
}

struct ReleaseObservation {
    pool: HostBytes,
    initial: usize,
    observed: bool,
}
thread_local! {
    static RELEASE: std::cell::RefCell<Option<ReleaseObservation>> = const { std::cell::RefCell::new(None) };
}
struct ObserveRelease;
impl Drop for ObserveRelease {
    fn drop(&mut self) {
        RELEASE.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}
// Called by the actual keeper only after Box::drop returned. No replacement keeper.
pub(super) fn after_keeper_deallocation(credit: Option<&ByteCredit>) {
    RELEASE.with(|slot| {
        if let Some(observation) = slot.borrow_mut().as_mut() {
            let credit = credit.expect("final real keeper must still own its credit");
            assert_eq!(
                observation.initial - observation.pool.available_bytes(),
                credit.bytes()
            );
            assert!(credit.bytes() > 0);
            observation.observed = true;
        }
    });
}

// Fails if the final real permits are returned before the concrete Box is freed.
#[test]
fn final_keeper_deallocates_before_returning_actual_credit() {
    let (ingress, _receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let initial = pool.available_bytes();
    let allowance = host_allowance(&pool).unwrap();
    let reservation = allowance.reserve(128).unwrap();
    allowance.finish_turn();
    drop(allowance);
    RELEASE.with(|slot| {
        *slot.borrow_mut() = Some(ReleaseObservation {
            pool: pool.clone(),
            initial,
            observed: false,
        })
    });
    let _scope = ObserveRelease;
    drop(reservation);
    assert!(RELEASE.with(|slot| slot.borrow().as_ref().unwrap().observed));
    assert_eq!(pool.available_bytes(), initial);
}

// Invalid native construction still consumes/releases the real host keeper correctly.
#[test]
fn invalid_native_constructor_returns_real_credit_after_keeper_deallocation() {
    let (ingress, _receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let initial = pool.available_bytes();
    let credit = pool.try_acquire_bulk(4096).unwrap();
    RELEASE.with(|slot| {
        *slot.borrow_mut() = Some(ReleaseObservation {
            pool: pool.clone(),
            initial,
            observed: false,
        })
    });
    let _scope = ObserveRelease;
    assert!(NativeAllowance::with_host_credit(0, HostNativeCredit::new(credit).unwrap()).is_err());
    assert!(RELEASE.with(|slot| slot.borrow().as_ref().unwrap().observed));
    assert_eq!(pool.available_bytes(), initial);
}

// Test utility for the embedding owner's cancellation duty. No alternative
// keeper, pool, registry or qualification token is introduced.
struct SealPublicAllowance(NativeAllowance);
impl Drop for SealPublicAllowance {
    fn drop(&mut self) {
        self.0.finish_turn();
    }
}
struct PeerOutputBacking {
    bytes: Vec<u8>,
    _reservation: ui::automerge::resource_limits::NativeReservation,
}
fn peer_output(work: &qos_peer_state::PeerWork<'_>, bytes: usize) -> PeerOutputBacking {
    let reservation = work.reserve_output(bytes).unwrap();
    PeerOutputBacking {
        bytes: vec![0x63; bytes],
        _reservation: reservation,
    }
}
fn peer_registry(pool: &HostBytes) -> qos_peer_state::PeerRegistry {
    let initial = pool.native_allowance().unwrap();
    let guard = SealPublicAllowance(initial.clone());
    let registry = qos_peer_state::PeerRegistry::new(initial).unwrap();
    drop(guard);
    registry
}

#[test]
fn public_native_allowance_reserves_exact_original_pool_and_releases_after_finish() {
    let (ingress, _receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let before = pool.available_bytes();
    let allowance = pool
        .native_allowance()
        .expect("public seam must use actual keeper");
    assert_eq!(before - pool.available_bytes(), NATIVE_TURN_BYTES);
    let guard = SealPublicAllowance(allowance.clone());
    let reservation = allowance.reserve(1024).unwrap();
    drop(guard);
    assert!(matches!(
        allowance.reserve(1),
        Err(NativeResourceError::WorkBytes)
    ));
    assert!(before - pool.available_bytes() >= 1024);
    assert!(before - pool.available_bytes() < 2048);
    drop(allowance);
    assert!(pool.available_bytes() < before);
    drop(reservation);
    assert_eq!(pool.available_bytes(), before);
}

#[test]
fn public_native_allowance_peer_turns_keep_real_backings_above_one_turn_budget() {
    let (ingress, _receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let before = pool.available_bytes();
    let mut registry = peer_registry(&pool);
    let startup = before - pool.available_bytes();
    assert!(startup > 0 && startup < 1024);
    let first = {
        let current = pool.native_allowance().unwrap();
        let _cancel = SealPublicAllowance(current.clone());
        assert_eq!(before - pool.available_bytes(), startup + NATIVE_TURN_BYTES);
        let mut turn = registry
            .begin_turn(current, qos_peer_state::PeerLimits::default())
            .unwrap();
        let work = turn.work().unwrap();
        peer_output(&work, 40 * 1024 * 1024)
    };
    let retained_first = before - pool.available_bytes();
    assert!(retained_first >= 40 * 1024 * 1024 && retained_first < 41 * 1024 * 1024);
    let second = {
        let current = pool.native_allowance().unwrap();
        let _cancel = SealPublicAllowance(current.clone());
        assert_eq!(
            before - pool.available_bytes(),
            retained_first + NATIVE_TURN_BYTES
        );
        let mut turn = registry
            .begin_turn(current, qos_peer_state::PeerLimits::default())
            .unwrap();
        let work = turn.work().unwrap();
        peer_output(&work, 40 * 1024 * 1024)
    };
    assert!(before - pool.available_bytes() >= 80 * 1024 * 1024);
    assert!(before - pool.available_bytes() < 81 * 1024 * 1024);
    assert_eq!((first.bytes[0], second.bytes[0]), (0x63, 0x63));
    drop(registry);
    assert!(before - pool.available_bytes() >= 80 * 1024 * 1024);
    drop(first);
    assert!(before - pool.available_bytes() >= 40 * 1024 * 1024);
    drop(second);
    assert_eq!(pool.available_bytes(), before);
}

#[test]
fn public_native_allowance_seals_actual_registry_and_begin_turn_failures() {
    let (ingress, _receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let before = pool.available_bytes();
    let initial = pool.native_allowance().unwrap();
    let alias = initial.clone();
    let cancel = SealPublicAllowance(initial.clone());
    let count = initial.available_bytes();
    let reservation = initial.reserve(count).unwrap();
    let backing = PeerOutputBacking {
        bytes: vec![0x31; count],
        _reservation: reservation,
    };
    assert!(matches!(
        qos_peer_state::PeerRegistry::new(initial),
        Err(qos_peer_state::BoundedPeerError::Resource(
            NativeResourceError::WorkBytes
        ))
    ));
    drop(cancel);
    assert!(matches!(
        alias.reserve(1),
        Err(NativeResourceError::WorkBytes)
    ));
    drop(alias);
    assert_eq!(backing.bytes[0], 0x31);
    assert!(pool.available_bytes() < before);
    drop(backing);
    assert_eq!(pool.available_bytes(), before);

    let mut registry = peer_registry(&pool);
    let startup = pool.available_bytes();
    let current = pool.native_allowance().unwrap();
    let alias = current.clone();
    let cancel = SealPublicAllowance(current.clone());
    let limits = qos_peer_state::PeerLimits {
        normalized_bytes: 8 * 1024 * 1024 + 1,
        ..Default::default()
    };
    assert!(matches!(
        registry.begin_turn(current, limits),
        Err(qos_peer_state::BoundedPeerError::Resource(
            NativeResourceError::NormalizedBytes
        ))
    ));
    drop(cancel);
    assert!(matches!(
        alias.reserve(1),
        Err(NativeResourceError::WorkBytes)
    ));
    drop(alias);
    assert_eq!(pool.available_bytes(), startup);
    drop(registry);
    assert_eq!(pool.available_bytes(), before);
}

#[test]
fn public_native_allowance_qps_cancellation_keeps_escaped_real_output() {
    let (ingress, _receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let before = pool.available_bytes();
    let mut registry = peer_registry(&pool);
    let mut backing = None;
    let mut alias = None;
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let current = pool.native_allowance().unwrap();
        alias = Some(current.clone());
        let _cancel = SealPublicAllowance(current.clone());
        let mut turn = registry
            .begin_turn(current, qos_peer_state::PeerLimits::default())
            .unwrap();
        let work = turn.work().unwrap();
        backing = Some(peer_output(&work, 32 * 1024));
        panic!("cancel genuine qps operation");
    }));
    assert!(panic.is_err());
    assert!(matches!(
        alias.as_ref().unwrap().reserve(1),
        Err(NativeResourceError::WorkBytes)
    ));
    assert_eq!(backing.as_ref().unwrap().bytes[0], 0x63);
    drop(registry);
    drop(alias);
    assert!(before - pool.available_bytes() >= 32 * 1024);
    assert!(before - pool.available_bytes() < 33 * 1024);
    drop(backing);
    assert_eq!(pool.available_bytes(), before);
}

#[test]
fn public_native_allowance_bulk_refusal_preserves_same_pool_control_floor() {
    let (ingress, _receiver) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let before = pool.available_bytes();
    let pressure = pool
        .try_acquire_bulk(before - crate::host_bytes::CONTROL_RESERVE_BYTES)
        .unwrap();
    assert!(matches!(
        pool.native_allowance(),
        Err(NativeResourceError::WorkBytes)
    ));
    assert_eq!(
        pool.available_bytes(),
        crate::host_bytes::CONTROL_RESERVE_BYTES
    );
    let control = pool.try_acquire_control(1).unwrap();
    drop(control);
    drop(pressure);
    assert_eq!(pool.available_bytes(), before);
    let allowance = pool.native_allowance().unwrap();
    allowance.finish_turn();
    drop(allowance);
    assert_eq!(pool.available_bytes(), before);
}
