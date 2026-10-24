//! Pure diff & merge model — no rendering, no IO.
//!
//! Two layers live here:
//!
//! * [`DiffModel`] — a two-way, line-aligned comparison of a *left* and a
//!   *right* text. The alignment produces a flat list of [`AlignedRow`]s so the
//!   side-by-side view can paint left and right gutters that line up: an inserted
//!   line on the right has an empty cell on the left, a deletion the reverse, and
//!   a modification pairs the two changed lines on the same row.
//!
//! * [`MergeModel`] — a three-way merge of a common *base* against *ours* and
//!   *theirs*. Regions where only one side changed auto-resolve; regions where
//!   both sides changed the same base lines become `Conflict` hunks the user
//!   resolves per-hunk (take ours / theirs / both). The resolved output is a
//!   sequence of lines the app mirrors into an Automerge `Text` so the merged
//!   buffer is collaboratively editable.
//!
//! All hunking is built on the `similar` crate's Myers/patience diff.

use similar::{Algorithm, ChangeTag, TextDiff};

thread_local! {
    // Resource publication and the retained view ask for the same comparison.
    // Share immutable alignment rather than rerunning the full algorithm on
    // every local Next/Previous action. Two entries bound retained input memory.
    static COMPARISONS: std::cell::RefCell<std::collections::VecDeque<ComparisonCache>> =
        const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
}

#[cfg(test)]
thread_local! {
    static DIGEST_PASSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn digest_passes() -> usize {
    DIGEST_PASSES.get()
}

struct ComparisonCache {
    key: (blake3::Hash, blake3::Hash),
    model: std::sync::Arc<DiffModel>,
}

/// The exact content's alignment, shared by resources and their retained view.
/// Labels do not affect alignment; both source digests do. Editing either
/// source therefore cannot reuse the previous comparison's blocks.
pub(crate) fn comparison(left: &str, right: &str) -> std::sync::Arc<DiffModel> {
    #[cfg(test)]
    DIGEST_PASSES.set(DIGEST_PASSES.get().saturating_add(1));
    let key = (
        blake3::hash(left.as_bytes()),
        blake3::hash(right.as_bytes()),
    );
    COMPARISONS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(entry) = cache.iter().find(|entry| entry.key == key) {
            return std::sync::Arc::clone(&entry.model);
        }
        let model = std::sync::Arc::new(DiffModel::compute(left, right));
        if cache.len() >= 2 {
            cache.pop_front();
        }
        cache.push_back(ComparisonCache {
            key,
            model: std::sync::Arc::clone(&model),
        });
        model
    })
}

/// The kind of change a single aligned row represents in a two-way diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// Both sides carry the same line.
    Equal,
    /// Line present only on the right (added).
    Insert,
    /// Line present only on the left (removed).
    Delete,
    /// Both sides carry a line but they differ.
    Modify,
}

/// One visual row of a side-by-side diff. `left`/`right` hold the line text
/// for that side, or `None` when that side has no line on this row (so the
/// gutter paints a "phantom" empty cell and the two columns stay aligned).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlignedRow {
    pub kind: RowKind,
    /// 1-based line number on the left, if this row occupies a left line.
    pub left_no: Option<usize>,
    /// 1-based line number on the right, if this row occupies a right line.
    pub right_no: Option<usize>,
    pub left: Option<String>,
    pub right: Option<String>,
    /// Index of the change-block this row belongs to (used for prev/next-change
    /// navigation and the minimap). `None` for unchanged rows.
    pub block: Option<usize>,
}

impl AlignedRow {
    pub fn is_change(&self) -> bool {
        self.kind != RowKind::Equal
    }
}

/// A contiguous run of changed rows — the unit prev/next navigation jumps
/// between and the minimap renders as a single marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChangeBlock {
    /// Inclusive first row index in [`DiffModel::rows`].
    pub first_row: usize,
    /// Inclusive last row index.
    pub last_row: usize,
    pub kind: BlockKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Added,
    Removed,
    Modified,
}

