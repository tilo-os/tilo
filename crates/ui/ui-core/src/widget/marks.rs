//! Inline marks model used by
//! [`RichTextLine`](crate::widget::rich_text_line::RichTextLine).
//!
//! Two marks of different kinds may share a range, but two marks of the same kind never
//! overlap — the mutation helpers ([`toggle_mark_range`]) enforce this by merging touching
//! same-kind spans and removing same-kind spans fully covered by the toggle range.

use std::ops::Range;
use tilo_sort::CompactSort as _;

use uuid::Uuid;

use crate::richtext::{BaselineShift, FontFamily};

/// Stable identifier for a page link target. `RichTextLine` doesn't own page
/// identity — that's the notes app's concern — so a UUID is used here; the
/// notes app's `PageId` is itself a `uuid::Uuid`, so `Mark::Link(PageId)`
/// round-trips through the markdown layer.
pub type PageId = Uuid;

/// One inline mark applied to a byte range of a
/// [`RichTextLine`](crate::widget::rich_text_line::RichTextLine).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    Bold,
    Italic,
    Code,
    Underline,
    Strikethrough,
    Link(PageId),
    Transclusion(PageId),
    Embed(String),
    ExternalLink(String),
    ResourceLink(String),
    /// A member named in the text (`@Ada`).
    Mention(tilo_actions::MemberId),
    /// A record named in the text, by the value its reference is stored as
    /// ([`tilo_reference::Embed::mark_value`]).
    Reference(String),
    /// Raised text, as `x²` writes its exponent.
    Superscript,
    /// Lowered text, as `H₂O` writes its subscript.
    Subscript,
    /// The run's own size in points, overriding the block's.
    Size(u16),
    /// The run's own face, overriding the block's.
    Font(FontFamily),
    /// A run someone inserted, tracked as a change: the id of the change as the document keeps
    /// it, so accepting takes the mark and rejecting takes the words.
    Insertion(String),
    /// A run someone deleted, tracked as a change: the words stay until the change is accepted.
    Deletion(String),
    /// The run's own ink, as RGB.
    Color([u8; 3]),
    /// A band behind the run, as RGB: a highlighter's mark.
    Highlight([u8; 3]),
    /// A run a comment is anchored to, by the comment's id: the editor marks the range and
    /// points at the thread from the margin.
    Comment(String),
    /// A field drawn at layout time, by the kind of field it is (a table of contents): the run
    /// is the field's placeholder, never its drawn text.
    Field(String),
}

/// How a mention of one member is painted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MentionColors {
    pub ink: vello::peniko::Color,
    pub band: Option<vello::peniko::Color>,
}

/// The colours a mention of the member with this id is painted in, supplied by the
/// owning model (it knows who the member is).
pub type MentionPaint = std::rc::Rc<dyn Fn(&str) -> MentionColors>;

impl Mark {
    /// True iff `other` is the same *kind* of mark, regardless of payload
    /// (so `Link(a)` and `Link(b)` are not conflated).
    pub fn same_kind(&self, other: &Mark) -> bool {
        matches!(
            (self, other),
            (Mark::Bold, Mark::Bold)
                | (Mark::Italic, Mark::Italic)
                | (Mark::Code, Mark::Code)
                | (Mark::Underline, Mark::Underline)
                | (Mark::Strikethrough, Mark::Strikethrough)
                | (Mark::Link(_), Mark::Link(_))
                | (Mark::Transclusion(_), Mark::Transclusion(_))
                | (Mark::Embed(_), Mark::Embed(_))
                | (Mark::ExternalLink(_), Mark::ExternalLink(_))
                | (Mark::ResourceLink(_), Mark::ResourceLink(_))
                | (
                    Mark::Mention(_) | Mark::Reference(_),
                    Mark::Mention(_) | Mark::Reference(_)
                )
                // Raised and lowered text are one kind: a stretch is one or the
                // other, never both.
                | (
                    Mark::Superscript | Mark::Subscript,
                    Mark::Superscript | Mark::Subscript
                )
                | (Mark::Size(_), Mark::Size(_))
                | (Mark::Font(_), Mark::Font(_))
                | (Mark::Insertion(_), Mark::Insertion(_))
                | (Mark::Deletion(_), Mark::Deletion(_))
                | (Mark::Color(_), Mark::Color(_))
                | (Mark::Highlight(_), Mark::Highlight(_))
                | (Mark::Comment(_), Mark::Comment(_))
                | (Mark::Field(_), Mark::Field(_))
        )
    }

