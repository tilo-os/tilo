use super::*;
use automerge::resource_limits::NativeAllowance;
use std::sync::{MutexGuard, TryLockError, mpsc};
use std::time::Duration;

pub(super) mod allocations;

fn budget() -> NativeAllowance {
    NativeAllowance::new(64 * 1024).unwrap()
}
fn domain(budget: &NativeAllowance) -> RetirementDomain {
    RetirementDomain::new(
        budget
            .reserve(RetirementDomain::allocation_bytes().unwrap())
            .unwrap(),
    )
    .unwrap()
}
fn owner(domain: &RetirementDomain, budget: &NativeAllowance) -> RetirementOwner {
    domain
        .marker(
            budget
                .reserve(RetirementOwner::allocation_bytes().unwrap())
                .unwrap(),
        )
        .unwrap()
}
fn handle(owner: &RetirementOwner, budget: &NativeAllowance) -> RetirementHandle {
    owner
        .handle(
            budget
                .reserve(RetirementHandle::allocation_bytes().unwrap())
                .unwrap(),
        )
        .unwrap()
}

// Fixed private synchronization probe, not a document/owner/positive sink API.
pub(crate) fn probe(handle: &RetirementHandle) -> Result<MutexGuard<'_, ()>, RetirementError> {
    let guard = match handle.value.domain.gate.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::WouldBlock) => return Err(RetirementError::Busy),
        Err(TryLockError::Poisoned(_)) => return Err(RetirementError::Poisoned),
    };
    if handle.value.domain.requested.load(Ordering::Acquire)
        || handle.value.domain.retired.load(Ordering::Acquire)
        || handle.value.marker.requested.load(Ordering::Acquire)
        || handle.value.marker.retired.load(Ordering::Acquire)
    {
        return Err(RetirementError::Retired);
    }
    Ok(guard)
}

#[test]
fn retirement_rejects_short_credit_before_allocation_and_refunds() {
    let budget = budget();
    let baseline = budget.retained_bytes();
    let (result, stats) = allocations::measure(&budget, || {
        RetirementDomain::new(budget.reserve(1).unwrap())
    });
    assert!(matches!(result, Err(RetirementError::CreditTooSmall)));
    assert_eq!(stats.allocations, 0);
    assert_eq!(budget.retained_bytes(), baseline);
}

#[test]
fn retirement_marker_drop_denies_retained_handles_without_retiring_sibling() {
    let budget = budget();
    let domain = domain(&budget);
    let first = owner(&domain, &budget);
    let retained = handle(&first, &budget);
    let sibling = owner(&domain, &budget);
    let sibling_handle = handle(&sibling, &budget);
    drop(probe(&retained).unwrap());
    drop(first);
    assert!(matches!(probe(&retained), Err(RetirementError::Retired)));
    drop(probe(&sibling_handle).unwrap());
    drop(domain);
    assert!(matches!(
        probe(&sibling_handle),
        Err(RetirementError::Retired)
    ));
}

#[test]
fn retirement_negative_handle_and_finish_alone_are_idempotent() {
    let budget = budget();
    let domain = domain(&budget);
    let owner = owner(&domain, &budget);
    let handle = handle(&owner, &budget);
    handle.finish_retirement();
    handle.retire();
    owner.finish_retirement();
    assert!(matches!(probe(&handle), Err(RetirementError::Retired)));
    domain.finish_retirement();
    let result = domain.marker(
        budget
            .reserve(RetirementOwner::allocation_bytes().unwrap())
            .unwrap(),
    );
    assert!(matches!(result, Err(RetirementError::Retired)));
}

