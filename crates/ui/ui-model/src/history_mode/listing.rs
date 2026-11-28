//! What a window's History lists: the changes of the documents the window keeps, read from their own
//! history. These are the window's document and the parts of its chains. The workspace's feed adds
//! who made each change ([`super::feed`]); without a session, History lists the same changes.

use automerge::{AutoCommit, ChangeHash};

use super::HistoryParts;

use super::sorted_heads;
use crate::HistoryEntry;
use crate::NamedRange;
use crate::history_source::{HistorySource, groups::read_named_ranges};

/// How many of the newest changes History lists when it opens, and how many more each request for
/// earlier ones adds: the first page the activity's feed answers.
const HISTORY_PAGE: usize = 200;

pub(super) struct Listing {
    limit: usize,
    /// The frontiers the rows were read at: the window's document's, then every part's.
    read_at: Option<(Vec<ChangeHash>, Vec<ChangeHash>)>,
    entries: Vec<HistoryEntry>,
    named: Vec<NamedRange>,
    has_earlier: bool,
}

impl Listing {
    pub(super) fn new() -> Self {
        Self {
            limit: HISTORY_PAGE,
            read_at: None,
            entries: Vec::new(),
            named: Vec::new(),
            has_earlier: false,
        }
    }

    pub(super) fn limit(&self) -> usize {
        self.limit
    }

    pub(super) fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    pub(super) fn named(&self) -> &[NamedRange] {
        &self.named
    }

    pub(super) fn has_earlier(&self) -> bool {
        self.has_earlier
    }

    /// List another page of older changes on the next read.
    pub(super) fn show_earlier(&mut self) {
        self.limit = self.limit.saturating_add(HISTORY_PAGE);
        self.read_at = None;
    }

    /// Whether `own` or a part moved since the rows were read.
    pub(super) fn moved(&self, own: &[ChangeHash], parts: Option<&dyn HistoryParts>) -> bool {
        self.read_at.as_ref().is_none_or(|(read_own, read_parts)| {
            sorted_heads(own.to_vec()) != *read_own
                || sorted_heads(parts.map(HistoryParts::heads).unwrap_or_default()) != *read_parts
        })
    }

    /// Read the newest changes of `own` and of `parts`, in time order. Returns whether the rows
    /// were read again. Each part's genesis is left out: nobody made it.
    pub(super) fn read(&mut self, own: &AutoCommit, parts: Option<&dyn HistoryParts>) -> bool {
        let mut own = super::committed_clone(own);
        let own_heads = own.get_heads();
        if !self.moved(&own_heads, parts) {
            return false;
        }
        let parts_heads = parts.map(HistoryParts::heads).unwrap_or_default();
        let source = HistorySource::new().with_max_entries(self.limit);
        let mut entries = source.entries(&mut own);
        let mut earlier = own.get_changes_meta(&[]).len() > self.limit;
        for (_, mut part) in parts.map(HistoryParts::documents).unwrap_or_default() {
            earlier |= part.get_changes_meta(&[]).len() > self.limit;
            entries.extend(
                source
                    .entries(&mut part)
                    .into_iter()
                    .filter(|entry| !entry.is_genesis()),
            );
        }
        entries.sort_by_key(|entry| entry.timestamp);
        let older = entries.len().saturating_sub(self.limit);
        entries.drain(..older);
        self.named = read_named_ranges(&own, source.authors());
        self.entries = entries;
        self.has_earlier = earlier || older > 0;
        self.read_at = Some((sorted_heads(own_heads), sorted_heads(parts_heads)));
        true
    }
}
