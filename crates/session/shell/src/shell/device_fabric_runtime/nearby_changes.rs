//! When Files' Nearby Send view may have changed.
//!
//! Files used to ask for a fresh Nearby snapshot every two seconds, and on an
//! idle desktop that poll was the compositor's only self-wake: every wake is
//! a whole loop turn and costs deep-sleep residency whatever it finds. Now the
//! shell says when to ask. Each fabric pump run, and each Nearby command,
//! compares a digest of what a snapshot is built from (the person's other
//! online devices, the transfer state, the continuity switch) with the last
//! one, and moves a revision when it differs. The broker's
//! `nearbyTransferChanged` wait resolves on that revision; the revision names
//! nothing, so no device, transfer or handle leaves the shell this way.

use std::hash::{Hash as _, Hasher as _};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use tokio::sync::watch;

use super::{NEARBY_CONSENT_TIMEOUT, nearby_drop_state, nearby_store_cell};
use crate::shell::continuity_policy::{ContinuityCapability, ContinuityPolicySnapshot};
use crate::shell::nearby_transfer::{InboxEntry, OutboxEntry, TransferKind};
use crate::shell::network::ShellNetwork;

/// The revision Files' waits follow. It starts at 1, so an endpoint's first
/// wait (from 0) answers at once and fetches its first view.
pub(crate) fn nearby_changes() -> &'static watch::Sender<u64> {
    static CHANGES: OnceLock<watch::Sender<u64>> = OnceLock::new();
    CHANGES.get_or_init(|| watch::Sender::new(1))
}

/// Tell every waiting Files instance to ask again.
pub(crate) fn note_nearby_changed() {
    nearby_changes().send_modify(|revision| *revision = revision.wrapping_add(1).max(1));
}

/// The digest of the view as last observed. `None` until the first
/// observation, and again after a snapshot was served while the store was
/// busy: that snapshot showed no queued transfers, so the next complete
/// observation must announce a change even if the digest is the same.
fn observed() -> &'static Mutex<Option<u64>> {
    static OBSERVED: OnceLock<Mutex<Option<u64>>> = OnceLock::new();
    OBSERVED.get_or_init(|| Mutex::new(None))
}

/// A snapshot was answered from a busy store, so it showed fewer transfers
/// than there are. The next complete observation announces a change.
pub(crate) fn served_incomplete_view() {
    if let Ok(mut observed) = observed().lock() {
        *observed = None;
    }
}

/// Compare what a Nearby snapshot would be built from with the last
/// observation, and announce a change when it differs. Cheap enough for every
/// pump run: it reads the fabric in place and never waits for the store.
pub(crate) fn observe_nearby_view(network: &ShellNetwork, policy: ContinuityPolicySnapshot) {
    let Some(digest) = view_digest(network, policy) else {
        // The store is busy; a later run sees it settled.
        return;
    };
    let changed = observed()
        .lock()
        .is_ok_and(|mut observed| observed.replace(digest) != Some(digest));
    if changed {
        note_nearby_changed();
    }
}

/// When the oldest offer still awaiting consent expires: the view changes
/// then with nothing else happening, so a wait must not outlast it.
pub(crate) fn next_consent_expiry() -> Option<Instant> {
    nearby_drop_state().lock().ok().and_then(|state| {
        state
            .incoming
            .values()
            .filter_map(|pending| pending.received.checked_add(NEARBY_CONSENT_TIMEOUT))
            .min()
    })
}

/// Both queues of the Nearby store, without waiting for it. `None` while it is
/// busy; a store that is not open yet reads as empty, and is asked to open.
pub(super) fn try_store_queues() -> Option<(Vec<InboxEntry>, Vec<OutboxEntry>)> {
    let Ok(guard) = nearby_store_cell().try_lock() else {
        return None;
    };
    let Some(store) = guard.as_ref() else {
        drop(guard);
        super::nearby_store_worker::open_store();
        return Some((Vec::new(), Vec::new()));
    };
    Some((store.inbox().to_vec(), store.outbox().to_vec()))
}

