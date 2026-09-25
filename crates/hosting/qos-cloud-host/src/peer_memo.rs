//! Exact, content-addressed memo of `Store::peer_transition_at`.
//!
//! Restoring the committed [`StatePeer`] loads and validates every Automerge
//! document of the workspace, which dominates a Store read (hundreds of
//! milliseconds on a multi-megabyte workspace), and one agent step used to pay
//! it dozens of times: every task-ledger check, lease revalidation and
//! membership snapshot restores it again although its inputs had not changed.
//!
//! The memo key is a BLAKE3 digest of *every* input the restore and the
//! expired-identity transition read: room, host signing identity, identity
//! root, authority, roster, revocations and the exact committed peer bytes.
//! A hit is only used inside the time interval in which the same inputs give
//! the same result: from the restore instant up to the next certificate
//! issue/expiry or revocation instant of the roster, the authority *and* every
//! certificate the restored peer carries (so the expired-identity set is
//! constant), and at most [`MAX_REUSE_NS`] after the restore. Outside that
//! interval, or on any input change (a document commit changes the peer
//! bytes), the peer is restored again. Task-ledger commits do not change these
//! inputs, so they keep the memo.
//!
//! A hit is shared read-only (`Store::peer_shared`) or cloned
//! (`Store::peer`): callers that mutate own their copy exactly as with a
//! fresh restore. Nothing here grants authority.
use qos_peer_state::StatePeer;
use std::sync::{Arc, Mutex};

/// Longest interval one restore is reused even when no input and no identity
/// time boundary changes.
const MAX_REUSE_NS: u64 = 5_000_000_000;
/// Retained restores (distinct inputs, e.g. the state just before and after
/// a document commit). The oldest is dropped first.
const ENTRIES: usize = 2;

struct Entry {
    key: [u8; 32],
    at_ns: u64,
    until_ns: u64,
    peers: Arc<(StatePeer, StatePeer)>,
}

#[cfg(not(test))]
static MEMO: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
// Tests run in parallel threads with their own Stores: per-thread memos keep
// their reuse assertions independent of other tests' entries.
#[cfg(test)]
thread_local! {
    static MEMO: Mutex<Vec<Entry>> = const { Mutex::new(Vec::new()) };
    static CALLER_MEMO: Mutex<Vec<CallerEntry>> = const { Mutex::new(Vec::new()) };
}
fn with_memo<R>(f: impl FnOnce(&Mutex<Vec<Entry>>) -> R) -> R {
    #[cfg(not(test))]
    return f(&MEMO);
    #[cfg(test)]
    MEMO.with(f)
}
fn with_callers<R>(f: impl FnOnce(&Mutex<Vec<CallerEntry>>) -> R) -> R {
    #[cfg(not(test))]
    return f(&CALLER_MEMO);
    #[cfg(test)]
    CALLER_MEMO.with(f)
}

/// Digest of every restore/transition input. `None` when not enrolled.
pub(crate) fn key(state: &crate::State, host_public: &[u8; 32]) -> Option<[u8; 32]> {
    let bound = state.bound.as_ref()?;
    let identity = serde_json::to_vec(&(
        &state.room,
        host_public,
        &bound.root,
        &bound.authority,
        &bound.roster,
        &bound.revocations,
    ))
    .ok()?;
    let mut hasher = blake3::Hasher::new_derive_key("qos cloud committed peer memo v1");
    hasher.update(&(identity.len() as u64).to_le_bytes());
    hasher.update(&identity);
    hasher.update(&(bound.peer.len() as u64).to_le_bytes());
    hasher.update(&bound.peer);
    Some(*hasher.finalize().as_bytes())
}

/// End of the exact reuse interval of a restore at `at_ns`.
pub(crate) fn until(state: &crate::State, peer: &StatePeer, at_ns: u64) -> u64 {
    let mut until = crate::agent_runtime_adapter::store_hint::peer_reuse_until(state, at_ns)
        .min(at_ns.saturating_add(MAX_REUSE_NS));
    let mut bound_at = |instant: Option<u64>| {
        if let Some(instant) = instant.filter(|instant| *instant > at_ns) {
            until = until.min(instant);
        }
    };
    for certificate in peer.certificates() {
        bound_at(certificate.expires_at_ns);
        bound_at(Some(certificate.issued_at_ns));
    }
    until
}

