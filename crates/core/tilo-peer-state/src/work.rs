//! The work reading and writing a workspace's history costs, counted for the
//! whole process.
//!
//! Admitting one person must cost a workspace's host the same whether the
//! workspace has ten members or ten thousand, and a change must cost what it
//! changed however long the history it joins. Time is a poor proof of either
//! (it varies with the machine and with everything else running), so the
//! steps whose number would grow with the workspace are counted instead: a
//! test admits someone into a small workspace and a large one, or writes a
//! change into a short history and a long one, and compares.
//!
//! The counts are process-wide, one relaxed add per step, and stay on in
//! every build. A test therefore compares [`Work::since`] around its own work
//! only: a test process runs one test (nextest), and work done elsewhere in
//! the process meanwhile is the test's to keep out.

use std::sync::atomic::{AtomicU64, Ordering};

/// What reading and writing a workspace's history cost, process-wide.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    /// Change frames read from a store.
    pub changes_read: u64,
    /// Change frames written to a store.
    pub changes_written: u64,
    /// Bytes of change frames and of record history read.
    pub bytes_read: u64,
    /// Bytes of those change frames written.
    pub bytes_written: u64,
    /// Durable write batches, however many changes each held: a sequence
    /// that only increases.
    pub commits: u64,
    /// Identity rows decoded from their stored text.
    pub identities_decoded: u64,
    /// Signatures checked in full: device certificates, invitations and
    /// membership operations not checked before
    /// ([`tilo_user_identity::verify_remembered`]).
    pub signatures_verified: u64,
    /// Whole reads of a workspace's admission rows from its document: who is in
    /// it and what each may do.
    pub admission_reads: u64,
    /// Replays of the workspace's membership log.
    pub replays: u64,
    /// Invitation receipts read back and checked.
    pub receipts_read: u64,
    /// Security bases worked out over every member's row
    /// ([`crate::StatePeer::security_basis`]).
    pub security_bases: u64,
    /// Walks of the workspace's grants for what they say of its people: who
    /// administers it, and what each holds beyond their role.
    pub grant_walks: u64,
    /// Readings of who holds the workspace's keys and what each may do, which
    /// deciding whether a record needs keys of its own reads
    /// ([`crate::DocumentKeysBasis`]).
    pub key_access_reads: u64,
}

impl Work {
    /// What the work counted after `before` added: what a test did between
    /// its two readings.
    #[must_use]
    pub const fn since(self, before: Work) -> Work {
        Work {
            changes_read: self.changes_read.saturating_sub(before.changes_read),
            changes_written: self.changes_written.saturating_sub(before.changes_written),
            bytes_read: self.bytes_read.saturating_sub(before.bytes_read),
            bytes_written: self.bytes_written.saturating_sub(before.bytes_written),
            commits: self.commits.saturating_sub(before.commits),
            identities_decoded: self
                .identities_decoded
                .saturating_sub(before.identities_decoded),
            signatures_verified: self
                .signatures_verified
                .saturating_sub(before.signatures_verified),
            admission_reads: self.admission_reads.saturating_sub(before.admission_reads),
            replays: self.replays.saturating_sub(before.replays),
            receipts_read: self.receipts_read.saturating_sub(before.receipts_read),
            security_bases: self.security_bases.saturating_sub(before.security_bases),
            grant_walks: self.grant_walks.saturating_sub(before.grant_walks),
            key_access_reads: self
                .key_access_reads
                .saturating_sub(before.key_access_reads),
        }
    }
}

static CHANGES_READ: AtomicU64 = AtomicU64::new(0);
static CHANGES_WRITTEN: AtomicU64 = AtomicU64::new(0);
static BYTES_READ: AtomicU64 = AtomicU64::new(0);
static BYTES_WRITTEN: AtomicU64 = AtomicU64::new(0);
static COMMITS: AtomicU64 = AtomicU64::new(0);
static IDENTITIES_DECODED: AtomicU64 = AtomicU64::new(0);
static ADMISSION_READS: AtomicU64 = AtomicU64::new(0);
static REPLAYS: AtomicU64 = AtomicU64::new(0);
static RECEIPTS_READ: AtomicU64 = AtomicU64::new(0);
static SECURITY_BASES: AtomicU64 = AtomicU64::new(0);
static GRANT_WALKS: AtomicU64 = AtomicU64::new(0);
static KEY_ACCESS_READS: AtomicU64 = AtomicU64::new(0);

