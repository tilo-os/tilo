//! Per-participant view operations: `move-caret`, `select-range`,
//! `select-block`, `select-all` and `find-text`.
//!
//! They change only the invoking participant's [`ParticipantView`](crate::actions::participant::ParticipantView) (caret,
//! selection, Find), never the document; they are published as the
//! participant view effects of [`crate::effects::WordProcessorEffect`]. The
//! request is applied with [`apply`](crate::actions::view_ops::apply) against the current document, which
//! runs the same [`editing`](crate::actions::editing) function the keyboard/mouse path uses.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

use super::editing::{self, CaretTarget, EditResult, EditingError};
use super::participant::{ParticipantView, ViewPoint};
use super::{RegionKind, SelectionTarget};

/// One view change requested by a participant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "op")]
pub enum ViewOperation {
    MoveCaret { to: CaretTarget },
    SelectRange { range: SelectionTarget },
    SelectBlock { block: String },
    SelectAll,
    FindText { query: String },
}

/// Apply one view operation to the invoker's view. Pure; returns no
/// envelopes, only the next view.
pub fn apply(view: &ParticipantView, doc: &AutoCommit, operation: &ViewOperation) -> EditResult {
    match operation {
        ViewOperation::MoveCaret { to } => editing::move_caret(view, doc, to),
        ViewOperation::SelectRange { range } => editing::select_range(view, doc, range),
        ViewOperation::SelectBlock { block } => editing::select_block(view, doc, block),
        ViewOperation::SelectAll => editing::select_all(view, doc),
        ViewOperation::FindText { query } => editing::find(view, doc, query),
    }
}

/// `go-to` on a search hit or a cell for a participant view: select one exact body range, as the widget's
/// `navigate_to_range` does for its own window.
pub fn navigate_to(
    view: &ParticipantView,
    doc: &AutoCommit,
    block: &str,
    start: usize,
    end: usize,
) -> EditResult {
    if view.region != RegionKind::Body {
        return Err(EditingError::Invalid(
            "leave header or footer editing before navigating body results".into(),
        ));
    }
    if start > end {
        return Err(EditingError::Invalid(
            "the requested document range is stale".into(),
        ));
    }
    let (_, mut next) = editing::select_range(
        view,
        doc,
        &SelectionTarget {
            start_block: block.to_owned(),
            start,
            end_block: block.to_owned(),
            end,
        },
    )?;
    if start == end {
        next.anchor = None;
        next.caret = ViewPoint::new(block, start);
    }
    Ok((Vec::new(), next))
}
