//! Format-neutral, borrowed views used while exporting rich text. [`InlinePlan`]
//! turns the canonical mark spans for one block into deterministic semantic
//! boundaries and runs, knowing nothing about XML, packages or a format's nesting
//! rules.

use crate::document::MarkSpan;
use crate::snapshot::{AttrVal, BlockView, ChangeKind};
use std::collections::{BTreeMap, BTreeSet};
use tilo_sort::CompactSort as _;

/// Canonical block semantics shared by format exporters. Aliases are normalized here, and unknown
/// text-bearing kinds stay paragraphs, matching the exporters' forward-compatible fallback.
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

    pub fn heading_level(self) -> u8 {
        self.block
            .int_attr("level")
            .map_or(1, |level| u8::try_from(level.clamp(1, 6)).unwrap_or(1))
    }

    /// A list item's 0-based nesting depth; negative values read as 0.
    pub fn list_level(self) -> i64 {
        self.block.int_attr("level").map_or(0, |level| level.max(0))
    }

    pub fn ordered(self) -> bool {
        self.block.flag("ordered")
    }

    /// A numbered item's canonical format (`decimal`, `lower-roman`, ...).
    pub fn num_format(self) -> &'a str {
        self.block.str_attr("num_format").unwrap_or("decimal")
    }

    pub fn num_start(self) -> i64 {
        self.block.int_attr("num_start").unwrap_or(1)
    }

    pub fn num_restart(self) -> bool {
        self.block.flag("num_restart")
    }

    pub fn image_anchor(self) -> &'a str {
        self.block.str_attr("anchor").unwrap_or("inline")
    }

    pub fn image_wrap(self) -> &'a str {
        self.block.str_attr("wrap").unwrap_or("none")
    }

    pub fn cell_table(self) -> Option<&'a str> {
        self.block.str_attr("table")
    }

    pub fn cell_position(self) -> (usize, usize) {
        self.block.cell_position()
    }

    /// How many grid columns a cell spans, at least 1 and at most 1024.
    pub fn col_span(self) -> usize {
        self.block.cell_span("colspan")
    }

    /// How many grid rows a cell spans, at least 1 and at most 1024.
    pub fn row_span(self) -> usize {
        self.block.cell_span("rowspan")
    }
}

/// One table stored as a contiguous run of flat `cell` blocks, laid out by grid position: office
/// imports keep cells flat with their `table` id, position and spans, and every writer needs the
/// same grid back.
#[derive(Clone, Debug)]
pub struct CellGrid<'a> {
    cells: &'a [BlockView],
    origins: BTreeMap<(usize, usize), &'a BlockView>,
    covered: BTreeMap<(usize, usize), &'a BlockView>,
    rows: usize,
    cols: usize,
}

impl<'a> CellGrid<'a> {
    /// The cells from `blocks[start]` on that are `cell` blocks of the same table; empty when that
    /// block is not a cell.
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

    pub fn cells(&self) -> &'a [BlockView] {
        self.cells
    }

    pub fn table(&self) -> &'a str {
        self.cells
            .first()
            .and_then(|cell| ExportBlock::new(cell).cell_table())
            .unwrap_or_default()
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn origin(&self, row: usize, col: usize) -> Option<&'a BlockView> {
        self.origins.get(&(row, col)).copied()
    }

    pub fn covering(&self, row: usize, col: usize) -> Option<&'a BlockView> {
        self.covered.get(&(row, col)).copied()
    }
}

/// Canonical row/cell view of a table: imports may produce nested row blocks or a flat cell list
/// with a `cols` attribute, and exporters should not each rediscover which.
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

/// The values whose ranges end and start at one character offset. Ends and starts stay separate
/// because office formats require adjacent ranges to close before the next opens, and values are
/// sorted but not deduplicated.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RangeBoundary<'a> {
    pub ends: Vec<&'a str>,
    pub starts: Vec<&'a str>,
}

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

/// The tracked change covering a run. Insertion takes precedence when malformed input has
/// overlapping insertion and deletion marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveChange<'a> {
    pub kind: ChangeKind,
    pub id: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineRun<'a> {
    /// Half-open range in Unicode scalar-value (character) offsets.
    pub char_range: std::ops::Range<usize>,
    pub text: &'a str,
    /// Every mark active over the run, sorted stably by mark name.
    pub marks: Vec<&'a MarkSpan>,
    pub link: Option<&'a str>,
    pub change: Option<ActiveChange<'a>>,
    pub note_reference: Option<&'a str>,
}

impl<'a> InlineRun<'a> {
    pub fn cross_reference(&self) -> Option<&'a str> {
        self.link
            .and_then(|href| href.strip_prefix(crate::schema::CROSS_REF_PREFIX))
    }
}

/// Boundaries followed by the run beginning at the same character offset; `run` is `None` for the
/// terminal point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlinePoint<'a> {
    pub offset: usize,
    pub boundary: InlineBoundary<'a>,
    pub run: Option<InlineRun<'a>>,
}

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

/// `snap` as a writer that cannot reproduce kept inline elements shows it: the placeholder of each
/// one without visible text (see [`crate::schema::KEPT_INLINE_MARKS`]) is left out, the marks
/// moving with the text, and kept elements with visible text stay as their text. Borrows `snap`
/// unchanged when it holds no such placeholder.
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
