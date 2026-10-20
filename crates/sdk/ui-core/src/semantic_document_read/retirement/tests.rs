use super::*;
use std::sync::{MutexGuard, TryLockError, mpsc};
use std::time::Duration;

pub(crate) mod allocations;

fn domain() -> RetirementDomain {
    RetirementDomain::new().unwrap()
}
fn owner(domain: &RetirementDomain) -> RetirementOwner {
    domain.marker().unwrap()
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
fn retirement_marker_drop_denies_retained_handles_without_retiring_sibling() {
    let domain = domain();
    let first = owner(&domain);
    let retained = first.handle();
    let sibling = owner(&domain);
    let sibling_handle = sibling.handle();
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
    let domain = domain();
    let owner = owner(&domain);
    let handle = owner.handle();
    handle.finish_retirement();
    handle.retire();
    owner.finish_retirement();
    assert!(matches!(probe(&handle), Err(RetirementError::Retired)));
    domain.finish_retirement();
    let result = domain.marker();
    assert!(matches!(result, Err(RetirementError::Retired)));
}

#[test]
fn retirement_waits_for_entered_probe_before_finishing() {
    let domain = domain();
    let owner = owner(&domain);
    let observer = owner.handle();
    let entered = probe(&observer).unwrap();
    let (requested_tx, requested_rx) = mpsc::sync_channel(0);
    let (finished_tx, finished_rx) = mpsc::sync_channel(0);
    let retiring = std::thread::spawn(move || {
        owner.request_retirement();
        requested_tx.send(()).unwrap();
        owner.finish_retirement();
        finished_tx.send(()).unwrap();
    });
    tilo_test_env::recv("the retirement request", &requested_rx);
    assert!(observer.value.marker.requested.load(Ordering::Acquire));
    assert!(!observer.value.marker.retired.load(Ordering::Acquire));
    assert!(matches!(
        finished_rx.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    drop(entered);
    tilo_test_env::recv("the retirement to finish", &finished_rx);
    retiring.join().unwrap();
    assert!(matches!(probe(&observer), Err(RetirementError::Retired)));
}

#[test]
fn retirement_poison_is_permanent_and_cleanup_does_not_panic() {
    let domain = domain();
    let owner = owner(&domain);
    let handle = owner.handle();
    // Only the fixed mutex probe runs here; no callback runs.
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _held = probe(&handle).unwrap();
        panic!("intentional private fixed-probe poison");
    }));
    assert!(poisoned.is_err());
    assert!(matches!(probe(&handle), Err(RetirementError::Poisoned)));
    let result = domain.marker();
    assert!(matches!(result, Err(RetirementError::Poisoned)));
    owner.retire();
    domain.retire();
    drop(owner);
    drop(domain);
    assert!(matches!(probe(&handle), Err(RetirementError::Poisoned)));
}

#[test]
fn retirement_last_observer_outlives_owners_without_keeping_them_live() {
    let domain = domain();
    let owner = owner(&domain);
    let first = owner.handle();
    let last = first.clone();
    drop(owner);
    drop(first);
    drop(domain);
    // Only an observer remains; it keeps backing, never logical life.
    assert!(matches!(probe(&last), Err(RetirementError::Retired)));
    drop(last);
}

#[test]
fn retirement_and_drop_allocate_nothing_after_eager_mutex_initialization() {
    let (domain, owner, handle) = {
        let domain = domain();
        let owner = owner(&domain);
        let handle = owner.handle();
        (domain, owner, handle)
    };
    let ((), stats) = allocations::allocations_only(|| {
        handle.request_retirement();
        handle.finish_retirement();
        drop(owner);
        drop(handle);
        drop(domain);
    });
    assert_eq!(stats.allocations, 0);
}

// Fixed negative observation for actual SDK lifecycle synchronization tests.
// No guard, positive authority, or callback is exposed outside cfg(test).
pub(crate) fn retirement_requested(handle: &RetirementHandle) -> bool {
    handle.value.marker.requested.load(Ordering::Acquire)
}