/// Progress in whole percent: a snapshot moves on with every chunk, but Files
/// needs to hear about it at most a hundred times a transfer.
fn percent(done: u64, total: u64) -> u64 {
    done.min(total)
        .saturating_mul(100)
        .checked_div(total)
        .unwrap_or(0)
}

/// A digest of everything a Nearby snapshot is built from, or `None` while the
/// store is busy. It may change when the snapshot would not (an extra ask is
/// harmless); it must not stay the same when the snapshot would change.
fn view_digest(network: &ShellNetwork, policy: ContinuityPolicySnapshot) -> Option<u64> {
    let mut hasher = std::hash::DefaultHasher::new();
    let enabled = policy.permits(ContinuityCapability::FileTransfer);
    enabled.hash(&mut hasher);
    if !enabled {
        // A disabled service answers every snapshot with a refusal.
        return Some(hasher.finish());
    }
    let local = network
        .local_device_fabric_identity()
        .map(|(device, _)| device);
    network.with_device_fabric(|fabric| {
        for snapshot in fabric.snapshots().values() {
            if snapshot.presence.online && local.as_ref() != Some(&snapshot.owner) {
                snapshot.owner.hash(&mut hasher);
                snapshot.presence.label.hash(&mut hasher);
            }
        }
    });
    let (inbox, outbox) = try_store_queues()?;
    let files = |kind: TransferKind| kind != TransferKind::MediaHandoff;
    for entry in inbox.iter().filter(|entry| files(entry.kind)) {
        (entry.id, &entry.name, &entry.source, entry.total_bytes).hash(&mut hasher);
        percent(entry.committed_offset, entry.total_bytes).hash(&mut hasher);
    }
    0xff_u8.hash(&mut hasher);
    for entry in outbox.iter().filter(|entry| files(entry.kind)) {
        (entry.id, &entry.name, &entry.target, entry.total_bytes).hash(&mut hasher);
    }
    let Ok(state) = nearby_drop_state().lock() else {
        return Some(hasher.finish());
    };
    for (id, pending) in &state.incoming {
        (id, &pending.name, &pending.source, pending.total_bytes).hash(&mut hasher);
    }
    0xff_u8.hash(&mut hasher);
    for (id, (target, name, total)) in &state.pending {
        (id, target, name, total).hash(&mut hasher);
    }
    0xff_u8.hash(&mut hasher);
    for (id, (_, sent, total, status)) in &state.progress {
        (id, percent(*sent, *total), total).hash(&mut hasher);
        std::mem::discriminant(status).hash(&mut hasher);
    }
    0xff_u8.hash(&mut hasher);
    for entry in &state.history {
        (
            entry.transfer_id,
            &entry.peer,
            &entry.name,
            entry.total_bytes,
        )
            .hash(&mut hasher);
        std::mem::discriminant(&entry.phase).hash(&mut hasher);
    }
    0xff_u8.hash(&mut hasher);
    for id in state.destinations.keys() {
        id.hash(&mut hasher);
    }
    drop(state);
    Some(hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_announced_in_whole_percent() {
        assert_eq!(percent(0, 0), 0, "an empty transfer has no progress");
        assert_eq!(percent(5, 1_000), 0);
        assert_eq!(percent(10, 1_000), 1);
        assert_eq!(percent(2_000, 1_000), 100, "never past the total");
    }

    #[test]
    fn the_revision_never_reads_as_an_endpoints_first_wait() {
        let before = *nearby_changes().borrow();
        assert_ne!(before, 0, "a first wait from 0 answers at once");
        note_nearby_changed();
        let after = *nearby_changes().borrow();
        assert_ne!(after, before, "a change moves the revision");
        assert_ne!(after, 0);
    }
}
