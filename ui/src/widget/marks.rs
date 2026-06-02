//! Inline marks model used by [`RichTextLine`].
//!
//! A `MarkSpan` is a half-open byte range `[start, end)` over a
//! [`RichTextLine`]'s text plus a [`Mark`] tag. Spans are stored
//! sorted by `start` (and non-overlapping when possible). Multiple
//! marks may cover the same byte range — bold + italic on the same
//! word produces two `MarkSpan`s, not one fused `MarkSpan { mark:
//! BoldItalic, .. }`.
//!
//! ## Overlapping marks
//!
//! v0.1: **two marks of different kinds may share a range** (so a
//! word can be both bold and italic), but **two marks of the same
//! kind never overlap**. The mutation helpers ([`toggle_mark_range`])
//! enforce this by merging touching same-kind spans and removing
//! same-kind spans that get fully covered by the toggle range.
//!
//! ## Nested markdown — out of scope for v0.1
//!
//! The serialiser emits `**bold *italic***` etc.; the parser accepts
//! the same. But the model treats bold + italic as independent marks
//! — we don't materialise a "bold-italic" enum variant. This is
//! enough for round-trip stability on documents we produce.

use crate::widget::block_editor::BlockKind;
use std::ops::Range;

use uuid::Uuid;

/// Stable identifier for a page link target.
///
/// `RichTextLine` doesn't own page identity — that's the notes app's
/// concern — so we use a UUID here. The notes app's `PageId` is itself
/// a `uuid::Uuid`, so a `Mark::Link(PageId)` round-trips through the
/// markdown layer.
pub type PageId = Uuid;

/// One inline mark applied to a byte range of a [`RichTextLine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    /// **Bold** — renders as `TextWeight::Bold`.
    Bold,
    /// *Italic* — renders with cosmic-text's italic style.
    Italic,
    /// `Inline code` — renders in monospace, accent-coloured.
    Code,
    /// `[[Page Title]]` — renders accent-coloured + underlined.
    /// Clicking navigates to the page identified by the embedded id.
    Link(PageId),
}

impl Mark {
    /// True iff `other` is the same *kind* of mark (regardless of
    /// payload). Used by mutation helpers to fold overlapping
    /// same-kind spans without conflating Link(a) with Link(b).
    pub fn same_kind(&self, other: &Mark) -> bool {
        matches!(
            (self, other),
            (Mark::Bold, Mark::Bold)
                | (Mark::Italic, Mark::Italic)
                | (Mark::Code, Mark::Code)
                | (Mark::Link(_), Mark::Link(_))
        )
    }
}

/// A byte range + a single [`Mark`]. Multiple `MarkSpan`s with the
/// same kind never overlap (the toggle helpers merge them); different
/// kinds may share a range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkSpan {
    pub range: Range<usize>,
    pub mark: Mark,
}

impl MarkSpan {
    pub fn new(range: Range<usize>, mark: Mark) -> Self {
        Self { range, mark }
    }
}

/// Result of `toggle_mark_range`: did the toggle add or remove the
/// mark on this range? Returned so the caller can update any pending
/// "next character will be marked" state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToggleOutcome {
    Added,
    Removed,
    /// The range was empty — toggle didn't touch the span list.
    NoOp,
}

/// True iff every byte in `range` (a non-empty sub-range of the line)
/// is covered by at least one same-kind span. Used to decide whether
/// a toggle should add or remove the mark.
pub fn range_fully_covered(spans: &[MarkSpan], range: &Range<usize>, mark: &Mark) -> bool {
    if range.start >= range.end {
        return false;
    }
    // Walk same-kind spans sorted by start; track the rightmost end of
    // contiguous coverage. If at any point our needle's `current`
    // byte falls outside that coverage, return false.
    let mut covered = range.start;
    let mut sorted: Vec<&Range<usize>> = spans
        .iter()
        .filter(|s| s.mark.same_kind(mark))
        .map(|s| &s.range)
        .collect();
    sorted.sort_by_key(|r| r.start);
    for r in sorted {
        if r.start > covered {
            return false;
        }
        if r.end > covered {
            covered = r.end;
        }
        if covered >= range.end {
            return true;
        }
    }
    covered >= range.end
}

