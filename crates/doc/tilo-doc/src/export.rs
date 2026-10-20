//! Format-neutral, borrowed views used while exporting rich text.
//!
//! [`InlinePlan`] turns the canonical mark spans for one block into deterministic
//! semantic boundaries and runs.  It deliberately knows nothing about XML,
//! package relationships, escaping, or the nesting rules of a particular file
//! format.  Exporters remain responsible for those representation choices.

use crate::document::MarkSpan;
use crate::snapshot::{AttrVal, BlockView, ChangeKind};
use std::collections::{BTreeMap, BTreeSet};
use tilo_sort::CompactSort as _;

/// Canonical block semantics shared by format exporters.
///
/// Aliases are normalized here so adding a writer does not require copying the
/// same string dispatch yet again. Unknown text-bearing kinds deliberately
/// remain paragraphs, matching the existing exporters' forward-compatible
/// fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    Heading,
    Quote,
    ListItem,
    Table,
    Image,
    Code,
}

/// Borrowed semantic view of one canonical block.
#[derive(Clone, Copy, Debug)]
pub struct ExportBlock<'a> {
    block: &'a BlockView,
}

impl<'a> ExportBlock<'a> {
    pub fn new(block: &'a BlockView) -> Self {
        Self { block }
    }

    pub fn kind(self) -> BlockKind {
        match self.block.kind.as_str() {
            "heading" => BlockKind::Heading,
            "quote" | "blockquote" => BlockKind::Quote,
            "list_item" => BlockKind::ListItem,
            "table" => BlockKind::Table,
            "image" => BlockKind::Image,
            "code" | "code_block" => BlockKind::Code,
            _ => BlockKind::Paragraph,
        }
    }

    /// Heading level in the range supported by the canonical document model.
    pub fn heading_level(self) -> u8 {
        self.block
            .int_attr("level")
            .map_or(1, |level| u8::try_from(level.clamp(1, 6)).unwrap_or(1))
    }

    /// A list item's 0-based nesting depth; negative values read as 0.
    pub fn list_level(self) -> i64 {
        self.block.int_attr("level").map_or(0, |level| level.max(0))
    }

    /// Whether a list item is numbered rather than bulleted.
    pub fn ordered(self) -> bool {
        self.block.flag("ordered")
    }

    /// A numbered item's canonical format (`decimal`, `lower-roman`, ...).
    pub fn num_format(self) -> &'a str {
        self.block.str_attr("num_format").unwrap_or("decimal")
    }

    /// The number a numbered list starts at.
    pub fn num_start(self) -> i64 {
        self.block.int_attr("num_start").unwrap_or(1)
    }

    /// Whether this item restarts its list's numbering at [`Self::num_start`].
    pub fn num_restart(self) -> bool {
        self.block.flag("num_restart")
    }

    /// An image's anchoring: `inline` unless the block says otherwise.
    pub fn image_anchor(self) -> &'a str {
        self.block.str_attr("anchor").unwrap_or("inline")
    }

    /// How text wraps around an anchored image: `none` unless the block says
    /// otherwise.
    pub fn image_wrap(self) -> &'a str {
        self.block.str_attr("wrap").unwrap_or("none")
    }

    /// The table a flat `cell` block belongs to.
    pub fn cell_table(self) -> Option<&'a str> {
        self.block.str_attr("table")
    }

    /// A cell's `(row, col)` grid position; missing or negative values read
    /// as 0.
    pub fn cell_position(self) -> (usize, usize) {
        self.block.cell_position()
    }

    /// How many grid columns a cell spans (at least 1, at most 1024).
    pub fn col_span(self) -> usize {
        self.block.cell_span("colspan")
    }

    /// How many grid rows a cell spans (at least 1, at most 1024).
    pub fn row_span(self) -> usize {
        self.block.cell_span("rowspan")
    }
}

/// One table stored as a contiguous run of flat `cell` blocks, laid out by
/// grid position.
///
/// Office imports keep a table's cells flat, each with its `table` id, its
/// position and its spans; every office writer needs the same grid back.
#[derive(Clone, Debug)]
pub struct CellGrid<'a> {
    cells: &'a [BlockView],
    origins: BTreeMap<(usize, usize), &'a BlockView>,
    covered: BTreeMap<(usize, usize), &'a BlockView>,
    rows: usize,
    cols: usize,
}

