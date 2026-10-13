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

    /// Helpers retain stored anchors outside a clipped displayed band.
    /// Validate those independently before writing.
    fn editorial_record_bounds(&self, id: &str) -> Option<(usize, usize)> {
        let range = self.editorial_record(id)?;
        let end = match range.end {
            Some(end) => end,
            None => self.timeline.entries().last()?.id,
        };
        self.editorial_bounds(range.start, end)
    }

    fn displayed_record_bounds(&self, id: &str) -> Option<(usize, usize)> {
        let (_, start, end) = self
            .named_positions()
            .into_iter()
            .find(|(r, _, _)| r.id == id)?;
        Some((start, end))
    }

    fn validate_editorial(&self, op: &Op) -> Option<Op> {
        let label_valid = |s: &str| s.trim().chars().count() <= ui::history_groups::MAX_LABEL_LEN;
        match op {
            Op::Name(req) => {
                if !label_valid(&req.label) {
                    return None;
                }
                if let Some(id) = &req.record_id {
                    // Clearing addresses the stored record, not its clipped band.
                    if req.label.trim().is_empty() {
                        self.editorial_record(id)?;
                    } else {
                        let (a, b) = self.displayed_record_bounds(id)?;
                        if self.timeline.entries().get(a)?.id != req.start
                            || self.timeline.entries().get(b)?.id != req.end
                        {
                            return None;
                        }
                        self.editorial_bounds(req.start, req.end)?;
                        self.editorial_record_bounds(id)?;
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
                self.editorial_record_bounds(record)?;
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
                    self.editorial_record_bounds(id)?;
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
                self.editorial_record_bounds(id)?;
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
            } else {
                // None cannot invent a session: bounds and title must match
                // a current automatic projection from the canonical engine.
                let mut groups = Vec::new();
                for group in self.timeline.groups() {
                    group.walk(&mut groups);
                }
                let timeline_match = groups.into_iter().any(|group| {
                    group.record_id.is_none()
                        && group.first_entry == session.start
                        && group.last_entry == session.end
                        && group.title(self.timeline.entries()) == session.label
                        && self
                            .timeline
                            .entries()
                            .get(group.start..group.end)
                            .unwrap_or_default()
                            .iter()
                            .any(|entry| self.timeline.filter().matches(entry))
                });
                // Recap titles describe outcomes rather than timeline counts.
                let recap_match = self.view == super::ActivityView::CatchUp
                    && self.digest.briefs.iter().any(|brief| {
                        brief.work_session_id.is_none()
                            && brief.entry_ids.first() == Some(&session.start)
                            && brief.entry_ids.last() == Some(&session.end)
                            && brief.title == session.label
                            && brief.entry_ids.iter().any(|id| {
                                self.entry_position(id)
                                    .and_then(|position| self.timeline.entries().get(position))
                                    .is_some_and(|entry| self.timeline.filter().matches(entry))
                            })
                    });
                if !timeline_match && !recap_match {
                    return None;
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};
    use ui::widget::ArtifactKind;
    use ui::{EntryKind, HistoryAuthor, HistoryEntry};

    fn id(n: u8) -> HistoryEntryId {
        HistoryEntryId([n; 16])
    }

    fn pane() -> RoomActivityPane {
        let mut pane = RoomActivityPane::new();
        pane.timeline.set_entries(
            (0..6)
                .map(|n| HistoryEntry {
                    id: id(n),
                    author: HistoryAuthor::new("Ada", 210.0, "ada"),
                    entry_kind: EntryKind::Text,
                    artifact_kind: ArtifactKind::File,
                    timestamp: SystemTime::UNIX_EPOCH
                        + Duration::from_secs(u64::from(n) * 10 + if n >= 3 { 600 } else { 0 }),
                    summary: "Edited text".into(),
                    text_delta: None,
                    diff_payload: None,
                    redaction: None,
                    context: None,
                })
                .collect(),
        );
        pane
    }

    fn named(pane: &mut RoomActivityPane, ranges: &[(u8, Option<u8>)]) {
        pane.named = ranges
            .iter()
            .enumerate()
            .map(|(i, (start, end))| NamedRange {
                id: format!("record-{i}"),
                label: format!("Name {i}"),
                start: id(*start),
                end: end.map(id),
                named_by: None,
                created: SystemTime::UNIX_EPOCH,
            })
            .collect();
        pane.timeline.set_named_ranges(pane.named.clone());
    }

    fn rename(start: u8, end: u8, label: &str) -> Op {
        Op::Name(ui::GroupNameRequest {
            record_id: Some("record-1".into()),
            start: id(start),
            end: id(end),
            label: label.into(),
        })
    }

    #[test]
    fn editorial_canonical_overlap_clipping_and_stored_anchors() {
        let mut pane = pane();
        named(&mut pane, &[(0, Some(3)), (2, Some(5))]);
        assert!(pane.validate_editorial(&rename(4, 5, "Renamed")).is_some());
        assert!(pane.validate_editorial(&rename(2, 5, "Renamed")).is_none());
        assert!(
            pane.validate_editorial(&Op::Split {
                record: "record-1".into(),
                left_end: id(4),
                right_start: id(5),
                label: "Left".into(),
                right_label: "Right".into(),
            })
            .is_some()
        );
        assert!(
            pane.validate_editorial(&Op::Merge {
                records: vec!["record-0".into(), "record-1".into()],
                start: id(0),
                end: id(5),
                label: "Merged".into(),
            })
            .is_some()
        );
        let left = EditorialSession {
            record: Some("record-0".into()),
            start: id(0),
            end: id(3),
            label: "Name 0".into(),
        };
        let right = EditorialSession {
            record: Some("record-1".into()),
            start: id(4),
            end: id(5),
            label: "Name 1".into(),
        };
        assert!(
            pane.validate_boundary(&left, &right, id(2), id(3))
                .is_some()
        );
        // The stored start is retained by rename/split even though clipped.
        pane.external_entry = pane.timeline.entries().get(2).cloned();
        assert!(pane.validate_editorial(&rename(4, 5, "Renamed")).is_none());
        assert!(pane.validate_editorial(&rename(4, 5, "")).is_some());
        pane.external_entry = None;
        pane.timeline
            .set_filter(ui::widget::history::HistoryFilter {
                authors: vec!["Nobody".into()],
                ..Default::default()
            });
        assert!(pane.validate_editorial(&rename(4, 5, "Renamed")).is_none());
    }

    #[test]
    fn editorial_reversed_record_is_not_a_displayed_band_and_open_clear_survives() {
        let mut pane = pane();
        named(&mut pane, &[(0, Some(1)), (5, Some(2))]);
        assert_eq!(pane.named_positions().len(), 1);
        assert!(pane.validate_editorial(&rename(2, 5, "Renamed")).is_none());
        assert!(
            pane.validate_editorial(&Op::Split {
                record: "record-1".into(),
                left_end: id(3),
                right_start: id(4),
                label: "Left".into(),
                right_label: "Right".into(),
            })
            .is_none()
        );
        named(&mut pane, &[(0, Some(1)), (2, None)]);
        assert!(pane.validate_editorial(&rename(2, 5, "Renamed")).is_some());
        assert_eq!(pane.editorial_record("record-1").unwrap().end, None);
        assert!(pane.validate_editorial(&rename(0, 0, "")).is_some());
    }

    #[test]
    fn editorial_automatic_boundary_requires_exact_groups_and_titles() {
        let mut pane = pane();
        let sessions: Vec<_> = pane
            .timeline
            .groups()
            .iter()
            .map(|group| EditorialSession {
                record: group.record_id.clone(),
                start: group.first_entry,
                end: group.last_entry,
                label: group.title(pane.timeline.entries()),
            })
            .collect();
        assert_eq!(sessions.len(), 2);
        assert!(
            pane.validate_boundary(&sessions[0], &sessions[1], id(3), id(4))
                .is_some()
        );
        let mut invented = sessions[0].clone();
        invented.label = "Invented title".into();
        assert!(
            pane.validate_boundary(&invented, &sessions[1], id(3), id(4))
                .is_none()
        );
        invented = sessions[0].clone();
        invented.end = id(1);
        let mut right = sessions[1].clone();
        right.start = id(2);
        assert!(
            pane.validate_boundary(&invented, &right, id(2), id(3))
                .is_none()
        );
        pane.timeline
            .set_filter(ui::widget::history::HistoryFilter {
                authors: vec!["Nobody".into()],
                ..Default::default()
            });
        assert!(
            pane.validate_boundary(&sessions[0], &sessions[1], id(3), id(4))
                .is_none()
        );
    }
}