/// Apply a toggle of `mark` over `range`:
///
/// - If `range` is fully covered by same-kind spans → remove the
///   coverage (clip / split existing same-kind spans).
/// - Otherwise → add a new span and merge touching same-kind spans
///   into one contiguous run.
pub fn toggle_mark_range(
    spans: &mut Vec<MarkSpan>,
    range: Range<usize>,
    mark: Mark,
) -> ToggleOutcome {
    if range.start >= range.end {
        return ToggleOutcome::NoOp;
    }
    if range_fully_covered(spans, &range, &mark) {
        remove_mark_from_range(spans, range, &mark);
        ToggleOutcome::Removed
    } else {
        add_mark_to_range(spans, range, mark);
        ToggleOutcome::Added
    }
}

/// Remove every same-kind coverage from `range`. Spans that straddle
/// either edge of `range` get clipped or split.
pub fn remove_mark_from_range(spans: &mut Vec<MarkSpan>, range: Range<usize>, mark: &Mark) {
    let mut out: Vec<MarkSpan> = Vec::with_capacity(spans.len() + 2);
    for s in spans.drain(..) {
        if !s.mark.same_kind(mark) {
            out.push(s);
            continue;
        }
        if s.range.end <= range.start || s.range.start >= range.end {
            // No overlap.
            out.push(s);
            continue;
        }
        // Left fragment.
        if s.range.start < range.start {
            out.push(MarkSpan::new(s.range.start..range.start, s.mark.clone()));
        }
        // Right fragment.
        if s.range.end > range.end {
            out.push(MarkSpan::new(range.end..s.range.end, s.mark.clone()));
        }
        // Middle is dropped.
    }
    out.sort_by_key(|s| s.range.start);
    *spans = out;
}

/// Add a same-kind span covering `range` and merge any touching /
/// overlapping same-kind spans into a single run.
pub fn add_mark_to_range(spans: &mut Vec<MarkSpan>, range: Range<usize>, mark: Mark) {
    let mut start = range.start;
    let mut end = range.end;
    let mut out: Vec<MarkSpan> = Vec::with_capacity(spans.len() + 1);
    for s in spans.drain(..) {
        if !s.mark.same_kind(&mark) {
            out.push(s);
            continue;
        }
        // Overlapping or touching: merge.
        if s.range.end >= start && s.range.start <= end {
            start = start.min(s.range.start);
            end = end.max(s.range.end);
        } else {
            out.push(s);
        }
    }
    // Preserve payload of Link mark from any merged span if the
    // toggle is adding the same kind (e.g. dragging a link extension
    // shouldn't change the target). For simplicity, the new payload
    // wins. Most callers pass the same Link payload anyway.
    out.push(MarkSpan::new(start..end, mark));
    out.sort_by_key(|s| s.range.start);
    *spans = out;
}

/// Adjust every span's range by a splice at byte `at`: inserting
/// `inserted_len` bytes (positive) or removing `removed_len` bytes.
/// Used to keep marks aligned after a user edit.
///
/// Semantics:
/// - Insertion at `at`: spans entirely before `at` are untouched.
///   Spans entirely at or after `at` are shifted by `inserted_len`.
///   Spans straddling `at` are extended on the right.
/// - Deletion of `[at, at+removed_len)`: spans entirely before `at`
///   untouched. Spans entirely inside the deleted region are dropped.
///   Spans straddling are clipped.
pub fn shift_after_edit(spans: &mut Vec<MarkSpan>, at: usize, inserted_len: usize, removed_len: usize) {
    if inserted_len == 0 && removed_len == 0 {
        return;
    }
    let mut out: Vec<MarkSpan> = Vec::with_capacity(spans.len());
    let removed_end = at + removed_len;
    for s in spans.drain(..) {
        let mut start = s.range.start;
        let mut end = s.range.end;

        if removed_len > 0 {
            // Clip the span against [at, removed_end).
            if start >= removed_end {
                start -= removed_len;
                end -= removed_len;
            } else if end <= at {
                // entirely before — no change.
            } else {
                // overlaps deletion: figure new span as the union of
                // the portions before `at` and after `removed_end`.
                let before = if s.range.start < at {
                    Some(s.range.start..at.min(s.range.end))
                } else {
                    None
                };
                let after = if s.range.end > removed_end {
                    Some(removed_end.max(s.range.start)..s.range.end)
                } else {
                    None
                };
                match (before, after) {
                    (Some(b), Some(a)) => {
                        // Glue the two halves: a mark that spans
                        // across a deletion stays one mark. (Notion
                        // / Word / Quill behaviour — deleting glue
                        // between two bold halves yields one bold
                        // word, not two.)
                        let a_start = a.start - removed_len;
                        let a_end = a.end - removed_len;
                        out.push(MarkSpan::new(b.start..a_end.max(b.end), s.mark.clone()));
                        let _ = a_start; // kept for symmetry
                        continue;
                    }
                    (Some(b), None) => {
                        start = b.start;
                        end = b.end;
                    }
                    (None, Some(a)) => {
                        start = a.start - removed_len;
                        end = a.end - removed_len;
                    }
                    (None, None) => {
                        // entirely within deletion
                        continue;
                    }
                }
            }
        }

        if inserted_len > 0 {
            if start >= at {
                start += inserted_len;
                end += inserted_len;
            } else if end > at {
                // straddle — extend right
                end += inserted_len;
            }
        }

        if end > start {
            out.push(MarkSpan::new(start..end, s.mark.clone()));
        }
    }
    out.sort_by_key(|s| s.range.start);
    *spans = out;
}

