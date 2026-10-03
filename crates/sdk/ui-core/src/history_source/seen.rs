//! Private, causal-ish seen state for collaborative history.
//!
//! Wall-clock cursors are incorrect for CRDT history: a delayed concurrent
//! change may arrive with an older timestamp, and several changes can share a
//! timestamp. The stable content-addressed entry ids are the truth. A frontier
//! stores the bounded set of ids present when the person finished viewing;
//! any later id is unread regardless of its clock.

use std::collections::HashSet;

use crate::widget::history::{HistoryEntry, HistoryEntryId};
use crate::widget::history_group::{from_hex16, hex16};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistorySeenFrontier {
    ids: HashSet<HistoryEntryId>,
}

impl HistorySeenFrontier {
    pub fn from_hex(ids: impl IntoIterator<Item = String>) -> Self {
        Self {
            ids: ids
                .into_iter()
                .filter_map(|encoded| from_hex16(&encoded))
                .collect(),
        }
    }

    /// Stable sorted form for private settings persistence.
    pub fn to_hex(&self) -> Vec<String> {
        let mut encoded: Vec<String> = self.ids.iter().map(hex16).collect();
        encoded.sort();
        encoded
    }

    pub fn contains(&self, id: &HistoryEntryId) -> bool {
        self.ids.contains(id)
    }

    pub fn unread_count(&self, entries: &[HistoryEntry]) -> usize {
        entries
            .iter()
            .filter(|entry| !self.contains(&entry.id))
            .count()
    }

    pub fn unseen<'a>(&self, entries: &'a [HistoryEntry]) -> Vec<&'a HistoryEntry> {
        entries
            .iter()
            .filter(|entry| !self.contains(&entry.id))
            .collect()
    }

    /// Replace, rather than append, because history sources are deliberately
    /// bounded. Ids outside the current retention window cannot become visible
    /// again without a cold-history source supplying them as entries.
    pub fn mark_seen(&mut self, entries: &[HistoryEntry]) {
        self.ids = entries.iter().map(|entry| entry.id).collect();
    }
}

#[cfg(test)]
mod tests {
    use crate::time::{Duration, SystemTime};

    use collab_types::{ArtifactKind, OpClass};

    use super::*;
    use crate::widget::HistoryAuthor;

    fn entry(id: u8, timestamp: u64) -> HistoryEntry {
        HistoryEntry {
            id: HistoryEntryId([id; 16]),
            author: HistoryAuthor::new("Ada", 0.0, "ada"),
            op_class: OpClass::EditMeta,
            artifact_kind: ArtifactKind::File,
            timestamp: SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(timestamp))
                .unwrap_or(SystemTime::UNIX_EPOCH),
            summary: "Changed".into(),
            text_delta: None,
            diff_payload: None,
            redaction: None,
            context: None,
        }
    }

    #[test]
    fn seen_markers_keep_delayed_changes_unread_and_persist_strictly() {
        // phase: delayed_older_changes_are_still_unread
        {
            let first = entry(1, 100);
            let mut frontier = HistorySeenFrontier::default();
            frontier.mark_seen(std::slice::from_ref(&first));
            let delayed = entry(2, 50);
            assert_eq!(frontier.unread_count(&[delayed, first]), 1);
        }
        // phase: persistence_round_trips_and_rejects_malformed_ids
        {
            let entries = [entry(2, 10), entry(1, 20)];
            let mut frontier = HistorySeenFrontier::default();
            frontier.mark_seen(&entries);
            let mut encoded = frontier.to_hex();
            encoded.push("not-an-id".into());
            assert_eq!(HistorySeenFrontier::from_hex(encoded), frontier);
        }
    }
}
