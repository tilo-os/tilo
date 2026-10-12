//! Spares repeating the parts of certified caller verification that can't
//! answer differently, never the parts that can.
//!
//! * [`join_proof_verified`]: whether an ed25519 join proof verifies is a
//!   pure function of (device key, room, signature). An exact triple that
//!   verified once verifies again, so only successes are remembered, by the
//!   exact bytes. Certificate validity, fleet authorization and membership
//!   are still checked on every verification.
//! * [`with_admitted_now`]: one synchronous call that has just verified a
//!   certified caller against current membership and policy
//!   ([`super::local_action_host`]'s `invoke`) lends that admission to the
//!   dispatch it reaches before returning, so the same presentation is not
//!   verified twice within microseconds on one thread. The scope ends when
//!   the call returns: whatever continues later (a launch wait, a directory
//!   reply, lifecycle checks) runs outside it and verifies again.

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use super::{AdmittedJoinIdentity, VerifiedActionCaller};

static VERIFIED_JOIN_PROOFS: LazyLock<Mutex<HashSet<Vec<u8>>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// A desktop presents a handful of devices in a handful of rooms; the bound
/// only stops a stream of distinct proofs from growing the set.
const VERIFIED_JOIN_PROOF_LIMIT: usize = 4096;

/// Run `verify`, the signature check of `proof` by `device` over the join
/// bytes of `workspace_id`, unless this exact triple verified before.
pub(super) fn join_proof_verified(
    device: &[u8; 32],
    workspace_id: &str,
    proof: &[u8; 64],
    verify: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let mut triple = Vec::with_capacity(96_usize.saturating_add(workspace_id.len()));
    triple.extend_from_slice(device);
    triple.extend_from_slice(proof);
    triple.extend_from_slice(workspace_id.as_bytes());
    if VERIFIED_JOIN_PROOFS
        .lock()
        .is_ok_and(|seen| seen.contains(&triple))
    {
        return Ok(());
    }
    verify()?;
    if let Ok(mut seen) = VERIFIED_JOIN_PROOFS.lock() {
        if seen.len() >= VERIFIED_JOIN_PROOF_LIMIT {
            seen.clear();
        }
        seen.insert(triple);
    }
    Ok(())
}

/// The certified caller the synchronous call running on this thread has
/// verified, and what that verification admitted.
struct AdmittedNow {
    room: String,
    presentation: Vec<u8>,
    admitted: AdmittedJoinIdentity,
}

std::thread_local! {
    static ADMITTED_NOW: RefCell<Option<AdmittedNow>> = const { RefCell::new(None) };
}

/// Run `run` with `admitted`, the admission a verification of `caller` just
/// returned, standing for any further verification of exactly `caller`
/// that `run` reaches on this thread before it returns.
pub(super) fn with_admitted_now<T>(
    caller: &VerifiedActionCaller,
    admitted: AdmittedJoinIdentity,
    run: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<AdmittedNow>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            ADMITTED_NOW.with(|slot| *slot.borrow_mut() = previous);
        }
    }
    if caller.network.is_some() {
        return run();
    }
    let previous = ADMITTED_NOW.with(|slot| {
        slot.replace(Some(AdmittedNow {
            room: caller.room.clone(),
            presentation: caller.presentation.clone(),
            admitted,
        }))
    });
    let _restore = Restore(previous);
    run()
}

/// The admission [`with_admitted_now`] lends for exactly `caller`, if this
/// thread is inside such a call.
pub(super) fn admitted_now(caller: &VerifiedActionCaller) -> Option<AdmittedJoinIdentity> {
    if caller.network.is_some() {
        return None;
    }
    ADMITTED_NOW.with(|slot| {
        slot.borrow()
            .as_ref()
            .filter(|now| now.room == caller.room && now.presentation == caller.presentation)
            .map(|now| now.admitted.clone())
    })
}

#[cfg(test)]
std::thread_local! {
    static VERIFICATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Count one full verification of a certified caller's presentation.
#[cfg(test)]
pub(super) fn count_verification() {
    VERIFICATIONS.with(|count| count.set(count.get().saturating_add(1)));
}

/// Full certified-caller verifications run on this thread.
#[cfg(test)]
pub(crate) fn verifications_on_this_thread() -> usize {
    VERIFICATIONS.with(std::cell::Cell::get)
}
