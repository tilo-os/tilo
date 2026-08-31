//! Format-neutral, borrowed views used while exporting rich text.
//!
//! [`InlinePlan`] turns the canonical mark spans for one block into deterministic
//! semantic boundaries and runs.  It deliberately knows nothing about XML,
//! package relationships, escaping, or the nesting rules of a particular file
//! format.  Exporters remain responsible for those representation choices.

use crate::document::MarkSpan;
use crate::snapshot::ChangeKind;
use std::collections::BTreeSet;

/// The values whose ranges end and start at one character offset.
///
/// Ends and starts are kept separately because office formats require adjacent
/// ranges to close before the next range opens. Values are sorted but not
/// deduplicated, preserving distinct canonical marks with the same value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RangeBoundary<'a> {
    pub ends: Vec<&'a str>,
    pub starts: Vec<&'a str>,
}

/// All semantic range boundaries at a character offset.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InlineBoundary<'a> {
    pub comments: RangeBoundary<'a>,
    pub bookmarks: RangeBoundary<'a>,
    pub changes: RangeBoundary<'a>,
}

impl InlineBoundary<'_> {
    pub fn is_empty(&self) -> bool {
        self.comments.ends.is_empty()
            && self.comments.starts.is_empty()
            && self.bookmarks.ends.is_empty()
            && self.bookmarks.starts.is_empty()
            && self.changes.ends.is_empty()
            && self.changes.starts.is_empty()
    }
}

/// The tracked change covering a run. Insertion takes precedence if malformed
/// input contains overlapping insertion and deletion marks, matching the
/// canonical DOCX export behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveChange<'a> {
    pub kind: ChangeKind,
    pub id: &'a str,
}

/// A maximal run between semantic or mark boundaries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineRun<'a> {
    /// Half-open range in Unicode scalar-value (character) offsets.
    pub char_range: std::ops::Range<usize>,
    /// Borrowed UTF-8 text for `char_range`.
    pub text: &'a str,
    /// Every mark active over the run, sorted stably by mark name.
    pub marks: Vec<&'a MarkSpan>,
    /// The active `link` value, whether external or an internal `#target`.
    pub link: Option<&'a str>,
    /// The active tracked change, if any.
    pub change: Option<ActiveChange<'a>>,
    /// Note id when this is the single object-replacement placeholder covered
    /// by an exact `note-ref` mark.
    pub note_reference: Option<&'a str>,
}

impl<'a> InlineRun<'a> {
    /// Internal cross-reference target (`#name` -> `name`), if this run has one.
    pub fn cross_reference(&self) -> Option<&'a str> {
        self.link
            .and_then(|href| href.strip_prefix(crate::schema::CROSS_REF_PREFIX))
    }
}

/// Boundaries followed by the run beginning at the same character offset.
/// `run` is `None` for the terminal point at `text.chars().count()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlinePoint<'a> {
    pub offset: usize,
    pub boundary: InlineBoundary<'a>,
    pub run: Option<InlineRun<'a>>,
}

/// Borrowed, format-neutral inline export plan for one block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlinePlan<'a> {
    points: Vec<InlinePoint<'a>>,
}

