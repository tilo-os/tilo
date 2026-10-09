//! Change notification for what a live admission decision reads.
//!
//! A session that is already admitted is re-checked when something its
//! admission depended on may have changed, instead of on a fixed clock:
//! revocations, the relay-wide trust policy, a room's membership, its
//! signed-authority users and its authenticated-admission switch, the live
//! subscriber a session registered, and agent delegations. Every writer of
//! those calls [`bump`]; consumers hold a [`subscribe`]d receiver and re-check
//! when it reports a change.
//!
//! The epoch is a `watch`, not a `Notify`: it keeps the latest value, so a
//! consumer busy with its previous re-check cannot miss a change made
//! meanwhile. It is process-wide rather than per relay so that a room's
//! runtime can announce its own narrowing without a handle threaded into every
//! place it is opened. Two relays in one process (only tests run that way)
//! wake each other's consumers needlessly; a spurious wake costs one in-memory
//! re-check and never grants anything.
//!
//! Wall-clock expiry (certificates, delegations, grants) is not a change any
//! writer can announce, so consumers also sleep until their next deadline, and
//! keep a slow backstop for a writer that forgets to bump.

use std::sync::OnceLock;

use tokio::sync::watch;

fn epoch() -> &'static watch::Sender<u64> {
    static EPOCH: OnceLock<watch::Sender<u64>> = OnceLock::new();
    EPOCH.get_or_init(|| watch::channel(0).0)
}

/// Announce that something an admission decision reads may have narrowed.
pub(crate) fn bump() {
    epoch().send_modify(|epoch| *epoch = epoch.wrapping_add(1));
}

/// A receiver that reports each [`bump`] after this call.
pub(crate) fn subscribe() -> watch::Receiver<u64> {
    epoch().subscribe()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_bump_while_busy_is_not_missed() {
        let mut changes = subscribe();
        changes.mark_unchanged();
        // Bumped before the consumer waits: the wait still returns at once.
        bump();
        tilo_test_env::within("the bump to be seen", changes.changed())
            .await
            .unwrap();
    }
}