/// A complete two-way comparison ready to render.
#[derive(Clone, Debug, Default)]
pub struct DiffModel {
    pub rows: Vec<AlignedRow>,
    pub blocks: Vec<ChangeBlock>,
    pub left_lines: usize,
    pub right_lines: usize,
    pub added: usize,
    pub removed: usize,
    pub modified: usize,
}

/// One line from the underlying `similar` diff, stripped of the trailing
/// newline `similar` keeps on each line so the renderer measures clean text.
#[derive(Clone)]
struct DiffChange {
    tag: ChangeTag,
    text: String,
    old: Option<usize>,
    new: Option<usize>,
}

/// Run the line-level diff and collect its change stream.
fn collect_changes(left: &str, right: &str) -> Vec<DiffChange> {
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Patience)
        .diff_lines(left, right);

    diff.iter_all_changes()
        .map(|ch| DiffChange {
            tag: ch.tag(),
            text: ch.value().trim_end_matches(['\n', '\r']).to_string(),
            old: ch.old_index().map(|i| i + 1),
            new: ch.new_index().map(|i| i + 1),
        })
        .collect()
}

/// Walk the change stream and pair adjacent Delete/Insert runs into `Modify`
/// rows so a changed line shows old-vs-new side by side instead of as two
/// separate add/remove rows. Leftover deletes/inserts become single-sided
/// rows.
fn align_rows(changes: &[DiffChange]) -> Vec<AlignedRow> {
    let mut rows: Vec<AlignedRow> = Vec::new();
    let mut i = 0usize;
    while let Some(c) = changes.get(i) {
        match c.tag {
            ChangeTag::Equal => {
                rows.push(AlignedRow {
                    kind: RowKind::Equal,
                    left_no: c.old,
                    right_no: c.new,
                    left: Some(c.text.clone()),
                    right: Some(c.text.clone()),
                    block: None,
                });
                i += 1;
            }
            ChangeTag::Delete => {
                // Gather the full delete run, then the immediately following
                // insert run; zip them into Modify rows.
                let del_start = i;
                while matches!(changes.get(i), Some(next) if next.tag == ChangeTag::Delete) {
                    i += 1;
                }
                let del_end = i;
                let ins_start = i;
                while matches!(changes.get(i), Some(next) if next.tag == ChangeTag::Insert) {
                    i += 1;
                }
                let ins_end = i;

                let dels = changes.get(del_start..del_end).unwrap_or(&[]);
                let inss = changes.get(ins_start..ins_end).unwrap_or(&[]);
                let paired = dels.len().min(inss.len());
                for (d, ins) in dels.iter().zip(inss.iter()) {
                    rows.push(AlignedRow {
                        kind: RowKind::Modify,
                        left_no: d.old,
                        right_no: ins.new,
                        left: Some(d.text.clone()),
                        right: Some(ins.text.clone()),
                        block: None,
                    });
                }
                for d in dels.iter().skip(paired) {
                    rows.push(AlignedRow {
                        kind: RowKind::Delete,
                        left_no: d.old,
                        right_no: None,
                        left: Some(d.text.clone()),
                        right: None,
                        block: None,
                    });
                }
                for ins in inss.iter().skip(paired) {
                    rows.push(AlignedRow {
                        kind: RowKind::Insert,
                        left_no: None,
                        right_no: ins.new,
                        left: None,
                        right: Some(ins.text.clone()),
                        block: None,
                    });
                }
            }
            ChangeTag::Insert => {
                rows.push(AlignedRow {
                    kind: RowKind::Insert,
                    left_no: None,
                    right_no: c.new,
                    left: None,
                    right: Some(c.text.clone()),
                    block: None,
                });
                i += 1;
            }
        }
    }
    rows
}