impl<'a> CellGrid<'a> {
    /// The cells from `blocks[start]` on that are `cell` blocks of the same
    /// table. Empty when `blocks[start]` is not a cell.
    pub fn run(blocks: &'a [BlockView], start: usize) -> Self {
        let rest = blocks.get(start..).unwrap_or_default();
        let table = rest
            .first()
            .and_then(|cell| ExportBlock::new(cell).cell_table());
        let len = rest
            .iter()
            .take_while(|block| {
                block.kind == "cell" && ExportBlock::new(block).cell_table() == table
            })
            .count();
        Self::new(rest.get(..len).unwrap_or_default())
    }

    fn new(cells: &'a [BlockView]) -> Self {
        let mut grid = Self {
            cells,
            origins: BTreeMap::new(),
            covered: BTreeMap::new(),
            rows: 0,
            cols: 0,
        };
        for cell in cells {
            let view = ExportBlock::new(cell);
            let (row, col) = view.cell_position();
            let (rows, cols) = (view.row_span(), view.col_span());
            grid.origins.entry((row, col)).or_insert(cell);
            for dr in 0..rows {
                for dc in 0..cols {
                    if dr > 0 || dc > 0 {
                        grid.covered
                            .entry((row.saturating_add(dr), col.saturating_add(dc)))
                            .or_insert(cell);
                    }
                }
            }
            grid.rows = grid.rows.max(row.saturating_add(rows));
            grid.cols = grid.cols.max(col.saturating_add(cols));
        }
        grid
    }

    /// The run's blocks, in document order.
    pub fn cells(&self) -> &'a [BlockView] {
        self.cells
    }

    /// The table id the run's cells share (empty when untagged).
    pub fn table(&self) -> &'a str {
        self.cells
            .first()
            .and_then(|cell| ExportBlock::new(cell).cell_table())
            .unwrap_or_default()
    }

    /// Grid rows, including those merged cells span into.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Grid columns, including those merged cells span into.
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// The cell whose top-left corner is at `(row, col)`.
    pub fn origin(&self, row: usize, col: usize) -> Option<&'a BlockView> {
        self.origins.get(&(row, col)).copied()
    }

    /// The merged cell covering `(row, col)` from an earlier row or column.
    pub fn covering(&self, row: usize, col: usize) -> Option<&'a BlockView> {
        self.covered.get(&(row, col)).copied()
    }
}

/// Canonical row/cell view of a table.
///
/// Imports may produce nested row blocks or a flat cell list with a `cols`
/// attribute. Exporters should not each have to rediscover that distinction.
#[derive(Clone, Debug, Default)]
pub struct TablePlan<'a> {
    rows: Vec<Vec<&'a BlockView>>,
}

impl<'a> TablePlan<'a> {
    pub fn new(table: &'a BlockView) -> Self {
        let rows = if table
            .children
            .iter()
            .any(|child| !child.children.is_empty())
        {
            table
                .children
                .iter()
                .map(|row| row.children.iter().collect())
                .collect()
        } else if table.children.is_empty() {
            Vec::new()
        } else {
            let requested = match table.attrs.get("cols") {
                Some(AttrVal::Int(cols)) => usize::try_from((*cols).max(1)).unwrap_or(usize::MAX),
                _ => table.children.len(),
            };
            let cols = requested.min(table.children.len()).max(1);
            table
                .children
                .chunks(cols)
                .map(|row| row.iter().collect())
                .collect()
        };
        Self { rows }
    }

    pub fn rows(&self) -> &[Vec<&'a BlockView>] {
        &self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Pre-order, depth-first walk over a block forest.
pub struct BlockWalk<'a> {
    stack: Vec<std::slice::Iter<'a, BlockView>>,
}

pub fn walk_blocks(blocks: &[BlockView]) -> BlockWalk<'_> {
    BlockWalk {
        stack: vec![blocks.iter()],
    }
}

impl<'a> Iterator for BlockWalk<'a> {
    type Item = &'a BlockView;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let iter = self.stack.last_mut()?;
            if let Some(block) = iter.next() {
                if !block.children.is_empty() {
                    self.stack.push(block.children.iter());
                }
                return Some(block);
            }
            self.stack.pop();
        }
    }
}

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
                active.compact_sort_by(|a, b| a.name.cmp(&b.name));
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
        values.compact_sort();
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
        values.compact_sort();
        values
    };
    RangeBoundary {
        ends: values(false),
        starts: values(true),
    }
}