/// Partition `text` into contiguous segments [start, end) tagged with
/// the set of marks active inside that segment. Marks of different
/// kinds may co-occur. Useful for the render path: one `draw_text`
/// call per segment with the appropriate styling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkSegment {
    pub range: Range<usize>,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: Option<PageId>,
}

impl MarkSegment {
    pub fn plain(range: Range<usize>) -> Self {
        Self {
            range,
            bold: false,
            italic: false,
            code: false,
            link: None,
        }
    }
}

/// Produce the partition. Boundaries: every span's `start` and `end`,
/// plus 0 and `text_len`. The resulting segments together cover
/// `[0, text_len)`.
pub fn segments_for(text_len: usize, spans: &[MarkSpan]) -> Vec<MarkSegment> {
    if text_len == 0 {
        return Vec::new();
    }
    let mut boundaries: Vec<usize> = Vec::with_capacity(spans.len() * 2 + 2);
    boundaries.push(0);
    boundaries.push(text_len);
    for s in spans {
        if s.range.start <= text_len {
            boundaries.push(s.range.start);
        }
        if s.range.end <= text_len {
            boundaries.push(s.range.end);
        }
    }
    boundaries.sort();
    boundaries.dedup();
    let mut out = Vec::with_capacity(boundaries.len());
    for w in boundaries.windows(2) {
        let start = w[0];
        let end = w[1];
        if start >= end {
            continue;
        }
        let mid = start; // any byte inside; same set of marks covers the whole segment
        let mut seg = MarkSegment::plain(start..end);
        for s in spans {
            if s.range.start <= mid && s.range.end >= end {
                match &s.mark {
                    Mark::Bold => seg.bold = true,
                    Mark::Italic => seg.italic = true,
                    Mark::Code => seg.code = true,
                    Mark::Link(id) => seg.link = Some(*id),
                }
            }
        }
        out.push(seg);
    }
    out
}

