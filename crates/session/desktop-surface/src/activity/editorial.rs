//! Exact, frontier-bound history organization requests. Owner admission is
//! checked at execution, never inferred from display attribution.
use automerge::ChangeHash;
use ui::{GroupNameRequest, HistoryEntryId};

#[derive(Clone, Debug)]
pub struct HistoryEditorialRequest {
    pub room: String,
    pub origin: super::HistoryOrigin,
    pub frontier: Vec<ChangeHash>,
    pub operation: HistoryEditorialOperation,
}

#[derive(Clone, Debug)]
pub struct EditorialSession {
    pub record: Option<String>,
    pub start: HistoryEntryId,
    pub end: HistoryEntryId,
    pub label: String,
}

#[derive(Clone, Debug)]
pub enum HistoryEditorialOperation {
    Name(GroupNameRequest),
    Split {
        record: String,
        left_end: HistoryEntryId,
        right_start: HistoryEntryId,
        label: String,
        right_label: String,
    },
    Merge {
        records: Vec<String>,
        start: HistoryEntryId,
        end: HistoryEntryId,
        label: String,
    },
    MoveBoundary {
        left: EditorialSession,
        right: EditorialSession,
        left_end: HistoryEntryId,
        right_start: HistoryEntryId,
    },
}