impl<'a> InlinePlan<'a> {
    pub fn new(text: &'a str, marks: &'a [MarkSpan]) -> Self {
        let mut byte_offsets: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        byte_offsets.push(text.len());
        let len = byte_offsets.len().saturating_sub(1);

        let mut offsets = BTreeSet::from([0, len]);
        for mark in marks {
            if mark.start <= len {
                offsets.insert(mark.start);
            }
            if mark.end <= len {
                offsets.insert(mark.end);
            }
        }
        let offsets: Vec<usize> = offsets.into_iter().collect();
        let mut points = Vec::with_capacity(offsets.len());

        for (index, &offset) in offsets.iter().enumerate() {
            let boundary = InlineBoundary {
                comments: range_boundary(marks, crate::schema::MARK_COMMENT, offset),
                bookmarks: range_boundary(marks, crate::schema::MARK_BOOKMARK, offset),
                changes: change_boundary(marks, offset),
            };
            let run = offsets.get(index.saturating_add(1)).map(|&end| {
                let mut active: Vec<&MarkSpan> = marks
                    .iter()
                    .filter(|mark| offset >= mark.start && offset < mark.end)
                    .collect();
                // Stable sorting preserves canonical order among same-name marks.
                active.sort_by(|a, b| a.name.cmp(&b.name));
                let link = active
                    .iter()
                    .find(|mark| mark.name == crate::schema::MARK_LINK)
                    .and_then(|mark| mark.value.as_str());
                let find_change = |name: &str, kind| {
                    active
                        .iter()
                        .find(|mark| mark.name == name)
                        .and_then(|mark| mark.value.as_str())
                        .map(|id| ActiveChange { kind, id })
                };
                let change = find_change(crate::schema::MARK_INS, ChangeKind::Ins)
                    .or_else(|| find_change(crate::schema::MARK_DEL, ChangeKind::Del));
                let run_text = byte_offsets
                    .get(offset)
                    .zip(byte_offsets.get(end))
                    .and_then(|(&start_b, &end_b)| text.get(start_b..end_b))
                    .unwrap_or_default();
                let note_reference = if end == offset.saturating_add(1)
                    && run_text.starts_with(crate::schema::FIELD_PLACEHOLDER)
                {
                    marks
                        .iter()
                        .find(|mark| {
                            mark.name == crate::schema::MARK_NOTE_REF
                                && mark.start == offset
                                && mark.end == end
                        })
                        .and_then(|mark| mark.value.as_str())
                } else {
                    None
                };
                InlineRun {
                    char_range: offset..end,
                    text: run_text,
                    marks: active,
                    link,
                    change,
                    note_reference,
                }
            });
            points.push(InlinePoint {
                offset,
                boundary,
                run,
            });
        }
        Self { points }
    }

    pub fn points(&self) -> &[InlinePoint<'a>] {
        &self.points
    }
}

fn range_boundary<'a>(marks: &'a [MarkSpan], name: &str, offset: usize) -> RangeBoundary<'a> {
    let values = |at_start: bool| {
        let mut values: Vec<&str> = marks
            .iter()
            .filter(|mark| {
                mark.name == name
                    && if at_start {
                        mark.start == offset
                    } else {
                        mark.end == offset
                    }
            })
            .filter_map(|mark| mark.value.as_str())
            .collect();
        values.sort_unstable();
        values
    };
    RangeBoundary {
        ends: values(false),
        starts: values(true),
    }
}

