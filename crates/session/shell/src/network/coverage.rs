//! Which retained messages a checkpoint provably holds.
//!
//! A durable host trims a document's retained history only by the entries a
//! newer checkpoint names as covered (see `tilo_relay::Coverage`). The
//! shell names only what the saved bytes contain: every retained message it
//! sends or receives is logged with the changes it carries, and a checkpoint
//! lists the logged messages whose changes are all in it.

use std::collections::VecDeque;

use automerge::ChangeHash;

/// Most messages remembered per document — as many as one checkpoint may
/// name (`tilo_relay::MAX_COVERED_ENTRIES`).
pub const MAX_LOGGED_ENTRIES: usize = tilo_relay::MAX_COVERED_ENTRIES;

/// Retained messages seen on one document, oldest first.
#[derive(Debug, Default)]
pub struct CoverageLog {
    entries: VecDeque<([u8; 32], Vec<ChangeHash>)>,
}

impl CoverageLog {
    /// Remember a retained message and the changes it carries.
    pub fn record(&mut self, entry: [u8; 32], required: Vec<ChangeHash>) {
        if self.entries.iter().any(|(known, _)| *known == entry) {
            return;
        }
        if self.entries.len() >= MAX_LOGGED_ENTRIES {
            self.entries.pop_front();
        }
        self.entries.push_back((entry, required));
    }

    /// The logged messages whose every change `holds` confirms.
    pub fn covered(&self, mut holds: impl FnMut(&[ChangeHash]) -> bool) -> Vec<[u8; 32]> {
        self.entries
            .iter()
            .filter(|(_, required)| holds(required))
            .map(|(entry, _)| *entry)
            .collect()
    }
}

/// The change hashes of a shell-room `SyncResp`, whose blobs are single
/// Automerge changes. `None` if any blob is not one.
pub fn room_changes(changes: &[Vec<u8>]) -> Option<Vec<ChangeHash>> {
    changes
        .iter()
        .map(|blob| {
            automerge::Change::try_from(blob.as_slice())
                .ok()
                .map(|change| change.hash())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_messages_whose_changes_are_all_held_are_named() {
        let mut log = CoverageLog::default();
        let (a, b) = (ChangeHash([1; 32]), ChangeHash([2; 32]));
        log.record([10; 32], vec![a]);
        log.record([11; 32], vec![a, b]);
        log.record([10; 32], vec![a]);
        assert_eq!(
            log.covered(|required| required.iter().all(|hash| *hash == a)),
            vec![[10; 32]]
        );
    }
}