/// Group contiguous change-rows into blocks and stamp the block index back
/// onto each row.
fn group_blocks(rows: &mut [AlignedRow]) -> Vec<ChangeBlock> {
    let mut blocks: Vec<ChangeBlock> = Vec::new();
    let mut r = 0usize;
    while let Some(row) = rows.get(r) {
        if !row.is_change() {
            r += 1;
            continue;
        }
        let first = r;
        let mut has_add = false;
        let mut has_del = false;
        while let Some(row) = rows.get(r) {
            if !row.is_change() {
                break;
            }
            match row.kind {
                RowKind::Insert => has_add = true,
                RowKind::Delete => has_del = true,
                RowKind::Modify => {
                    has_add = true;
                    has_del = true;
                }
                RowKind::Equal => {}
            }
            r += 1;
        }
        let last = r - 1;
        let kind = match (has_add, has_del) {
            (true, false) => BlockKind::Added,
            (false, true) => BlockKind::Removed,
            (true, true) | (false, false) => BlockKind::Modified,
        };
        let idx = blocks.len();
        for row in rows.iter_mut().take(last + 1).skip(first) {
            row.block = Some(idx);
        }
        blocks.push(ChangeBlock {
            first_row: first,
            last_row: last,
            kind,
        });
    }
    blocks
}

impl DiffModel {
    /// Compute a line-aligned diff of `left` vs `right`.
    pub fn compute(left: &str, right: &str) -> Self {
        let changes = collect_changes(left, right);
        let mut rows = align_rows(&changes);
        let blocks = group_blocks(&mut rows);

        let added = rows.iter().filter(|r| r.kind == RowKind::Insert).count();
        let removed = rows.iter().filter(|r| r.kind == RowKind::Delete).count();
        let modified = rows.iter().filter(|r| r.kind == RowKind::Modify).count();

        Self {
            rows,
            blocks,
            left_lines: left.lines().count(),
            right_lines: right.lines().count(),
            added,
            removed,
            modified,
        }
    }

    pub fn is_identical(&self) -> bool {
        self.blocks.is_empty()
    }
}

// ===========================================================================
// Three-way merge
// ===========================================================================

/// Which side a resolved merge region took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Region is unchanged from base (both sides equal here).
    Unchanged,
    /// Auto-resolved: only `ours` changed this base region.
    Ours,
    /// Auto-resolved: only `theirs` changed this base region.
    Theirs,
    TookOurs,
    TookTheirs,
    /// A conflict the user has resolved by taking both (ours then theirs).
    TookBoth,
    /// A change resolved with text of its own ([`MergeHunk::written`]).
    Written,
    /// A still-open conflict.
    Conflict,
}

impl Resolution {
    pub fn is_conflict(&self) -> bool {
        matches!(self, Resolution::Conflict)
    }
}

/// One merge region: either a clean run carried straight through, or a hunk
/// where ours and/or theirs diverged from base.
#[derive(Clone, Debug)]
pub struct MergeHunk {
    pub base: Vec<String>,
    pub ours: Vec<String>,
    pub theirs: Vec<String>,
    pub resolution: Resolution,
    /// True when this hunk is a real divergence (vs. a passthrough equal run).
    pub is_change: bool,
    /// The lines a [`Resolution::Written`] hunk contributes.
    pub written: Vec<String>,
}

impl MergeHunk {
    /// The lines this hunk contributes to the merged output given its current
    /// resolution. Conflicts contribute their `ours` side as a provisional
    /// best-effort (so the buffer is always renderable) but report unresolved.
    pub fn resolved_lines(&self) -> Vec<String> {
        match self.resolution {
            Resolution::Unchanged => self.base.clone(),
            // Conflict is provisional: show ours until the user decides.
            Resolution::Ours | Resolution::TookOurs | Resolution::Conflict => self.ours.clone(),
            Resolution::Theirs | Resolution::TookTheirs => self.theirs.clone(),
            Resolution::TookBoth => {
                let mut v = self.ours.clone();
                v.extend(self.theirs.clone());
                v
            }
            Resolution::Written => self.written.clone(),
        }
    }
}