    /// The tracked change this mark names, when it names one: what a view accepts or rejects
    /// change by change.
    #[must_use]
    pub fn change(&self) -> Option<&str> {
        match self {
            Mark::Insertion(id) | Mark::Deletion(id) => Some(id),
            _ => None,
        }
    }

    /// Whether the mark is one of a stretch's own look, which a toolbar sets to
    /// a value rather than toggles: setting it again replaces what was there.
    #[must_use]
    pub fn replaces_its_kind(&self) -> bool {
        matches!(
            self,
            Mark::Size(_) | Mark::Font(_) | Mark::Color(_) | Mark::Highlight(_)
        )
    }

    /// The mark a stored reference `value` reads as: a mention of the member it names, or a
    /// reference to whatever else it names. `None` for an empty value.
    #[must_use]
    pub fn of_reference(value: &str) -> Option<Self> {
        match tilo_reference::Embed::of_mark(value) {
            Some(tilo_reference::Embed::Member(member)) => Some(Self::Mention(member)),
            Some(tilo_reference::Embed::Resource(_)) | None => {
                (!value.is_empty()).then(|| Self::Reference(value.to_owned()))
            }
        }
    }

    /// The value a mention or a record reference is stored as; `None` for any other mark.
    #[must_use]
    pub fn reference(&self) -> Option<String> {
        match self {
            Self::Mention(member) => {
                Some(tilo_reference::Embed::Member(member.clone()).mark_value())
            }
            Self::Reference(value) => Some(value.clone()),
            _ => None,
        }
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

/// Result of `toggle_mark_range`: did the toggle add or remove the mark on this range?
/// Returned so the caller can update any pending "next character will be marked" state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToggleOutcome {
    Added,
    Removed,
    NoOp,
}

/// True iff every byte in `range` (a non-empty sub-range of the line)
/// is covered by at least one same-kind span. Used to decide whether
/// a toggle should add or remove the mark.
pub fn range_fully_covered(spans: &[MarkSpan], range: &Range<usize>, mark: &Mark) -> bool {
    if range.start >= range.end {
        return false;
    }
    let mut covered = range.start;
    let mut sorted: Vec<&Range<usize>> = spans
        .iter()
        .filter(|s| s.mark.same_kind(mark))
        .map(|s| &s.range)
        .collect();
    sorted.compact_sort_by_key(|r| r.start);
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

/// True iff every byte in `range` (a non-empty sub-range of the line)
/// is covered by a span whose mark is exactly `mark`: not merely one of its
/// kind, but the same value — the same colour, the same size, the same
/// baseline.
pub fn range_covered_by(spans: &[MarkSpan], range: &Range<usize>, mark: &Mark) -> bool {
    if range.start >= range.end {
        return false;
    }
    let mut covered = range.start;
    let mut sorted: Vec<&Range<usize>> = spans
        .iter()
        .filter(|s| &s.mark == mark)
        .map(|s| &s.range)
        .collect();
    sorted.compact_sort_by_key(|r| r.start);
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

/// Apply a toggle of `mark` over `range`: if fully covered by same-kind
/// spans, remove the coverage (clip/split); otherwise add a new span and
/// merge touching same-kind spans into one contiguous run.
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
    let mut out: Vec<MarkSpan> = Vec::with_capacity(spans.len().saturating_add(2));
    for s in spans.drain(..) {
        if !s.mark.same_kind(mark) {
            out.push(s);
            continue;
        }
        if s.range.end <= range.start || s.range.start >= range.end {
            out.push(s);
            continue;
        }
        if s.range.start < range.start {
            out.push(MarkSpan::new(s.range.start..range.start, s.mark.clone()));
        }
        if s.range.end > range.end {
            out.push(MarkSpan::new(range.end..s.range.end, s.mark.clone()));
        }
    }
    out.compact_sort_by_key(|s| s.range.start);
    *spans = out;
}

/// Add a same-kind span covering `range` and merge any touching /
/// overlapping same-kind spans into a single run.
pub fn add_mark_to_range(spans: &mut Vec<MarkSpan>, range: Range<usize>, mark: Mark) {
    let mut start = range.start;
    let mut end = range.end;
    let mut out: Vec<MarkSpan> = Vec::with_capacity(spans.len().saturating_add(1));
    for s in spans.drain(..) {
        if !s.mark.same_kind(&mark) {
            out.push(s);
            continue;
        }
        if s.range.end >= start && s.range.start <= end {
            start = start.min(s.range.start);
            end = end.max(s.range.end);
        } else {
            out.push(s);
        }
    }
    out.push(MarkSpan::new(start..end, mark));
    out.compact_sort_by_key(|s| s.range.start);
    *spans = out;
}

/// [`shift_after_edit`] the way the document carries marks over typing: a
/// span ending where text is inserted grows over it, as a mark the document
/// stores with `ExpandMark::After` does. Typing right after bold text goes
/// on in bold, and an editor shows what its document will hold.
pub fn carry_through_edit(
    spans: &mut Vec<MarkSpan>,
    at: usize,
    inserted_len: usize,
    removed_len: usize,
) {
    shift_after_edit(spans, at, inserted_len, removed_len);
    if inserted_len == 0 {
        return;
    }
    for span in spans.iter_mut() {
        if span.range.end == at && span.range.start < at {
            span.range.end = at.saturating_add(inserted_len);
        }
    }
}

/// Adjust every span's range by a splice at byte `at`, to keep marks
/// aligned after a user edit. Insertion: spans at/after `at` shift right,
/// straddling spans extend right. Deletion of `[at, at+removed_len)`: spans
/// entirely inside are dropped, straddling spans are clipped.
pub fn shift_after_edit(
    spans: &mut Vec<MarkSpan>,
    at: usize,
    inserted_len: usize,
    removed_len: usize,
) {
    if inserted_len == 0 && removed_len == 0 {
        return;
    }
    let mut out: Vec<MarkSpan> = Vec::with_capacity(spans.len());
    let removed_end = at.saturating_add(removed_len);
    for s in spans.drain(..) {
        let mut start = s.range.start;
        let mut end = s.range.end;

        if removed_len > 0 {
            if start >= removed_end {
                // `start >= removed_end == at + removed_len >= removed_len`
                // here, so this can't underflow; `saturating_sub` is a
                // no-op belt-and-braces rather than the safety net.
                start = start.saturating_sub(removed_len);
                end = end.saturating_sub(removed_len);
            } else if end <= at {
            } else {
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
                        // A mark spanning a deletion stays one mark (Notion/
                        // Word/Quill behaviour: deleting glue between two
                        // bold halves yields one bold word, not two).
                        // `a.start >= removed_end >= removed_len` here, so
                        // this can't underflow.
                        let a_start = a.start.saturating_sub(removed_len);
                        let a_end = a.end.saturating_sub(removed_len);
                        out.push(MarkSpan::new(b.start..a_end.max(b.end), s.mark.clone()));
                        let _ = a_start; // kept for symmetry
                        continue;
                    }
                    (Some(b), None) => {
                        start = b.start;
                        end = b.end;
                    }
                    (None, Some(a)) => {
                        // Same invariant as `a_start`/`a_end` above.
                        start = a.start.saturating_sub(removed_len);
                        end = a.end.saturating_sub(removed_len);
                    }
                    (None, None) => {
                        continue;
                    }
                }
            }
        }

        if inserted_len > 0 {
            if start >= at {
                start = start.saturating_add(inserted_len);
                end = end.saturating_add(inserted_len);
            } else if end > at {
                end = end.saturating_add(inserted_len);
            }
        }

        if end > start {
            out.push(MarkSpan::new(start..end, s.mark.clone()));
        }
    }
    out.compact_sort_by_key(|s| s.range.start);
    *spans = out;
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent mark kind mirroring the Mark enum's variants (a segment can carry any combination), not a state machine; renderers and tests read these by name"
)]
pub struct MarkSegment {
    pub range: Range<usize>,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub link: Option<PageId>,
    pub external_link: bool,
    pub resource_link: Option<String>,
    pub embed: Option<String>,
    pub mention: Option<String>,
    /// Raised or lowered, as the run's own baseline shift.
    pub baseline: BaselineShift,
    /// The run's own size in points.
    pub size: Option<u16>,
    /// The run's own face.
    pub family: Option<FontFamily>,
    /// The run's own ink.
    pub color: Option<[u8; 3]>,
    /// The tracked change that inserted it.
    pub insertion: Option<String>,
    /// The tracked change that deleted it.
    pub deletion: Option<String>,
    /// The band behind the run.
    pub highlight: Option<[u8; 3]>,
}

