//! Status of an invocation this host never admitted.
//!
//! "No record" (`update: null, uncertain: false`) is the answer while the
//! invocation may still arrive: its issuer legitimately re-sends the same
//! invocation when the host holds no record. Once
//! [`UNADMITTED_STATUS_DEADLINE`] has passed since this host first answered
//! "no record" for it, the answer becomes definitive: the host first fences
//! the identity durably (a `not_found` receipt under the caller's
//! `device:invocation` scope, which every admission path of this Store
//! refuses to reuse: desktop and collaboration receipts, and the private
//! agent owner's cross-journal check), then answers a terminal
//! `unknown_invocation` failure. It is never reported for an invocation that
//! is admitted, or that could still be admitted: the fence is committed
//! under the Store lock after rechecking every journal, before the answer.
use super::*;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Longer than every issuer's own deadline for this invocation (the desktop
/// bridge gives up at 30 s, a ticket drive at 30 s plus backoff).
pub(super) const UNADMITTED_STATUS_DEADLINE: Duration = Duration::from_secs(60);
/// Receipt status of a fenced, never-admitted invocation.
pub(super) const NOT_FOUND: &str = "not_found";
/// Failure code of the definitive answer.
pub(super) const UNKNOWN_INVOCATION: &str = "unknown_invocation";
/// Receipt digest of the fence: never equal to a request digest (hex).
const FENCE_DIGEST: &str = "not_found";
/// Bound on the process-local first-seen clock; eviction only delays.
const MAX_TRACKED: usize = 4096;

type Key = (String, [u8; 32], uuid::Uuid);
/// Process-local and monotonic: a restart only restarts the wait (safe).
static FIRST_UNKNOWN: LazyLock<Mutex<HashMap<Key, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Whether the deadline has passed since this host first held no record.
pub(super) fn deadline_passed(room: &str, device: [u8; 32], invocation: uuid::Uuid) -> bool {
    let now = Instant::now();
    let Ok(mut seen) = FIRST_UNKNOWN.lock() else {
        return false;
    };
    if seen.len() >= MAX_TRACKED
        && !seen.contains_key(&(room.to_owned(), device, invocation))
        && let Some(oldest) = seen
            .iter()
            .min_by_key(|(_, at)| **at)
            .map(|(k, _)| k.clone())
    {
        seen.remove(&oldest);
    }
    let first = *seen
        .entry((room.to_owned(), device, invocation))
        .or_insert(now);
    now.duration_since(first) >= UNADMITTED_STATUS_DEADLINE
}

fn forget(room: &str, device: [u8; 32], invocation: uuid::Uuid) {
    if let Ok(mut seen) = FIRST_UNKNOWN.lock() {
        seen.remove(&(room.to_owned(), device, invocation));
    }
}

#[cfg(test)]
pub(super) fn backdate(room: &str, device: [u8; 32], invocation: uuid::Uuid, by: Duration) {
    let mut seen = FIRST_UNKNOWN.lock().unwrap();
    let at = seen
        .entry((room.to_owned(), device, invocation))
        .or_insert_with(Instant::now);
    *at = at.checked_sub(by).unwrap();
}

/// The definitive answer for a fenced invocation.
pub(super) fn not_found_update(invocation: InvocationId) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation,
        error: ActionFailure {
            code: UNKNOWN_INVOCATION.into(),
            message:
                "The workspace holds no record of this operation. Send it again as a new operation."
                    .into(),
            details: None,
            retryable: false,
        },
    }
}

/// Fence a never-admitted invocation of `device` under the Store lock.
/// `false` when it turned out to be admitted meanwhile (answer normally).
pub(super) fn fence(store: &mut Store, device: [u8; 32], invocation: InvocationId) -> Result<bool> {
    store.ensure_reconciled()?;
    let scope = format!("{}:{}", hex::encode(device), invocation.0);
    if let Some((_, receipt)) = store.state.receipts.get(&scope) {
        return Ok(receipt.status == NOT_FOUND);
    }
    if store.state.canonical_intents.contains_key(&scope)
        || store.state.pending_models.contains_key(&scope)
        || store
            .private_agent_invocation_reserved(device, invocation.0)
            .map_err(anyhow::Error::msg)?
    {
        forget(&store.state.room, device, invocation.0);
        return Ok(false);
    }
    let mut state = store.state.clone();
    state.receipts.insert(
        scope,
        (
            FENCE_DIGEST.into(),
            ActionReceipt {
                invocation_id: invocation.0.to_string(),
                status: NOT_FOUND.into(),
                error: Some("never admitted before its deadline".into()),
                output: None,
            },
        ),
    );
    store.commit(state)?;
    forget(&store.state.room, device, invocation.0);
    Ok(true)
}

#[cfg(test)]
#[path = "unadmitted/tests.rs"]
mod tests;
