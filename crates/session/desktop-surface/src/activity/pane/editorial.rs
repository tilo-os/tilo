use super::{ActivityTree, HistoryEntryId, NamedRange, RoomActivityPane, RoomScope};
use crate::activity::{EditorialSession, HistoryEditorialOperation as Op, HistoryEditorialRequest};
use automerge::transaction::Transactable;

impl RoomActivityPane {
    /// Whether the request addresses the exact currently displayed local pane.
    /// This observation grants neither writes nor remote admission.
    pub fn history_editorial_matches(
        &self,
        tree: &impl ActivityTree,
        request: &HistoryEditorialRequest,
    ) -> bool {
        self.scope == RoomScope::ThisRoom
            && request.room == self.task_room
            && request.origin == super::super::index::HistoryOrigin::DesktopTree
            && request.frontier == self.last_heads
            && self.last_active == Some(tree.active.raw())
            && self
                .host
                .authorizes_local_history_document(tree.document(), &request.room)
    }

    /// Canonical editorial door for UI and trusted in-process semantic callers.
    /// The native local host must hold the live tree's room-scoped projection
    /// capability. Admitted callers instead require current owner authorization;
    /// this method does not expose a remote action or grant guest host access.
    /// Requests bind the exact reviewed frontier; unsupported record owners are
    /// refused rather than writing their projected ids into the tree.
    pub fn apply_history_editorial(
        &mut self,
        tree: &mut impl ActivityTree,
        request: &HistoryEditorialRequest,
    ) -> bool {
        if self.scope != RoomScope::ThisRoom
            || request.room != self.task_room
            || request.origin != super::super::index::HistoryOrigin::DesktopTree
            || !self.host.history_edits_available()
            || tree.document().pending_ops() != 0
            || tree.document_mut().get_heads() != request.frontier
            || request.frontier != self.last_heads
            || self.last_active != Some(tree.active.raw())
            || (!(self.host.history_groups_are_local()
                && self
                    .host
                    .authorizes_local_history_document(tree.document(), &request.room))
                && self
                    .host
                    .authorize_history_edit(
                        &self.task_room,
                        &[super::super::index::HistoryOrigin::DesktopTree],
                    )
                    .is_err())
        {
            return false;
        }
        let Some(operation) = self.validate_editorial(&request.operation) else {
            return false;
        };
        let named_record = match &operation {
            Op::Name(req) => req
                .record_id
                .as_deref()
                .and_then(|id| self.editorial_record(id))
                .cloned(),
            _ => None,
        };
        let doc = tree.document_mut();
        // A trait implementation can expose different documents for reading
        // and writing. Check the actual execution reference as well.
        if doc.pending_ops() != 0
            || doc.get_heads() != request.frontier
            || (self.host.history_groups_are_local()
                && !self
                    .host
                    .authorizes_local_history_document(doc, &request.room))
        {
            return false;
        }
        let changed = match operation {
            Op::Name(req) => {
                if req.label.trim().is_empty() {
                    req.record_id
                        .as_deref()
                        .is_some_and(|id| ui::history_groups::clear_name(doc, id))
                } else {
                    let (start, end) = named_record
                        .as_ref()
                        .map_or((req.start, Some(req.end)), |range| (range.start, range.end));
                    ui::history_groups::name_range(doc, &start, end.as_ref(), &req.label);
                    true
                }
            }
            Op::Split {
                record,
                left_end,
                right_start,
                label,
                right_label,
            } => ui::history_groups::split_named_range(
                doc,
                &record,
                &left_end,
                &right_start,
                &label,
                &right_label,
            )
            .is_some(),
            Op::Merge {
                records,
                start,
                end,
                label,
            } => ui::history_groups::merge_named_ranges(doc, &records, &start, Some(&end), &label)
                .is_some(),
            Op::MoveBoundary {
                left,
                right,
                left_end,
                right_start,
            } => ui::history_groups::materialize_and_move_boundary(
                doc,
                displayed(&left),
                displayed(&right),
                &left_end,
                &right_start,
            )
            .is_some(),
        };
        if !changed {
            return false;
        }
        tree.commit_activity_change();
        self.last_heads.clear();
        self.refresh(tree);
        true
    }

    fn editorial_bounds(
        &self,
        start: HistoryEntryId,
        end: HistoryEntryId,
    ) -> Option<(usize, usize)> {
        let start = self.entry_position(&start)?;
        let end = self.entry_position(&end)?;
        if start > end {
            return None;
        }
        let entries = self.timeline.entries().get(start..=end)?;
        if entries.iter().any(|entry| {
            self.external_entry
                .as_ref()
                .is_some_and(|external| external.id == entry.id)
                || self
                    .projected_origins
                    .get(&entry.id)
                    .is_some_and(|address| {
                        address.origin != super::super::index::HistoryOrigin::DesktopTree
                    })
        }) {
            return None;
        }
        Some((start, end))
    }