/// Built-in block kind awareness — keeps the rendering helpers
/// generic over which block they live in. (Currently unused, but
/// shapes the door for future block-specific mark filtering, e.g.
/// "code blocks render the whole line in monospace, so don't paint
/// the Mark::Code chrome on top".)
#[allow(dead_code)]
pub(crate) fn marks_render_inline(kind: &BlockKind) -> bool {
    !matches!(kind, BlockKind::CodeBlock { .. } | BlockKind::Divider)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> PageId {
        PageId::from_u128(n)
    }

    #[test]
    fn add_mark_inserts_span() {
        let mut spans = Vec::new();
        add_mark_to_range(&mut spans, 0..5, Mark::Bold);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].range, 0..5);
    }

    #[test]
    fn add_mark_merges_touching_same_kind() {
        let mut spans = vec![MarkSpan::new(0..3, Mark::Bold)];
        add_mark_to_range(&mut spans, 3..6, Mark::Bold);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].range, 0..6);
    }

    #[test]
    fn add_mark_keeps_different_kinds_separate() {
        let mut spans = vec![MarkSpan::new(0..5, Mark::Bold)];
        add_mark_to_range(&mut spans, 0..5, Mark::Italic);
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn remove_mark_clips_straddle() {
        let mut spans = vec![MarkSpan::new(0..10, Mark::Bold)];
        remove_mark_from_range(&mut spans, 3..6, &Mark::Bold);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].range, 0..3);
        assert_eq!(spans[1].range, 6..10);
    }

    #[test]
    fn range_fully_covered_simple() {
        let spans = vec![MarkSpan::new(0..5, Mark::Bold)];
        assert!(range_fully_covered(&spans, &(0..5), &Mark::Bold));
        assert!(range_fully_covered(&spans, &(1..4), &Mark::Bold));
        assert!(!range_fully_covered(&spans, &(0..6), &Mark::Bold));
        assert!(!range_fully_covered(&spans, &(0..5), &Mark::Italic));
    }

    #[test]
    fn range_fully_covered_two_adjacent_spans() {
        let spans = vec![
            MarkSpan::new(0..3, Mark::Bold),
            MarkSpan::new(3..6, Mark::Bold),
        ];
        assert!(range_fully_covered(&spans, &(0..6), &Mark::Bold));
    }

    #[test]
    fn toggle_adds_when_uncovered() {
        let mut spans = Vec::new();
        let outcome = toggle_mark_range(&mut spans, 0..5, Mark::Bold);
        assert_eq!(outcome, ToggleOutcome::Added);
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn toggle_removes_when_fully_covered() {
        let mut spans = vec![MarkSpan::new(0..5, Mark::Bold)];
        let outcome = toggle_mark_range(&mut spans, 0..5, Mark::Bold);
        assert_eq!(outcome, ToggleOutcome::Removed);
        assert!(spans.is_empty());
    }

    #[test]
    fn toggle_partial_range_adds_then_merges() {
        let mut spans = vec![MarkSpan::new(0..5, Mark::Bold)];
        let outcome = toggle_mark_range(&mut spans, 3..10, Mark::Bold);
        assert_eq!(outcome, ToggleOutcome::Added);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].range, 0..10);
    }

    #[test]
    fn shift_after_insert_after_span() {
        let mut spans = vec![MarkSpan::new(0..3, Mark::Bold)];
        shift_after_edit(&mut spans, 5, 2, 0);
        assert_eq!(spans[0].range, 0..3); // unaffected
    }

    #[test]
    fn shift_after_insert_inside_span_extends_right() {
        let mut spans = vec![MarkSpan::new(0..5, Mark::Bold)];
        shift_after_edit(&mut spans, 2, 3, 0);
        assert_eq!(spans[0].range, 0..8);
    }

    #[test]
    fn shift_after_insert_at_start_pushes_right() {
        let mut spans = vec![MarkSpan::new(2..5, Mark::Bold)];
        shift_after_edit(&mut spans, 0, 1, 0);
        assert_eq!(spans[0].range, 3..6);
    }

    #[test]
    fn shift_after_delete_inside_span_clips() {
        let mut spans = vec![MarkSpan::new(0..10, Mark::Bold)];
        shift_after_edit(&mut spans, 2, 0, 3);
        assert_eq!(spans[0].range, 0..7);
    }

    #[test]
    fn shift_after_delete_drops_engulfed_span() {
        let mut spans = vec![MarkSpan::new(2..4, Mark::Bold)];
        shift_after_edit(&mut spans, 0, 0, 10);
        assert!(spans.is_empty());
    }

    #[test]
    fn segments_split_at_span_boundaries() {
        let spans = vec![MarkSpan::new(3..6, Mark::Bold)];
        let segs = segments_for(10, &spans);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].range, 0..3);
        assert!(!segs[0].bold);
        assert_eq!(segs[1].range, 3..6);
        assert!(segs[1].bold);
        assert_eq!(segs[2].range, 6..10);
        assert!(!segs[2].bold);
    }

    #[test]
    fn segments_combine_overlapping_marks() {
        let spans = vec![
            MarkSpan::new(0..5, Mark::Bold),
            MarkSpan::new(0..5, Mark::Italic),
        ];
        let segs = segments_for(5, &spans);
        assert_eq!(segs.len(), 1);
        assert!(segs[0].bold);
        assert!(segs[0].italic);
    }

    #[test]
    fn link_mark_carries_id() {
        let i = id(42);
        let spans = vec![MarkSpan::new(0..5, Mark::Link(i))];
        let segs = segments_for(5, &spans);
        assert_eq!(segs[0].link, Some(i));
    }

    #[test]
    fn segments_for_empty_text_is_empty() {
        assert!(segments_for(0, &[]).is_empty());
    }

    #[test]
    fn segments_for_no_spans_is_one_plain_segment() {
        let segs = segments_for(7, &[]);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].range, 0..7);
        assert!(!segs[0].bold);
    }
}