/// Mutable state threaded through the base/gap sweep in [`MergeModel::compute`]
/// that accumulates clean runs and divergent segments into hunks.
#[derive(Default)]
struct MergeBuilder {
    hunks: Vec<MergeHunk>,
    clean_run: Vec<String>,
    seg_base: Vec<String>,
    seg_ours: Vec<String>,
    seg_theirs: Vec<String>,
    seg_any_ours: bool,
    seg_any_theirs: bool,
    seg_open: bool,
}

impl MergeBuilder {
    fn new() -> Self {
        Self::default()
    }

    /// Flush an accumulated run of clean base lines as a passthrough hunk.
    fn flush_clean(&mut self) {
        if self.clean_run.is_empty() {
            return;
        }
        let lines = std::mem::take(&mut self.clean_run);
        self.hunks.push(MergeHunk {
            base: lines.clone(),
            ours: lines.clone(),
            theirs: lines,
            resolution: Resolution::Unchanged,
            is_change: false,
            written: Vec::new(),
        });
    }

    /// Close the currently-open divergent segment (if any) into a hunk,
    /// classifying it as an auto-resolve or a real conflict.
    fn close_segment(&mut self) {
        if !self.seg_open {
            return;
        }
        let resolution = if self.seg_any_ours && self.seg_any_theirs {
            if self.seg_ours == self.seg_theirs {
                Resolution::Ours
            } else {
                Resolution::Conflict
            }
        } else if self.seg_any_ours {
            Resolution::Ours
        } else {
            Resolution::Theirs
        };
        self.hunks.push(MergeHunk {
            base: std::mem::take(&mut self.seg_base),
            ours: std::mem::take(&mut self.seg_ours),
            theirs: std::mem::take(&mut self.seg_theirs),
            resolution,
            is_change: true,
            written: Vec::new(),
        });
        self.seg_any_ours = false;
        self.seg_any_theirs = false;
        self.seg_open = false;
    }

    /// Fold in the insertions (if any) sitting in the gap before a base line
    /// (or the trailing gap when called for the tail).
    fn push_gap(&mut self, ours_ins: &[String], theirs_ins: &[String]) {
        if ours_ins.is_empty() && theirs_ins.is_empty() {
            return;
        }
        self.flush_clean();
        self.seg_open = true;
        self.seg_ours.extend(ours_ins.iter().cloned());
        self.seg_theirs.extend(theirs_ins.iter().cloned());
        self.seg_any_ours |= !ours_ins.is_empty();
        self.seg_any_theirs |= !theirs_ins.is_empty();
    }

    /// Fold in one base line: either it passes through clean, or it (and its
    /// per-side replacements) join the open divergent segment.
    fn push_line(&mut self, base_line: &str, orep: Option<&[String]>, trep: Option<&[String]>) {
        if orep.is_none() && trep.is_none() {
            self.close_segment();
            self.clean_run.push(base_line.to_string());
            return;
        }
        self.flush_clean();
        self.seg_open = true;
        self.seg_base.push(base_line.to_string());
        // Replacement (None => keep base line on that side).
        match orep {
            Some(lines) => {
                self.seg_ours.extend(lines.iter().cloned());
                self.seg_any_ours = true;
            }
            None => self.seg_ours.push(base_line.to_string()),
        }
        match trep {
            Some(lines) => {
                self.seg_theirs.extend(lines.iter().cloned());
                self.seg_any_theirs = true;
            }
            None => self.seg_theirs.push(base_line.to_string()),
        }
    }

    /// Finish the sweep: flush any still-open segment/run and return the
    /// resulting hunks.
    fn finish(mut self) -> Vec<MergeHunk> {
        self.close_segment();
        self.flush_clean();
        self.hunks
    }
}