    fn editorial_record(&self, id: &str) -> Option<&NamedRange> {
        self.named.iter().find(|range| range.id == id)
    }

    fn validate_editorial(&self, op: &Op) -> Option<Op> {
        let label_valid = |s: &str| s.trim().chars().count() <= ui::history_groups::MAX_LABEL_LEN;
        match op {
            Op::Name(req) => {
                if !label_valid(&req.label) {
                    return None;
                }
                if let Some(id) = &req.record_id {
                    self.editorial_record(id)?;
                    // Clearing addresses the stored record, not its clipped band.
                    if !req.label.trim().is_empty() {
                        let (_, a, b) = self
                            .named_positions()
                            .into_iter()
                            .find(|(r, _, _)| r.id == *id)?;
                        if self.timeline.entries().get(a)?.id != req.start
                            || self.timeline.entries().get(b)?.id != req.end
                        {
                            return None;
                        }
                        self.editorial_bounds(req.start, req.end)?;
                    }
                } else {
                    self.editorial_bounds(req.start, req.end)?;
                    if req.label.trim().is_empty()
                        || self.named.iter().any(|r| r.start == req.start)
                    {
                        return None;
                    }
                }
            }
            Op::Split {
                record,
                left_end,
                right_start,
                label,
                right_label,
            } => {
                let (_, start, end) = self
                    .named_positions()
                    .into_iter()
                    .find(|(r, _, _)| r.id == *record)?;
                self.editorial_bounds(
                    self.timeline.entries().get(start)?.id,
                    self.timeline.entries().get(end)?.id,
                )?;
                let (left, right) = self.editorial_bounds(*left_end, *right_start)?;
                if left < start
                    || right > end
                    || left.checked_add(1)? != right
                    || !label_valid(label)
                    || !label_valid(right_label)
                    || label.trim().is_empty()
                    || right_label.trim().is_empty()
                    || self.named.iter().any(|r| r.start == *right_start)
                {
                    return None;
                }
            }
            Op::Merge {
                records,
                start,
                end,
                label,
            } => {
                if records.len() < 2 || !label_valid(label) || label.trim().is_empty() {
                    return None;
                }
                let mut next = self.editorial_bounds(*start, *end)?.0;
                let mut seen = std::collections::HashSet::new();
                for id in records {
                    if !seen.insert(id) {
                        return None;
                    }
                    let (_, a, b) = self
                        .named_positions()
                        .into_iter()
                        .find(|(r, _, _)| r.id == *id)?;
                    self.editorial_bounds(
                        self.timeline.entries().get(a)?.id,
                        self.timeline.entries().get(b)?.id,
                    )?;
                    if a != next {
                        return None;
                    }
                    next = b.checked_add(1)?;
                }
                if next != self.entry_position(end)?.checked_add(1)? {
                    return None;
                }
            }
            Op::MoveBoundary {
                left,
                right,
                left_end,
                right_start,
            } => {
                self.validate_boundary(left, right, *left_end, *right_start)?;
            }
        }
        Some(op.clone())
    }

    fn validate_boundary(
        &self,
        left: &EditorialSession,
        right: &EditorialSession,
        left_end: HistoryEntryId,
        right_start: HistoryEntryId,
    ) -> Option<()> {
        let label_valid = |s: &str| s.trim().chars().count() <= ui::history_groups::MAX_LABEL_LEN;
        let (a, b) = self.editorial_bounds(left.start, left.end)?;
        let (c, d) = self.editorial_bounds(right.start, right.end)?;
        let (new_b, new_c) = self.editorial_bounds(left_end, right_start)?;
        if b.checked_add(1)? != c
            || new_b.checked_add(1)? != new_c
            || new_b < a
            || new_c > d
            || (new_b == b && new_c == c)
        {
            return None;
        }
        for session in [left, right] {
            if !label_valid(&session.label) || session.label.trim().is_empty() {
                return None;
            }
            if let Some(id) = &session.record {
                let range = self.editorial_record(id)?;
                let (_, start, end) = self
                    .named_positions()
                    .into_iter()
                    .find(|(r, _, _)| r.id == *id)?;
                if self.timeline.entries().get(start)?.id != session.start
                    || self.timeline.entries().get(end)?.id != session.end
                    || range.label != session.label
                {
                    return None;
                }
            } else if self.named.iter().any(|r| r.start == session.start) {
                return None;
            }
        }
        if self
            .named
            .iter()
            .any(|r| r.start == right_start && Some(&r.id) != right.record.as_ref())
        {
            return None;
        }
        Some(())
    }
}

fn displayed(session: &EditorialSession) -> ui::history_groups::DisplayedSession<'_> {
    ui::history_groups::DisplayedSession {
        record: session.record.as_deref(),
        start: &session.start,
        end: &session.end,
        label: &session.label,
    }
}