/// `snap` as a writer that cannot reproduce kept inline elements shows it: the placeholder
/// character of each one without visible text (see [`crate::schema::KEPT_INLINE_MARKS`]) is
/// left out, the marks moving with the text. Kept elements with visible text stay as their
/// text. Borrows `snap` unchanged when it holds no such placeholder.
pub fn without_kept_placeholders(
    snap: &crate::snapshot::DocSnapshot,
) -> std::borrow::Cow<'_, crate::snapshot::DocSnapshot> {
    use std::borrow::Cow;
    match without_kept_placeholders_in(&snap.blocks) {
        Cow::Borrowed(_) => Cow::Borrowed(snap),
        Cow::Owned(blocks) => {
            let mut out = snap.clone();
            out.blocks = blocks;
            Cow::Owned(out)
        }
    }
}

/// [`without_kept_placeholders`] for a block sequence.
pub fn without_kept_placeholders_in(blocks: &[BlockView]) -> std::borrow::Cow<'_, [BlockView]> {
    use std::borrow::Cow;
    if !blocks.iter().any(|b| !kept_placeholders(b).is_empty()) {
        return Cow::Borrowed(blocks);
    }
    let mut out = blocks.to_vec();
    for b in &mut out {
        let removed = kept_placeholders(b);
        if removed.is_empty() {
            continue;
        }
        // A position moves back by the placeholders before it.
        let shift = |p: usize| p.saturating_sub(removed.partition_point(|&r| r < p));
        b.text = b
            .text
            .chars()
            .enumerate()
            .filter(|(i, _)| removed.binary_search(i).is_err())
            .map(|(_, c)| c)
            .collect();
        b.marks = std::mem::take(&mut b.marks)
            .into_iter()
            .map(|m| MarkSpan {
                start: shift(m.start),
                end: shift(m.end),
                ..m
            })
            .filter(|m| m.end > m.start)
            .collect();
    }
    Cow::Owned(out)
}