/// A three-way merge: the hunk list plus convenience counters.
#[derive(Clone, Debug, Default)]
pub struct MergeModel {
    pub hunks: Vec<MergeHunk>,
}

impl MergeModel {
    /// Compute a three-way merge. The algorithm diffs base→ours and base→theirs
    /// into per-side `SideAlign` structures (a replacement slot per base line
    /// plus an insertion slot at every gap, including the trailing gap), then
    /// sweeps the base — and the gaps between base lines — grouping regions that
    /// either side touched into hunks. Keeping replacements and insertions in
    /// separate slots is what prevents trailing/boundary inserts from being
    /// double-counted.
    pub fn compute(base: &str, ours: &str, theirs: &str) -> Self {
        let base_lines: Vec<String> = split_lines(base);
        let ours_align = SideAlign::build(&base_lines, &split_lines(ours));
        let theirs_align = SideAlign::build(&base_lines, &split_lines(theirs));

        let n = base_lines.len();
        let mut builder = MergeBuilder::new();

        // Interleave gaps and base lines: gap(0), line(0), gap(1), line(1),
        // …, line(n-1), gap(n). A "unit" is either a gap insertion region or a
        // base line. The builder classifies each unit as clean or divergent,
        // and coalesces consecutive divergent units into a single hunk.
        for i in 0..=n {
            let oi = ours_align
                .inserts_before
                .get(i)
                .map_or(&[][..], Vec::as_slice);
            let ti = theirs_align
                .inserts_before
                .get(i)
                .map_or(&[][..], Vec::as_slice);
            builder.push_gap(oi, ti);

            let Some(base_line) = base_lines.get(i) else {
                break;
            };
            let orep = ours_align.replace.get(i).and_then(Option::as_deref);
            let trep = theirs_align.replace.get(i).and_then(Option::as_deref);
            builder.push_line(base_line, orep, trep);
        }

        Self {
            hunks: builder.finish(),
        }
    }

    /// Indices of hunks that are real changes (conflicts or auto-resolves).
    pub fn change_indices(&self) -> Vec<usize> {
        self.hunks
            .iter()
            .enumerate()
            .filter(|(_, h)| h.is_change)
            .map(|(i, _)| i)
            .collect()
    }

    pub fn conflict_count(&self) -> usize {
        self.hunks
            .iter()
            .filter(|h| h.resolution.is_conflict())
            .count()
    }

    pub fn resolved_count(&self) -> usize {
        self.hunks
            .iter()
            .filter(|h| h.is_change && !h.resolution.is_conflict())
            .count()
    }

    pub fn change_count(&self) -> usize {
        self.hunks.iter().filter(|h| h.is_change).count()
    }

    pub fn fully_resolved(&self) -> bool {
        self.conflict_count() == 0
    }

    /// The merged output as a single string with trailing newline per line.
    pub fn merged_text(&self) -> String {
        let mut out = String::new();
        for h in &self.hunks {
            for line in h.resolved_lines() {
                out.push_str(&line);
                out.push('\n');
            }
        }
        out
    }

    /// The merged output as a flat line list (no terminators).
    pub fn merged_lines(&self) -> Vec<String> {
        self.hunks
            .iter()
            .flat_map(MergeHunk::resolved_lines)
            .collect()
    }
}

/// Split into lines, dropping the line terminator. Empty input → empty vec.
fn split_lines(s: &str) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    s.lines().map(std::string::ToString::to_string).collect()
}

/// One side of the merge aligned against base.
///
/// * `replace[i]` — `Some(lines)` when base line `i` was modified or deleted on
///   this side (`lines` is the replacement, possibly empty for a deletion);
///   `None` when base line `i` is carried through unchanged.
/// * `inserts_before[g]` — lines this side inserts in gap `g`, where gap `g`
///   sits *before* base line `g` (and gap `n` is the trailing append region).
///   Length is `base.len() + 1`.
struct SideAlign {
    replace: Vec<Option<Vec<String>>>,
    inserts_before: Vec<Vec<String>>,
}