#[cfg(test)]
thread_local! {
    static RESTORES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}
/// Test-only: count one actual restore on this thread.
#[cfg(test)]
pub(crate) fn count_restore() {
    RESTORES.with(|count| count.set(count.get() + 1));
}
#[cfg(test)]
thread_local! {
    static DERIVES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}
#[cfg(test)]
pub(crate) fn restores() -> u64 {
    RESTORES.with(std::cell::Cell::get)
}

static OFF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Diagnostic A/B switch for latency measurements: a `peer-memo-off` file in
/// the Store root when the agent runtime opens makes every read restore.
pub(crate) fn configure(root: &std::path::Path) {
    if root.join("peer-memo-off").is_file() {
        OFF.store(true, std::sync::atomic::Ordering::Relaxed);
        tracing::warn!("committed peer memo disabled by diagnostic switch");
    }
}
fn disabled() -> bool {
    OFF.load(std::sync::atomic::Ordering::Relaxed)
}

/// The retained `(before, after)` transition, shared read-only.
pub(crate) fn lookup(key: &[u8; 32], now_ns: u64) -> Option<Arc<(StatePeer, StatePeer)>> {
    if disabled() {
        return None;
    }
    with_memo(|memo| {
        memo.lock()
            .ok()?
            .iter()
            .find(|entry| &entry.key == key && entry.at_ns <= now_ns && now_ns < entry.until_ns)
            .map(|entry| entry.peers.clone())
    })
}

pub(crate) fn retain(key: [u8; 32], at_ns: u64, until_ns: u64, peers: Arc<(StatePeer, StatePeer)>) {
    if until_ns <= at_ns {
        return;
    }
    with_memo(|memo| {
        let Ok(mut memo) = memo.lock() else {
            return;
        };
        memo.retain(|entry| entry.key != key);
        if memo.len() >= ENTRIES {
            memo.remove(0);
        }
        memo.push(Entry {
            key,
            at_ns,
            until_ns,
            peers,
        });
    })
}

/// Longest reuse of one authenticated agent-caller derivation (owner,
/// permissions, certificate and governance revision of one device, read from
/// one memoized peer). Several task checks inside one step derive the same
/// caller within milliseconds; the derivation reads the governed room each
/// time. Role and membership expiry inside the room are evaluated at the
/// derivation instant, so reuse is kept this short.
const CALLER_REUSE_NS: u64 = 100_000_000;
const CALLERS: usize = 8;
struct CallerEntry {
    peers: std::sync::Weak<(StatePeer, StatePeer)>,
    key: ([u8; 32], u64, String, [u8; 32], String),
    at_ns: u64,
    caller: qos_peer_state::owner_commit::VerifiedAgentCaller,
}
#[cfg(not(test))]
static CALLER_MEMO: Mutex<Vec<CallerEntry>> = Mutex::new(Vec::new());

