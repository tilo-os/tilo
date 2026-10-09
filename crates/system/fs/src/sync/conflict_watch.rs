//! Which conflicts a reconciliation pass looks for.
//!
//! Conflicts come only from changes another replica made: a record gains a
//! second head when a concurrent version arrives, and the namespace holds a
//! concurrent edit only after a merge. So a pass looks only at records
//! written since it last looked ([`RecordRepository::written_since`]), and
//! scans the namespace only when changes from another replica were merged
//! since its last scan ([`Namespace::merge_generation`]). An idle pass —
//! nothing received, nothing written — does no conflict work at all.
//!
//! Both signals come from the stores themselves, not from the pass that
//! received the changes, so changes that arrive any other way (the gossip
//! pump, a peer's own exchange with this device, a pull outside the loop)
//! are covered too. Still, the first pass after start, and one pass every
//! [`CONFLICT_SWEEP_INTERVAL`] after it, look everywhere.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use fs_protocol::FileRecordId;

use crate::conflict::ContentConflict;
use crate::namespace::Namespace;
use crate::record_publisher::RecordPublisher;
use crate::record_repository::RecordRepository;

/// How often a pass looks for conflicts everywhere without a sign that one
/// arrived: anti-entropy for a signal missed in a way nobody foresaw.
pub(super) const CONFLICT_SWEEP_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// What the reconciliation loop has already looked at for conflicts.
#[derive(Debug, Default)]
pub(super) struct ConflictWatch {
    /// When the last full sweep began; `None` before the first.
    last_sweep: Option<Instant>,
    /// The repository write generation every record up to has been
    /// checked for concurrent heads; `None` checks every record.
    records_checked: Option<u64>,
    /// Records whose check failed for a reason that may pass: checked
    /// again next pass.
    records_retry: BTreeSet<FileRecordId>,
    /// The namespace merge generation the last scan saw; `None` scans.
    namespace_scanned: Option<u64>,
}

impl ConflictWatch {
    /// Begins the conflict part of a pass at `now`. The first pass, and
    /// one every [`CONFLICT_SWEEP_INTERVAL`] after it, look everywhere.
    pub(super) fn begin_pass(&mut self, now: Instant) {
        let sweep = self
            .last_sweep
            .is_none_or(|last| now.saturating_duration_since(last) >= CONFLICT_SWEEP_INTERVAL);
        if sweep {
            self.last_sweep = Some(now);
            self.records_checked = None;
            self.namespace_scanned = None;
        }
    }

    /// Publishes a conflict for each record of `workspace_id` written since
    /// the last pass looked that now has concurrent heads. Returns whether
    /// any conflict was published.
    pub(super) fn publish_record_conflicts(
        &mut self,
        publisher: &RecordPublisher,
        repository: &RecordRepository,
        workspace_id: uuid::Uuid,
    ) -> bool {
        let (generation, written) = repository.written_since(workspace_id, self.records_checked);
        let mut check = std::mem::take(&mut self.records_retry);
        check.extend(written);
        let mut published = false;
        for record in check {
            match publisher.publish_concurrent_head_conflict(record) {
                Ok(changed) => published |= changed,
                Err(error) => {
                    eprintln!("record conflict publication failed: {error}");
                    // A record at a size bound fails the same way every
                    // time; its next write brings it back.
                    if !error.is_capacity_exhausted() {
                        self.records_retry.insert(record);
                    }
                }
            }
        }
        self.records_checked = Some(generation);
        published
    }

    /// The namespace's content conflicts still to materialise, scanned for
    /// only when changes from another replica were merged since the last
    /// scan (or a sweep or a retry asks).
    pub(super) fn namespace_conflicts(&mut self, namespace: &Namespace) -> Vec<ContentConflict> {
        let generation = namespace.merge_generation();
        if self.namespace_scanned == Some(generation) {
            return Vec::new();
        }
        self.namespace_scanned = Some(generation);
        crate::conflict::detect(namespace)
    }

    /// A conflict found could not be materialised yet (its content is not
    /// reachable): scan again next pass.
    pub(super) const fn retry_namespace(&mut self) {
        self.namespace_scanned = None;
    }
}