/// The work counted so far in this process.
#[must_use]
pub fn now() -> Work {
    Work {
        changes_read: load(&CHANGES_READ),
        changes_written: load(&CHANGES_WRITTEN),
        bytes_read: load(&BYTES_READ),
        bytes_written: load(&BYTES_WRITTEN),
        commits: load(&COMMITS),
        identities_decoded: load(&IDENTITIES_DECODED),
        signatures_verified: tilo_user_identity::signature_checks(),
        admission_reads: load(&ADMISSION_READS),
        replays: load(&REPLAYS),
        receipts_read: load(&RECEIPTS_READ),
        security_bases: load(&SECURITY_BASES),
        grant_walks: load(&GRANT_WALKS),
        key_access_reads: load(&KEY_ACCESS_READS),
    }
}

fn load(counter: &AtomicU64) -> u64 {
    counter.load(Ordering::Relaxed)
}

fn add(counter: &AtomicU64, count: u64) {
    counter.fetch_add(count, Ordering::Relaxed);
}

/// Count `bytes` read of a record's history to commit a write. A write must
/// cost what it changed, however long its record's history: the steps that
/// would read the history again count the bytes they read instead.
pub(crate) fn read(bytes: usize) {
    #[cfg(test)]
    add(&BYTES_READ, u64::try_from(bytes).unwrap_or(u64::MAX));
    #[cfg(not(test))]
    let _ = bytes;
}

/// Count one change frame written to a store and its bytes.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn frame_written(bytes: u64) {
    add(&CHANGES_WRITTEN, 1);
    add(&BYTES_WRITTEN, bytes);
}

/// Count `frames` change frames read from a store and `bytes` of them.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn frames_read(frames: u64, bytes: u64) {
    add(&CHANGES_READ, frames);
    add(&BYTES_READ, bytes);
}

/// Count one durable write batch: a store's sync of the frames appended to
/// it, however many shared it, or a host's publication of its workspaces'
/// state.
pub(crate) fn commit() {
    add(&COMMITS, 1);
}

pub(crate) fn identity_decoded() {
    add(&IDENTITIES_DECODED, 1);
}

pub(crate) fn admission_read() {
    add(&ADMISSION_READS, 1);
}

pub(crate) fn receipt_read() {
    add(&RECEIPTS_READ, 1);
}

pub(crate) fn security_basis_worked_out() {
    add(&SECURITY_BASES, 1);
}

pub(crate) fn grants_walked() {
    add(&GRANT_WALKS, 1);
}

pub(crate) fn key_access_read() {
    add(&KEY_ACCESS_READS, 1);
}

pub(crate) fn replayed() {
    add(&REPLAYS, 1);
}

/// `work`, its counting taken back: a test's check of a narrowed reading
/// against the whole one is no part of the work it checks.
#[cfg(test)]
pub(crate) fn uncounted<T>(work: impl FnOnce() -> T) -> T {
    let before = now();
    let done = work();
    retract(now().since(before));
    done
}

/// Take back `added`, what work a test did while it checked something else.
#[cfg(test)]
fn retract(added: Work) {
    undo(&CHANGES_READ, added.changes_read);
    undo(&CHANGES_WRITTEN, added.changes_written);
    undo(&BYTES_READ, added.bytes_read);
    undo(&BYTES_WRITTEN, added.bytes_written);
    undo(&COMMITS, added.commits);
    undo(&IDENTITIES_DECODED, added.identities_decoded);
    undo(&ADMISSION_READS, added.admission_reads);
    undo(&REPLAYS, added.replays);
    undo(&RECEIPTS_READ, added.receipts_read);
    undo(&SECURITY_BASES, added.security_bases);
    tilo_user_identity::retract_signature_checks(added.signatures_verified);
}

#[cfg(test)]
fn undo(counter: &AtomicU64, added: u64) {
    if added == 0 {
        return;
    }
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
        Some(held.saturating_sub(added))
    });
}