/// `derive()` for (`peers`, scope, generation, room, device, `origin`) at
/// `now_ns`, or its result from at most [`CALLER_REUSE_NS`] earlier over the
/// identical shared peer. `origin` is the exact prior proof a refresh
/// compares against (empty for a fresh derivation). Only successful
/// derivations are retained.
#[allow(
    clippy::too_many_arguments,
    reason = "the memo key covers each caller input"
)]
pub(crate) fn caller(
    peers: &Arc<(StatePeer, StatePeer)>,
    scope: [u8; 32],
    generation: u64,
    room: &str,
    device: [u8; 32],
    origin: String,
    now_ns: u64,
    derive: impl FnOnce() -> Result<qos_peer_state::owner_commit::VerifiedAgentCaller, String>,
) -> Result<qos_peer_state::owner_commit::VerifiedAgentCaller, String> {
    let key = (scope, generation, room.to_owned(), device, origin);
    if !disabled() {
        let found = with_callers(|memo| {
            let memo = memo.lock().ok()?;
            memo.iter()
                .find(|entry| {
                    entry.key == key
                        && entry.at_ns <= now_ns
                        && now_ns - entry.at_ns < CALLER_REUSE_NS
                        && entry
                            .peers
                            .upgrade()
                            .is_some_and(|retained| Arc::ptr_eq(&retained, peers))
                })
                .map(|entry| entry.caller.clone())
        });
        if let Some(caller) = found {
            return Ok(caller);
        }
    }
    #[cfg(test)]
    DERIVES.with(|count| count.set(count.get() + 1));
    let caller = derive()?;
    with_callers(|memo| {
        if let Ok(mut memo) = memo.lock() {
            memo.retain(|entry| entry.key != key && entry.peers.strong_count() > 0);
            if memo.len() >= CALLERS {
                memo.remove(0);
            }
            memo.push(CallerEntry {
                peers: Arc::downgrade(peers),
                key,
                at_ns: now_ns,
                caller: caller.clone(),
            });
        }
    });
    Ok(caller)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fingerprint(peer: &StatePeer) -> (String, Vec<[u8; 32]>, usize) {
        (
            peer.room().to_owned(),
            peer.certificates()
                .iter()
                .map(|c| c.device_public_key)
                .collect(),
            peer.rows().len(),
        )
    }
    #[test]
    fn peer_memo_reuses_an_unchanged_restore_inside_its_interval_only() {
        let (_dir, store, _) = crate::agent_owner_tests::enrolled();
        let start = restores();
        let first = store.peer_at_for_test(1_000).unwrap();
        assert_eq!(restores(), start + 1, "first read restores");
        let again = store.peer_at_for_test(2_000).unwrap();
        assert_eq!(restores(), start + 1, "unchanged inputs reuse the restore");
        assert_eq!(fingerprint(&first), fingerprint(&again));
        // Before the restore instant, or past the longest reuse interval,
        // the peer is restored again.
        store.peer_at_for_test(500).unwrap();
        assert_eq!(restores(), start + 2);
        store.peer_at_for_test(500 + MAX_REUSE_NS).unwrap();
        assert_eq!(restores(), start + 3);
    }
    #[test]
    fn peer_memo_key_covers_peer_bytes_and_identity_inputs() {
        let (_dir, store, _) = crate::agent_owner_tests::enrolled();
        let host = store.key().verifying_key().to_bytes();
        let base = key(&store.state, &host).unwrap();
        let mut changed = store.state.clone();
        changed.bound.as_mut().unwrap().peer.push(0);
        assert_ne!(key(&changed, &host).unwrap(), base, "peer bytes");
        let mut changed = store.state.clone();
        changed.bound.as_mut().unwrap().roster.clear();
        assert_ne!(key(&changed, &host).unwrap(), base, "roster");
        let mut changed = store.state.clone();
        changed.room.push('x');
        assert_ne!(key(&changed, &host).unwrap(), base, "room");
        assert_ne!(key(&store.state, &[9; 32]).unwrap(), base, "host identity");
        // Task-ledger and other private state do not change the key.
        let mut changed = store.state.clone();
        changed.generation += 1;
        changed.private_agent_ledger = None;
        assert_eq!(key(&changed, &host).unwrap(), base);
    }
    #[test]
    fn caller_memo_reuses_one_derivation_briefly_over_the_same_peer() {
        let (_dir, store, device) = crate::agent_owner_tests::enrolled();
        let derives = || DERIVES.with(std::cell::Cell::get);
        let start = derives();
        let first = store.agent_caller(device).unwrap();
        let again = store.agent_caller(device).unwrap();
        assert_eq!(derives(), start + 1, "one derivation, then reuse");
        assert_eq!(format!("{first:?}"), format!("{again:?}"));
        // A refresh is keyed by its exact prior proof: derived once, reused.
        store.refresh_agent_caller(&first).unwrap();
        store.refresh_agent_caller(&first).unwrap();
        assert_eq!(derives(), start + 2);
        // A different device is its own derivation (and fails here).
        assert!(store.agent_caller([3; 32]).is_err());
        assert!(
            store.agent_caller([3; 32]).is_err(),
            "failures are not retained"
        );
        assert_eq!(derives(), start + 4);
    }
    #[test]
    fn peer_memo_interval_ends_at_the_next_certificate_boundary() {
        let (_dir, store, _) = crate::agent_owner_tests::enrolled();
        let peer = store.peer_at_for_test(1_000).unwrap();
        let until = until(&store.state, &peer, 1_000);
        assert!(until > 1_000 && until <= 1_000 + MAX_REUSE_NS);
        for certificate in peer.certificates() {
            if let Some(expiry) = certificate.expires_at_ns.filter(|e| *e > 1_000) {
                assert!(until <= expiry);
            }
        }
    }
}