#[test]
fn retirement_waits_for_entered_probe_before_finishing() {
    let budget = budget();
    let domain = domain(&budget);
    let owner = owner(&domain, &budget);
    let observer = handle(&owner, &budget);
    let entered = probe(&observer).unwrap();
    let (requested_tx, requested_rx) = mpsc::sync_channel(0);
    let (finished_tx, finished_rx) = mpsc::sync_channel(0);
    let retiring = std::thread::spawn(move || {
        owner.request_retirement();
        requested_tx.send(()).unwrap();
        owner.finish_retirement();
        finished_tx.send(()).unwrap();
    });
    requested_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(observer.value.marker.requested.load(Ordering::Acquire));
    assert!(!observer.value.marker.retired.load(Ordering::Acquire));
    assert!(matches!(
        finished_rx.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    drop(entered);
    finished_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    retiring.join().unwrap();
    assert!(matches!(probe(&observer), Err(RetirementError::Retired)));
}

#[test]
fn retirement_poison_is_permanent_and_cleanup_does_not_panic() {
    let budget = budget();
    let domain = domain(&budget);
    let owner = owner(&domain, &budget);
    let handle = handle(&owner, &budget);
    // Only the fixed mutex probe runs here; no NativeHostCredit callback runs.
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _held = probe(&handle).unwrap();
        panic!("intentional private fixed-probe poison");
    }));
    assert!(poisoned.is_err());
    assert!(matches!(probe(&handle), Err(RetirementError::Poisoned)));
    let result = domain.marker(
        budget
            .reserve(RetirementOwner::allocation_bytes().unwrap())
            .unwrap(),
    );
    assert!(matches!(result, Err(RetirementError::Poisoned)));
    owner.retire();
    domain.retire();
    drop(owner);
    drop(domain);
    assert!(matches!(probe(&handle), Err(RetirementError::Poisoned)));
}

#[test]
fn retirement_all_allocations_and_last_reference_deallocation_remain_paid() {
    let budget = budget();
    let baseline = budget.retained_bytes();
    let ((), stats) = allocations::measure(&budget, || {
        let domain = domain(&budget);
        let owner = owner(&domain, &budget);
        let first = handle(&owner, &budget);
        let last = first
            .copy_with_credit(
                budget
                    .reserve(RetirementHandle::allocation_bytes().unwrap())
                    .unwrap(),
            )
            .unwrap();
        drop(owner);
        drop(first);
        drop(domain);
        // Only a funded observer remains; it keeps backing, never logical life.
        assert!(matches!(probe(&last), Err(RetirementError::Retired)));
        drop(last);
    });
    assert!(stats.allocations >= 2);
    assert_eq!(stats.live, 0);
    assert!(!stats.underfunded, "{stats:?}");
    assert_eq!(budget.retained_bytes(), baseline);
}

#[test]
fn retirement_and_drop_allocate_nothing_after_eager_mutex_initialization() {
    let budget = budget();
    let (domain, owner, handle) = {
        let domain = domain(&budget);
        let owner = owner(&domain, &budget);
        let handle = handle(&owner, &budget);
        (domain, owner, handle)
    };
    let (_, stats) = allocations::allocations_only(|| {
        handle.request_retirement();
        handle.finish_retirement();
        drop(owner);
        drop(handle);
        drop(domain);
    });
    assert_eq!(stats.allocations, 0);
}

#[test]
fn retirement_credit_exhaustion_sealing_and_failed_copy_preserve_original() {
    let budget = budget();
    let domain = domain(&budget);
    let owner = owner(&domain, &budget);
    let handle = handle(&owner, &budget);
    let baseline = budget.retained_bytes();
    let too_small = handle.copy_with_credit(budget.reserve(1).unwrap());
    assert!(matches!(too_small, Err(RetirementError::CreditTooSmall)));
    assert_eq!(budget.retained_bytes(), baseline);
    drop(probe(&handle).unwrap());
    let all = budget.reserve(budget.available_bytes()).unwrap();
    assert!(
        budget
            .reserve(RetirementHandle::allocation_bytes().unwrap())
            .is_err()
    );
    drop(all);
    budget.finish_turn();
    assert!(
        budget
            .reserve(RetirementHandle::allocation_bytes().unwrap())
            .is_err()
    );
    // Existing paid backing and negative cleanup remain valid after sealing.
    handle.retire();
    drop(handle);
    drop(owner);
    drop(domain);
    assert_eq!(
        budget.retained_bytes(),
        NativeAllowance::new(64 * 1024).unwrap().retained_bytes()
    );
}

// Fixed negative observation for actual SDK lifecycle synchronization tests.
// No guard, positive authority, or callback is exposed outside cfg(test).
pub(crate) fn retirement_requested(handle: &RetirementHandle) -> bool {
    handle.value.marker.requested.load(Ordering::Acquire)
}