fn change_boundary(marks: &[MarkSpan], offset: usize) -> RangeBoundary<'_> {
    let values = |at_start: bool| {
        let mut values: Vec<&str> = marks
            .iter()
            .filter(|mark| {
                (mark.name == crate::schema::MARK_INS || mark.name == crate::schema::MARK_DEL)
                    && if at_start {
                        mark.start == offset
                    } else {
                        mark.end == offset
                    }
            })
            .filter_map(|mark| mark.value.as_str())
            .collect();
        values.sort_unstable();
        values
    };
    RangeBoundary {
        ends: values(false),
        starts: values(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::MarkValue;

    fn mark(start: usize, end: usize, name: &str, value: MarkValue) -> MarkSpan {
        MarkSpan {
            start,
            end,
            name: name.into(),
            value,
        }
    }

    #[test]
    fn plans_unicode_by_character_and_borrows_utf8_runs() {
        let marks = vec![mark(1, 3, "strong", MarkValue::Bool(true))];
        let plan = InlinePlan::new("aé🦀z", &marks);
        let runs: Vec<_> = plan
            .points()
            .iter()
            .filter_map(|point| point.run.as_ref())
            .collect();
        assert_eq!(runs.len(), 3);
        assert_eq!((runs[0].char_range.clone(), runs[0].text), (0..1, "a"));
        assert_eq!((runs[1].char_range.clone(), runs[1].text), (1..3, "é🦀"));
        assert_eq!((runs[2].char_range.clone(), runs[2].text), (3..4, "z"));
        assert!(std::ptr::eq(
            runs[1].marks[0],
            std::ptr::from_ref(&marks[0])
        ));
    }

    #[test]
    fn orders_adjacent_and_overlapping_semantic_boundaries_deterministically() {
        let marks = vec![
            mark(
                0,
                2,
                crate::schema::MARK_COMMENT,
                MarkValue::Str("z".into()),
            ),
            mark(
                0,
                2,
                crate::schema::MARK_COMMENT,
                MarkValue::Str("a".into()),
            ),
            mark(
                2,
                4,
                crate::schema::MARK_COMMENT,
                MarkValue::Str("next".into()),
            ),
            mark(
                1,
                2,
                crate::schema::MARK_BOOKMARK,
                MarkValue::Str("bm".into()),
            ),
            mark(
                2,
                3,
                crate::schema::MARK_INS,
                MarkValue::Str("change".into()),
            ),
        ];
        let plan = InlinePlan::new("test", &marks);
        let at_two = plan
            .points()
            .iter()
            .find(|point| point.offset == 2)
            .unwrap();
        assert_eq!(at_two.boundary.comments.ends, ["a", "z"]);
        assert_eq!(at_two.boundary.comments.starts, ["next"]);
        assert_eq!(at_two.boundary.bookmarks.ends, ["bm"]);
        assert_eq!(at_two.boundary.changes.starts, ["change"]);
    }

    #[test]
    fn exposes_links_changes_marks_and_internal_targets_per_maximal_run() {
        let marks = vec![
            mark(
                0,
                4,
                crate::schema::MARK_LINK,
                MarkValue::Str("#target".into()),
            ),
            mark(
                1,
                3,
                crate::schema::MARK_DEL,
                MarkValue::Str("deleted".into()),
            ),
            mark(1, 3, "em", MarkValue::Bool(true)),
            mark(
                1,
                3,
                crate::schema::MARK_INS,
                MarkValue::Str("inserted".into()),
            ),
        ];
        let plan = InlinePlan::new("abcd", &marks);
        let runs: Vec<_> = plan
            .points()
            .iter()
            .filter_map(|p| p.run.as_ref())
            .collect();
        assert_eq!(
            runs.iter().map(|r| r.text).collect::<Vec<_>>(),
            ["a", "bc", "d"]
        );
        assert_eq!(runs[1].cross_reference(), Some("target"));
        assert_eq!(
            runs[1].change,
            Some(ActiveChange {
                kind: ChangeKind::Ins,
                id: "inserted"
            })
        );
        assert_eq!(
            runs[1]
                .marks
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            ["del", "em", "ins", "link"]
        );
    }

    #[test]
    fn recognizes_only_exact_note_placeholders_and_keeps_terminal_boundaries() {
        let p = crate::schema::FIELD_PLACEHOLDER;
        let text = format!("x{p}y{p}");
        let marks = vec![
            mark(
                1,
                2,
                crate::schema::MARK_NOTE_REF,
                MarkValue::Str("note".into()),
            ),
            mark(
                3,
                5,
                crate::schema::MARK_NOTE_REF,
                MarkValue::Str("missing-shape".into()),
            ),
            mark(
                0,
                4,
                crate::schema::MARK_BOOKMARK,
                MarkValue::Str("all".into()),
            ),
        ];
        let plan = InlinePlan::new(&text, &marks);
        let notes: Vec<_> = plan
            .points()
            .iter()
            .filter_map(|p| p.run.as_ref())
            .map(|r| r.note_reference)
            .collect();
        assert_eq!(notes, [None, Some("note"), None, None]);
        let terminal = plan.points().last().unwrap();
        assert_eq!(terminal.offset, 4);
        assert!(terminal.run.is_none());
        assert_eq!(terminal.boundary.bookmarks.ends, ["all"]);
    }

    #[test]
    fn ignores_out_of_range_boundaries_without_clamping_them_into_output() {
        let marks = vec![mark(
            8,
            9,
            crate::schema::MARK_COMMENT,
            MarkValue::Str("bad".into()),
        )];
        let plan = InlinePlan::new("hi", &marks);
        assert_eq!(plan.points().len(), 2);
        assert!(plan.points().iter().all(|p| p.boundary.is_empty()));
    }
}