impl SideAlign {
    fn build(base: &[String], side: &[String]) -> Self {
        let n = base.len();
        let mut replace: Vec<Option<Vec<String>>> = vec![None; n];
        let mut inserts_before: Vec<Vec<String>> = vec![Vec::new(); n + 1];

        let base_text = join(base);
        let side_text = join(side);
        let diff = TextDiff::configure()
            .algorithm(Algorithm::Patience)
            .diff_lines(&base_text, &side_text);

        // Walk the structured ops. Each op carries a base (old) range and a
        // side (new) range.
        for op in diff.ops() {
            use similar::DiffOp::{Delete, Equal, Insert, Replace};
            match *op {
                Equal { .. } => {}
                Delete {
                    old_index, old_len, ..
                } => {
                    if let Some(slice) = replace.get_mut(old_index..old_index + old_len) {
                        for slot in slice {
                            *slot = Some(Vec::new());
                        }
                    }
                }
                Insert {
                    old_index,
                    new_index,
                    new_len,
                } => {
                    let lines = side
                        .get(new_index..new_index + new_len)
                        .map(<[String]>::to_vec)
                        .unwrap_or_default();
                    // Insertion attaches to the gap before base line old_index.
                    if let Some(slot) = inserts_before.get_mut(old_index.min(n)) {
                        slot.extend(lines);
                    }
                }
                Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } => {
                    let lines = side
                        .get(new_index..new_index + new_len)
                        .map(<[String]>::to_vec)
                        .unwrap_or_default();
                    // The whole replacement maps onto the first replaced base
                    // line; the remaining replaced base lines become deletions
                    // so the segment spans the entire base range.
                    if old_len == 0 {
                        if let Some(slot) = inserts_before.get_mut(old_index.min(n)) {
                            slot.extend(lines);
                        }
                    } else {
                        if let Some(slot) = replace.get_mut(old_index) {
                            *slot = Some(lines);
                        }
                        if let Some(slice) = replace.get_mut(old_index + 1..old_index + old_len) {
                            for slot in slice {
                                *slot = Some(Vec::new());
                            }
                        }
                    }
                }
            }
        }

        Self {
            replace,
            inserts_before,
        }
    }
}