impl MarkSegment {
    pub fn plain(range: Range<usize>) -> Self {
        Self {
            range,
            bold: false,
            italic: false,
            code: false,
            underline: false,
            strikethrough: false,
            link: None,
            external_link: false,
            resource_link: None,
            embed: None,
            mention: None,
            baseline: BaselineShift::None,
            size: None,
            family: None,
            color: None,
            insertion: None,
            deletion: None,
            highlight: None,
        }
    }
}

/// Produce the partition.
pub fn segments_for(text_len: usize, spans: &[MarkSpan]) -> Vec<MarkSegment> {
    if text_len == 0 {
        return Vec::new();
    }
    let mut boundaries: Vec<usize> =
        Vec::with_capacity(spans.len().saturating_mul(2).saturating_add(2));
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
    boundaries.compact_sort();
    boundaries.dedup();
    let mut out = Vec::with_capacity(boundaries.len());
    for w in boundaries.windows(2) {
        // `.windows(2)` only ever yields length-2 slices, so this always
        // matches; destructuring keeps the lookup indexing-free instead of
        // trusting `w[0]`/`w[1]` to stay in bounds.
        let &[start, end] = w else {
            continue;
        };
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
                    Mark::Underline => seg.underline = true,
                    Mark::Strikethrough => seg.strikethrough = true,
                    Mark::Link(id) | Mark::Transclusion(id) => seg.link = Some(*id),
                    Mark::Embed(reference) => seg.embed = Some(reference.clone()),
                    Mark::ExternalLink(_) => seg.external_link = true,
                    Mark::ResourceLink(href) => seg.resource_link = Some(href.clone()),
                    Mark::Mention(_) | Mark::Reference(_) => seg.mention = s.mark.reference(),
                    Mark::Superscript => {
                        if seg.baseline == BaselineShift::None {
                            seg.baseline = BaselineShift::Super;
                        }
                    }
                    Mark::Subscript => seg.baseline = BaselineShift::Sub,
                    Mark::Insertion(id) => seg.insertion = Some(id.clone()),
                    Mark::Deletion(id) => seg.deletion = Some(id.clone()),
                    Mark::Size(points) => seg.size = Some(*points),
                    Mark::Font(family) => seg.family = Some(*family),
                    Mark::Color(rgb) => seg.color = Some(*rgb),
                    Mark::Highlight(rgb) => seg.highlight = Some(*rgb),
                    // A comment's band and a field's drawn text are the editor's to paint, not
                    // the run's look: neither changes how the run is shaped.
                    Mark::Comment(_) | Mark::Field(_) => {}
                }
            }
        }
        out.push(seg);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> PageId {
        PageId::from_u128(n)
    }

    tilo_test_env::scenario! {
        fn marks_are_added_removed_shifted_and_segmented;

        phase add_and_remove_marks_merge_separate_and_clip;
        {
            fn add_mark_inserts_span() {
                let mut spans = Vec::new();
                add_mark_to_range(&mut spans, 0..5, Mark::Bold);
                assert_eq!(spans.len(), 1);
                assert_eq!(spans[0].range, 0..5);
            }

            fn add_mark_merges_touching_same_kind() {
                let mut spans = vec![MarkSpan::new(0..3, Mark::Bold)];
                add_mark_to_range(&mut spans, 3..6, Mark::Bold);
                assert_eq!(spans.len(), 1);
                assert_eq!(spans[0].range, 0..6);
            }

            fn add_mark_keeps_different_kinds_separate() {
                let mut spans = vec![MarkSpan::new(0..5, Mark::Bold)];
                add_mark_to_range(&mut spans, 0..5, Mark::Italic);
                assert_eq!(spans.len(), 2);
            }

            fn remove_mark_clips_straddle() {
                let mut spans = vec![MarkSpan::new(0..10, Mark::Bold)];
                remove_mark_from_range(&mut spans, 3..6, &Mark::Bold);
                assert_eq!(spans.len(), 2);
                assert_eq!(spans[0].range, 0..3);
                assert_eq!(spans[1].range, 6..10);
            }

            fn underline_strikethrough_same_kind() {
                assert!(Mark::Underline.same_kind(&Mark::Underline));
                assert!(Mark::Strikethrough.same_kind(&Mark::Strikethrough));
                assert!(!Mark::Underline.same_kind(&Mark::Strikethrough));
                assert!(!Mark::Underline.same_kind(&Mark::Bold));
            }

            add_mark_inserts_span();
            add_mark_merges_touching_same_kind();
            add_mark_keeps_different_kinds_separate();
            remove_mark_clips_straddle();
            underline_strikethrough_same_kind();
        }

        phase range_coverage_and_toggle_add_or_remove_marks;
        {
            fn range_fully_covered_simple() {
                let spans = vec![MarkSpan::new(0..5, Mark::Bold)];
                assert!(range_fully_covered(&spans, &(0..5), &Mark::Bold));
                assert!(range_fully_covered(&spans, &(1..4), &Mark::Bold));
                assert!(!range_fully_covered(&spans, &(0..6), &Mark::Bold));
                assert!(!range_fully_covered(&spans, &(0..5), &Mark::Italic));
            }

            fn range_fully_covered_two_adjacent_spans() {
                let spans = vec![
                    MarkSpan::new(0..3, Mark::Bold),
                    MarkSpan::new(3..6, Mark::Bold),
                ];
                assert!(range_fully_covered(&spans, &(0..6), &Mark::Bold));
            }

            fn toggle_adds_when_uncovered() {
                let mut spans = Vec::new();
                let outcome = toggle_mark_range(&mut spans, 0..5, Mark::Bold);
                assert_eq!(outcome, ToggleOutcome::Added);
                assert_eq!(spans.len(), 1);
            }

            fn toggle_removes_when_fully_covered() {
                let mut spans = vec![MarkSpan::new(0..5, Mark::Bold)];
                let outcome = toggle_mark_range(&mut spans, 0..5, Mark::Bold);
                assert_eq!(outcome, ToggleOutcome::Removed);
                assert!(spans.is_empty());
            }

            fn toggle_partial_range_adds_then_merges() {
                let mut spans = vec![MarkSpan::new(0..5, Mark::Bold)];
                let outcome = toggle_mark_range(&mut spans, 3..10, Mark::Bold);
                assert_eq!(outcome, ToggleOutcome::Added);
                assert_eq!(spans.len(), 1);
                assert_eq!(spans[0].range, 0..10);
            }

            range_fully_covered_simple();
            range_fully_covered_two_adjacent_spans();
            toggle_adds_when_uncovered();
            toggle_removes_when_fully_covered();
            toggle_partial_range_adds_then_merges();
        }

        phase shift_after_insert_moves_or_extends_spans;
        {
            fn shift_after_insert_after_span() {
                let mut spans = vec![MarkSpan::new(0..3, Mark::Bold)];
                shift_after_edit(&mut spans, 5, 2, 0);
                assert_eq!(spans[0].range, 0..3); // unaffected
            }

            fn shift_after_insert_inside_span_extends_right() {
                let mut spans = vec![MarkSpan::new(0..5, Mark::Bold)];
                shift_after_edit(&mut spans, 2, 3, 0);
                assert_eq!(spans[0].range, 0..8);
            }

            fn shift_after_insert_at_start_pushes_right() {
                let mut spans = vec![MarkSpan::new(2..5, Mark::Bold)];
                shift_after_edit(&mut spans, 0, 1, 0);
                assert_eq!(spans[0].range, 3..6);
            }

            shift_after_insert_after_span();
            shift_after_insert_inside_span_extends_right();
            shift_after_insert_at_start_pushes_right();
        }

        phase shift_after_delete_clips_or_drops_spans;
        {
            fn shift_after_delete_inside_span_clips() {
                let mut spans = vec![MarkSpan::new(0..10, Mark::Bold)];
                shift_after_edit(&mut spans, 2, 0, 3);
                assert_eq!(spans[0].range, 0..7);
            }

            fn shift_after_delete_drops_engulfed_span() {
                let mut spans = vec![MarkSpan::new(2..4, Mark::Bold)];
                shift_after_edit(&mut spans, 0, 0, 10);
                assert!(spans.is_empty());
            }

            shift_after_delete_inside_span_clips();
            shift_after_delete_drops_engulfed_span();
        }

        phase segments_split_combine_and_carry_mark_presentation;
        {
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

            fn link_mark_carries_id() {
                let i = id(42);
                let spans = vec![MarkSpan::new(0..5, Mark::Link(i))];
                let segs = segments_for(5, &spans);
                assert_eq!(segs[0].link, Some(i));
            }

            fn external_link_segment_gets_link_presentation_without_a_page_id() {
                let spans = vec![MarkSpan::new(
                    0..5,
                    Mark::ExternalLink("https://example.test".into()),
                )];
                let segs = segments_for(5, &spans);
                assert!(segs[0].external_link);
                assert_eq!(segs[0].link, None);
            }

            fn segments_for_empty_text_is_empty() {
                assert!(segments_for(0, &[]).is_empty());
            }

            fn segments_for_no_spans_is_one_plain_segment() {
                let segs = segments_for(7, &[]);
                assert_eq!(segs.len(), 1);
                assert_eq!(segs[0].range, 0..7);
                assert!(!segs[0].bold);
                assert!(!segs[0].underline);
                assert!(!segs[0].strikethrough);
            }

            fn underline_and_strikethrough_segments_carry_their_flags() {
                let spans = vec![
                    MarkSpan::new(0..4, Mark::Underline),
                    MarkSpan::new(2..6, Mark::Strikethrough),
                ];
                let segs = segments_for(8, &spans);
                // Byte 0..2: underline only. 2..4: both. 4..6: strike only. 6..8: none.
                let at = |b: usize| segs.iter().find(|s| s.range.contains(&b)).unwrap();
                assert!(at(0).underline && !at(0).strikethrough);
                assert!(at(2).underline && at(2).strikethrough);
                assert!(!at(5).underline && at(5).strikethrough);
                assert!(!at(7).underline && !at(7).strikethrough);
            }

            segments_split_at_span_boundaries();
            segments_combine_overlapping_marks();
            link_mark_carries_id();
            external_link_segment_gets_link_presentation_without_a_page_id();
            segments_for_empty_text_is_empty();
            segments_for_no_spans_is_one_plain_segment();
            underline_and_strikethrough_segments_carry_their_flags();
        }
    }
    /// The run's own look reaches the segment a renderer draws: raised or lowered, a size in
    /// points, a face, an ink and a band. Raised and lowered are one kind, and a range is
    /// covered by the value it holds rather than by any mark of that kind.
    #[test]
    fn a_runs_own_look_reaches_its_segment() {
        let spans = vec![
            MarkSpan::new(0..1, Mark::Superscript),
            MarkSpan::new(1..2, Mark::Subscript),
            MarkSpan::new(2..3, Mark::Size(14)),
            MarkSpan::new(3..4, Mark::Font(FontFamily::Serif)),
            MarkSpan::new(4..5, Mark::Color([0xcf, 0x22, 0x2e])),
            MarkSpan::new(5..6, Mark::Highlight([0xff, 0xf8, 0xc5])),
        ];
        let segments = segments_for(6, &spans);
        assert_eq!(segments.len(), 6);
        assert_eq!(segments[0].baseline, BaselineShift::Super);
        assert_eq!(segments[1].baseline, BaselineShift::Sub);
        assert_eq!(segments[2].size, Some(14));
        assert_eq!(segments[3].family, Some(FontFamily::Serif));
        assert_eq!(segments[4].color, Some([0xcf, 0x22, 0x2e]));
        assert_eq!(segments[5].highlight, Some([0xff, 0xf8, 0xc5]));

        assert!(Mark::Superscript.same_kind(&Mark::Subscript));
        assert!(!Mark::Color([1, 2, 3]).same_kind(&Mark::Highlight([1, 2, 3])));
        assert!(range_covered_by(&spans, &(4..5), &Mark::Color([0xcf, 0x22, 0x2e])));
        assert!(
            !range_covered_by(&spans, &(4..5), &Mark::Color([0x09, 0x69, 0xda])),
            "another colour of the same kind is not the value held"
        );
    }
}