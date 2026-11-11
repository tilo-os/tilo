//! The work reading a workspace's governance takes, counted on each thread.
//!
//! Admitting one person must cost a workspace's host the same whether the
//! workspace has ten members or ten thousand. Time is a poor witness of that
//! (it varies with the machine and with everything else running), so the
//! steps whose number would grow with the workspace are counted instead: a
//! test admits someone into a small workspace and a large one and compares.
//! Counting is a thread-local add, cheap enough to stay on in every build.

use std::cell::Cell;

/// What reading governance cost, since the thread last took it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GovernanceWork {
    /// Governed identity rows decoded from their stored text.
    pub identities_decoded: u64,
    /// Signatures checked in full: device certificates, invitations and
    /// membership operations not checked before
    /// ([`tilo_user_identity::verify_remembered`]).
    pub signatures_verified: u64,
    /// Whole-workspace reads of the governance rows from the document.
    pub governance_reads: u64,
    /// Replays of the workspace's membership log.
    pub replays: u64,
    /// Invitation receipts read back from the governance and checked.
    pub receipts_read: u64,
}

thread_local! {
    static WORK: Cell<GovernanceWork> = const { Cell::new(GovernanceWork {
        identities_decoded: 0,
        signatures_verified: 0,
        governance_reads: 0,
        replays: 0,
        receipts_read: 0,
    }) };
}

/// The work counted on this thread since the last call, which starts the
/// count over.
#[must_use]
pub fn take() -> GovernanceWork {
    let mut work = WORK.with(|work| work.replace(GovernanceWork::default()));
    work.signatures_verified = tilo_user_identity::take_signature_checks();
    work
}

/// `work`, its reading uncounted: a test's check of a narrowed reading against
/// the whole one is no part of the work it checks.
#[cfg(test)]
pub(crate) fn uncounted<T>(work: impl FnOnce() -> T) -> T {
    let counted = WORK.with(Cell::get);
    let done = work();
    WORK.with(|work| work.set(counted));
    done
}

fn add(count: impl FnOnce(&mut GovernanceWork)) {
    WORK.with(|work| {
        let mut counted = work.get();
        count(&mut counted);
        work.set(counted);
    });
}

pub(crate) fn identity_decoded() {
    add(|work| work.identities_decoded = work.identities_decoded.saturating_add(1));
}

pub(crate) fn governance_read() {
    add(|work| work.governance_reads = work.governance_reads.saturating_add(1));
}

pub(crate) fn receipt_read() {
    add(|work| work.receipts_read = work.receipts_read.saturating_add(1));
}

pub(crate) fn replayed() {
    add(|work| work.replays = work.replays.saturating_add(1));
}