fn join(lines: &[String]) -> String {
    let mut s = String::new();
    for l in lines {
        s.push_str(l);
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One two-way diff input pair and every property the case pins down.
    struct DiffCase {
        name: &'static str,
        left: &'static str,
        right: &'static str,
        identical: bool,
        added: Option<usize>,
        removed: Option<usize>,
        modified: Option<usize>,
        /// A row of this kind exists and carries exactly these sides.
        row: Option<(RowKind, Option<&'static str>, Option<&'static str>)>,
        /// The exact change blocks, in order.
        blocks: Option<Vec<BlockKind>>,
    }

    /// Two-way diff classification: identical inputs have no blocks, pure
    /// insertions/deletions have one empty side, a modification pairs old and
    /// new, and contiguous changes group into one block.
    #[test]
    fn diff_classifies_rows_counts_and_blocks() {
        let cases = [
            DiffCase {
                name: "identical inputs have no blocks",
                left: "a\nb\nc\n",
                right: "a\nb\nc\n",
                identical: true,
                added: Some(0),
                removed: Some(0),
                modified: Some(0),
                row: None,
                blocks: None,
            },
            DiffCase {
                // The inserted row has no left, a right.
                name: "pure insertion",
                left: "a\nc\n",
                right: "a\nb\nc\n",
                identical: false,
                added: Some(1),
                removed: Some(0),
                modified: None,
                row: Some((RowKind::Insert, None, Some("b"))),
                blocks: None,
            },
            DiffCase {
                name: "pure deletion",
                left: "a\nb\nc\n",
                right: "a\nc\n",
                identical: false,
                added: Some(0),
                removed: Some(1),
                modified: None,
                row: Some((RowKind::Delete, Some("b"), None)),
                blocks: None,
            },
            DiffCase {
                name: "modification pairs old and new",
                left: "hello\nworld\n",
                right: "hello\nthere\n",
                identical: false,
                added: None,
                removed: None,
                modified: Some(1),
                row: Some((RowKind::Modify, Some("world"), Some("there"))),
                blocks: None,
            },
            DiffCase {
                // b,c modified to X,Y form one block.
                name: "blocks group contiguous changes",
                left: "a\nb\nc\nd\n",
                right: "a\nX\nY\nd\n",
                identical: false,
                added: None,
                removed: None,
                modified: None,
                row: None,
                blocks: Some(vec![BlockKind::Modified]),
            },
        ];
        for case in cases {
            let name = case.name;
            let m = DiffModel::compute(case.left, case.right);
            assert_eq!(m.is_identical(), case.identical, "case {name}: identical");
            if case.identical {
                assert!(
                    m.rows.iter().all(|r| r.kind == RowKind::Equal),
                    "case {name}: every row is Equal"
                );
            }
            if let Some(added) = case.added {
                assert_eq!(m.added, added, "case {name}: added");
            }
            if let Some(removed) = case.removed {
                assert_eq!(m.removed, removed, "case {name}: removed");
            }
            if let Some(modified) = case.modified {
                assert_eq!(m.modified, modified, "case {name}: modified");
            }
            if let Some((kind, left, right)) = case.row {
                let row = m
                    .rows
                    .iter()
                    .find(|r| r.kind == kind)
                    .unwrap_or_else(|| panic!("case {name}: a {kind:?} row"));
                assert_eq!(row.left.as_deref(), left, "case {name}: row left");
                assert_eq!(row.right.as_deref(), right, "case {name}: row right");
            }
            if let Some(blocks) = case.blocks {
                assert_eq!(
                    m.blocks.iter().map(|b| b.kind).collect::<Vec<_>>(),
                    blocks,
                    "case {name}: blocks"
                );
            }
        }
    }

    /// A resolution applied to the (single) conflict hunk of a case, with what
    /// the merge then reports.
    struct Take {
        resolution: Resolution,
        fully_resolved: Option<bool>,
        merged: Vec<&'static str>,
    }

    struct MergeCase {
        name: &'static str,
        base: &'static str,
        ours: &'static str,
        theirs: &'static str,
        conflicts: usize,
        fully_resolved: Option<bool>,
        merged: Option<Vec<&'static str>>,
        /// The conflict hunk carries both candidate sides.
        conflict_sides: Option<(Vec<&'static str>, Vec<&'static str>)>,
        /// Each applied to a freshly computed model.
        takes: Vec<Take>,
    }

    /// Three-way merge: one-sided changes auto-resolve to the changed side,
    /// identical edits are not conflicts, divergent edits (including trailing
    /// inserts) conflict, and taking theirs / both rewrites the output.
    #[allow(
        clippy::too_many_lines,
        reason = "case table: every row is one original merge case, kept verbatim"
    )]
    #[test]
    fn merge_auto_resolves_or_conflicts_and_resolutions_rewrite_output() {
        let cases = [
            MergeCase {
                // Only ours changes line 2; theirs unchanged → auto Ours, no conflict.
                name: "auto resolves one-sided ours change",
                base: "a\nb\nc\n",
                ours: "a\nB\nc\n",
                theirs: "a\nb\nc\n",
                conflicts: 0,
                fully_resolved: Some(true),
                merged: Some(vec!["a", "B", "c"]),
                conflict_sides: None,
                takes: vec![],
            },
            MergeCase {
                name: "auto resolves theirs change",
                base: "a\nb\nc\n",
                ours: "a\nb\nc\n",
                theirs: "a\nb\nC\n",
                conflicts: 0,
                fully_resolved: None,
                merged: Some(vec!["a", "b", "C"]),
                conflict_sides: None,
                takes: vec![],
            },
            MergeCase {
                name: "real conflict",
                base: "a\nb\nc\n",
                ours: "a\nOURS\nc\n",
                theirs: "a\nTHEIRS\nc\n",
                conflicts: 1,
                fully_resolved: Some(false),
                merged: None,
                conflict_sides: Some((vec!["OURS"], vec!["THEIRS"])),
                takes: vec![
                    Take {
                        resolution: Resolution::TookTheirs,
                        fully_resolved: Some(true),
                        merged: vec!["a", "THEIRS", "c"],
                    },
                    Take {
                        resolution: Resolution::TookBoth,
                        fully_resolved: None,
                        merged: vec!["a", "OURS", "THEIRS", "c"],
                    },
                ],
            },
            MergeCase {
                // Both sides made the SAME edit — not a conflict.
                name: "identical sides are no conflict",
                base: "a\nb\nc\n",
                ours: "a\nSAME\nc\n",
                theirs: "a\nSAME\nc\n",
                conflicts: 0,
                fully_resolved: None,
                merged: Some(vec!["a", "SAME", "c"]),
                conflict_sides: None,
                takes: vec![],
            },
            MergeCase {
                name: "trailing insert on both sides conflicts",
                base: "a\nb\n",
                ours: "a\nb\nOURS_TAIL\n",
                theirs: "a\nb\nTHEIRS_TAIL\n",
                conflicts: 1,
                fully_resolved: None,
                merged: None,
                conflict_sides: None,
                takes: vec![],
            },
        ];
        for case in cases {
            let name = case.name;
            let m = MergeModel::compute(case.base, case.ours, case.theirs);
            assert_eq!(m.conflict_count(), case.conflicts, "case {name}: conflicts");
            if let Some(fully) = case.fully_resolved {
                assert_eq!(m.fully_resolved(), fully, "case {name}: fully resolved");
            }
            if let Some(merged) = &case.merged {
                assert_eq!(&m.merged_lines(), merged, "case {name}: merged lines");
            }
            if let Some((ours, theirs)) = &case.conflict_sides {
                let conflict = m
                    .hunks
                    .iter()
                    .find(|h| h.resolution.is_conflict())
                    .unwrap_or_else(|| panic!("case {name}: a conflict hunk"));
                assert_eq!(&conflict.ours, ours, "case {name}: conflict ours");
                assert_eq!(&conflict.theirs, theirs, "case {name}: conflict theirs");
            }
            for take in case.takes {
                let label = format!("case {name}, {:?}", take.resolution);
                let mut m = MergeModel::compute(case.base, case.ours, case.theirs);
                let idx = m
                    .hunks
                    .iter()
                    .position(|h| h.resolution.is_conflict())
                    .unwrap_or_else(|| panic!("{label}: a conflict hunk"));
                m.hunks[idx].resolution = take.resolution;
                if let Some(fully) = take.fully_resolved {
                    assert_eq!(m.fully_resolved(), fully, "{label}: fully resolved");
                }
                assert_eq!(m.merged_lines(), take.merged, "{label}: merged lines");
            }
        }
    }
}

#[cfg(test)]
mod comparison_cache_tests {
    #[test]
    fn exact_input_reuses_alignment_and_either_changed_side_invalidates_it() {
        let original = super::comparison("left\n", "right\n");
        let same = super::comparison("left\n", "right\n");
        assert!(std::sync::Arc::ptr_eq(&original, &same));
        let changed_left = super::comparison("other\n", "right\n");
        assert!(!std::sync::Arc::ptr_eq(&original, &changed_left));
        let changed_right = super::comparison("left\n", "left\n");
        assert_eq!(changed_right.modified, 0);
        assert!(changed_right.blocks.is_empty());
        let evicted = super::comparison("left\n", "right\n");
        assert!(
            !std::sync::Arc::ptr_eq(&original, &evicted),
            "only two comparisons are retained"
        );
    }
}