/// The character offsets of `b`'s placeholders that stand for kept inline elements, ascending.
fn kept_placeholders(b: &BlockView) -> Vec<usize> {
    if !b.text.contains(crate::schema::FIELD_PLACEHOLDER) {
        return Vec::new();
    }
    let kept: Vec<&MarkSpan> = b
        .marks
        .iter()
        .filter(|m| crate::schema::KEPT_INLINE_MARKS.contains(&m.name.as_str()))
        .collect();
    b.text
        .chars()
        .enumerate()
        .filter(|&(i, c)| {
            c == crate::schema::FIELD_PLACEHOLDER && kept.iter().any(|m| m.start <= i && i < m.end)
        })
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::MarkValue;
    use std::collections::BTreeMap;

    #[test]
    fn kept_placeholders_are_left_out_and_marks_move() {
        let p = crate::schema::FIELD_PLACEHOLDER;
        let mut b = block("paragraph", &format!("a{p}b{p}c"));
        let mark = |name: &str, start, end| MarkSpan {
            start,
            end,
            name: name.into(),
            value: MarkValue::Str("x".into()),
        };
        // A kept drawing, a note reference (which stays), and bold over "b…c".
        b.marks = vec![
            mark(crate::schema::MARK_DOCX_XML, 1, 2),
            mark(crate::schema::MARK_NOTE_REF, 3, 4),
            mark("strong", 2, 5),
        ];
        let blocks = [b];
        let out = without_kept_placeholders_in(&blocks);
        assert_eq!(out[0].text, format!("ab{p}c"));
        assert_eq!(out[0].marks.len(), 2);
        assert_eq!((out[0].marks[0].start, out[0].marks[0].end), (2, 3));
        assert_eq!((out[0].marks[1].start, out[0].marks[1].end), (1, 4));
        let plain = [block("paragraph", "plain")];
        assert!(matches!(
            without_kept_placeholders_in(&plain),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    fn block(kind: &str, text: &str) -> BlockView {
        BlockView {
            id: text.into(),
            kind: kind.into(),
            text: text.into(),
            marks: Vec::new(),
            attrs: BTreeMap::new(),
            children: Vec::new(),
        }
    }

    #[test]
    fn block_plans_normalize_kinds_tables_images_and_walk_order() {
        // Aliases, clamped heading levels and unknown kinds.
        let mut heading = block("heading", "title");
        heading.attrs.insert("level".into(), AttrVal::Int(99));
        assert_eq!(ExportBlock::new(&heading).kind(), BlockKind::Heading);
        assert_eq!(ExportBlock::new(&heading).heading_level(), 6);
        for (kind, want) in [
            ("blockquote", BlockKind::Quote),
            ("code_block", BlockKind::Code),
            ("future-kind", BlockKind::Paragraph),
        ] {
            assert_eq!(
                ExportBlock::new(&block(kind, "x")).kind(),
                want,
                "case {kind}"
            );
        }

        // Tables: nested and flat shapes, without huge allocations.
        let mut nested = block("table", "nested");
        let mut row = block("table_row", "row");
        row.children = vec![block("cell", "a"), block("cell", "b")];
        nested.children.push(row);
        assert_eq!(TablePlan::new(&nested).rows()[0].len(), 2);
        let mut flat = block("table", "flat");
        flat.children = vec![block("cell", "a"), block("cell", "b"), block("cell", "c")];
        flat.attrs.insert("cols".into(), AttrVal::Int(2));
        assert_eq!(
            TablePlan::new(&flat)
                .rows()
                .iter()
                .map(Vec::len)
                .collect::<Vec<_>>(),
            [2, 1]
        );
        flat.attrs.insert("cols".into(), AttrVal::Int(i64::MAX));
        assert_eq!(TablePlan::new(&flat).rows().len(), 1);

        // The canonical depth-first walk.
        let mut root = block("paragraph", "root");
        let mut child = block("paragraph", "child");
        child.children.push(block("paragraph", "grandchild"));
        root.children.push(child);
        let sibling = block("paragraph", "sibling");
        assert_eq!(
            walk_blocks(&[root, sibling])
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>(),
            ["root", "child", "grandchild", "sibling"]
        );
    }

    #[test]
    fn export_accessors_use_canonical_defaults_and_cell_bounds() {
        let mut b = block("list_item", "item");
        let view = ExportBlock::new(&b);
        assert_eq!(view.list_level(), 0);
        assert!(!view.ordered());
        assert_eq!(view.num_format(), "decimal");
        assert_eq!(view.num_start(), 1);
        assert!(!view.num_restart());
        assert_eq!(view.image_anchor(), "inline");
        assert_eq!(view.image_wrap(), "none");
        assert_eq!(view.cell_table(), None);
        assert_eq!(view.cell_position(), (0, 0));
        assert_eq!((view.row_span(), view.col_span()), (1, 1));

        b.attrs.extend([
            ("level".into(), AttrVal::Int(-1)),
            ("ordered".into(), AttrVal::Bool(true)),
            ("num_format".into(), AttrVal::Str("lower-roman".into())),
            ("num_start".into(), AttrVal::Int(3)),
            ("num_restart".into(), AttrVal::Bool(true)),
            ("row".into(), AttrVal::Int(-1)),
            ("col".into(), AttrVal::Int(2)),
            ("rowspan".into(), AttrVal::Int(0)),
            ("colspan".into(), AttrVal::Int(i64::MAX)),
        ]);
        let view = ExportBlock::new(&b);
        assert_eq!(view.list_level(), 0);
        assert!(view.ordered());
        assert_eq!(view.num_format(), "lower-roman");
        assert_eq!(view.num_start(), 3);
        assert!(view.num_restart());
        assert_eq!(view.cell_position(), (0, 2));
        assert_eq!((view.row_span(), view.col_span()), (1, 1024));
    }

    #[test]
    fn cell_grid_groups_runs_and_includes_merged_extents() {
        let mut merged = block("cell", "merged");
        merged.attrs.extend([
            ("table".into(), AttrVal::Str("a".into())),
            ("rowspan".into(), AttrVal::Int(2)),
            ("colspan".into(), AttrVal::Int(3)),
        ]);
        let mut next_table = block("cell", "next");
        next_table
            .attrs
            .insert("table".into(), AttrVal::Str("b".into()));
        let blocks = [merged, next_table, block("paragraph", "after")];
        let grid = CellGrid::run(&blocks, 0);
        assert_eq!(grid.cells().len(), 1);
        assert_eq!(grid.table(), "a");
        assert_eq!((grid.rows(), grid.cols()), (2, 3));
        assert_eq!(grid.origin(0, 0).unwrap().text, "merged");
        assert!(grid.origin(1, 2).is_none());
        assert_eq!(grid.covering(1, 2).unwrap().text, "merged");
        assert!(grid.covering(0, 0).is_none());
        assert!(grid.covering(2, 0).is_none());
        assert_eq!(CellGrid::run(&blocks, 1).table(), "b");
        assert!(CellGrid::run(&blocks, 2).cells().is_empty());
        assert!(CellGrid::run(&blocks, usize::MAX).cells().is_empty());
        let untagged = [block("cell", "a"), block("cell", "b")];
        assert_eq!(CellGrid::run(&untagged, 0).cells().len(), 2);
    }

    fn mark(start: usize, end: usize, name: &str, value: MarkValue) -> MarkSpan {
        MarkSpan {
            start,
            end,
            name: name.into(),
            value,
        }
    }

    fn str_mark(start: usize, end: usize, name: &str, value: &str) -> MarkSpan {
        mark(start, end, name, MarkValue::Str(value.into()))
    }

    fn runs<'p, 'a>(plan: &'p InlinePlan<'a>) -> Vec<&'p InlineRun<'a>> {
        plan.points()
            .iter()
            .filter_map(|point| point.run.as_ref())
            .collect()
    }

    #[test]
    fn inline_plans_split_maximal_runs_and_order_boundaries() {
        use crate::schema::{
            MARK_BOOKMARK, MARK_COMMENT, MARK_DEL, MARK_INS, MARK_LINK, MARK_NOTE_REF,
        };

        // Unicode is planned by character and runs borrow the UTF-8 text.
        let marks = vec![mark(1, 3, "strong", MarkValue::Bool(true))];
        let plan = InlinePlan::new("aé🦀z", &marks);
        let r = runs(&plan);
        assert_eq!(r.len(), 3);
        assert_eq!((r[0].char_range.clone(), r[0].text), (0..1, "a"));
        assert_eq!((r[1].char_range.clone(), r[1].text), (1..3, "é🦀"));
        assert_eq!((r[2].char_range.clone(), r[2].text), (3..4, "z"));
        assert!(std::ptr::eq(r[1].marks[0], std::ptr::from_ref(&marks[0])));

        // Adjacent and overlapping semantic boundaries order deterministically.
        let marks = vec![
            str_mark(0, 2, MARK_COMMENT, "z"),
            str_mark(0, 2, MARK_COMMENT, "a"),
            str_mark(2, 4, MARK_COMMENT, "next"),
            str_mark(1, 2, MARK_BOOKMARK, "bm"),
            str_mark(2, 3, MARK_INS, "change"),
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

        // Links, changes, marks and internal targets per maximal run.
        let marks = vec![
            str_mark(0, 4, MARK_LINK, "#target"),
            str_mark(1, 3, MARK_DEL, "deleted"),
            mark(1, 3, "em", MarkValue::Bool(true)),
            str_mark(1, 3, MARK_INS, "inserted"),
        ];
        let plan = InlinePlan::new("abcd", &marks);
        let r = runs(&plan);
        assert_eq!(
            r.iter().map(|r| r.text).collect::<Vec<_>>(),
            ["a", "bc", "d"]
        );
        assert_eq!(r[1].cross_reference(), Some("target"));
        assert_eq!(
            r[1].change,
            Some(ActiveChange {
                kind: ChangeKind::Ins,
                id: "inserted"
            })
        );
        assert_eq!(
            r[1].marks
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            ["del", "em", "ins", "link"]
        );

        // Only exact note placeholders are note references, and the terminal
        // boundary is kept.
        let p = crate::schema::FIELD_PLACEHOLDER;
        let text = format!("x{p}y{p}");
        let marks = vec![
            str_mark(1, 2, MARK_NOTE_REF, "note"),
            str_mark(3, 5, MARK_NOTE_REF, "missing-shape"),
            str_mark(0, 4, MARK_BOOKMARK, "all"),
        ];
        let plan = InlinePlan::new(&text, &marks);
        let notes: Vec<_> = runs(&plan).iter().map(|r| r.note_reference).collect();
        assert_eq!(notes, [None, Some("note"), None, None]);
        let terminal = plan.points().last().unwrap();
        assert_eq!(terminal.offset, 4);
        assert!(terminal.run.is_none());
        assert_eq!(terminal.boundary.bookmarks.ends, ["all"]);

        // Out-of-range boundaries are ignored, not clamped into the output.
        let marks = vec![str_mark(8, 9, MARK_COMMENT, "bad")];
        let plan = InlinePlan::new("hi", &marks);
        assert_eq!(plan.points().len(), 2);
        assert!(plan.points().iter().all(|p| p.boundary.is_empty()));
    }
}
