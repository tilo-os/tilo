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
//!   both sides changed the same base lines become [`Conflict`] hunks the user
//!   resolves per-hunk (take ours / theirs / both). The resolved output is a
//!   sequence of lines the app mirrors into an Automerge `Text` so the merged
//!   buffer is collaboratively editable.
//!
//! All hunking is built on the `similar` crate's Myers/patience diff.

use similar::{Algorithm, ChangeTag, TextDiff};

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

impl DiffModel {
    /// Compute a line-aligned diff of `left` vs `right`.
    pub fn compute(left: &str, right: &str) -> Self {
        let diff = TextDiff::configure()
            .algorithm(Algorithm::Patience)
            .diff_lines(left, right);

        // Collect the per-line change stream, stripping the trailing newline
        // `similar` keeps on each line so the renderer measures clean text.
        #[derive(Clone)]
        struct Ch {
            tag: ChangeTag,
            text: String,
            old: Option<usize>,
            new: Option<usize>,
        }
        let mut changes: Vec<Ch> = Vec::new();
        for ch in diff.iter_all_changes() {
            changes.push(Ch {
                tag: ch.tag(),
                text: ch.value().trim_end_matches(['\n', '\r']).to_string(),
                old: ch.old_index().map(|i| i + 1),
                new: ch.new_index().map(|i| i + 1),
            });
        }

        // Walk the stream and pair adjacent Delete/Insert runs into Modify
        // rows so a changed line shows old-vs-new side by side instead of as
        // two separate add/remove rows. Leftover deletes/inserts become
        // single-sided rows.
        let mut rows: Vec<AlignedRow> = Vec::new();
        let mut i = 0;
        while i < changes.len() {
            match changes[i].tag {
                ChangeTag::Equal => {
                    let c = &changes[i];
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
                    // Gather the full delete run, then the immediately
                    // following insert run; zip them into Modify rows.
                    let del_start = i;
                    while i < changes.len() && changes[i].tag == ChangeTag::Delete {
                        i += 1;
                    }
                    let del_end = i;
                    let ins_start = i;
                    while i < changes.len() && changes[i].tag == ChangeTag::Insert {
                        i += 1;
                    }
                    let ins_end = i;

                    let dels = &changes[del_start..del_end];
                    let inss = &changes[ins_start..ins_end];
                    let paired = dels.len().min(inss.len());
                    for k in 0..paired {
                        rows.push(AlignedRow {
                            kind: RowKind::Modify,
                            left_no: dels[k].old,
                            right_no: inss[k].new,
                            left: Some(dels[k].text.clone()),
                            right: Some(inss[k].text.clone()),
                            block: None,
                        });
                    }
                    for d in &dels[paired..] {
                        rows.push(AlignedRow {
                            kind: RowKind::Delete,
                            left_no: d.old,
                            right_no: None,
                            left: Some(d.text.clone()),
                            right: None,
                            block: None,
                        });
                    }
                    for ins in &inss[paired..] {
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
                    let c = &changes[i];
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

        // Group contiguous change-rows into blocks and stamp the block index
        // back onto each row.
        let mut blocks: Vec<ChangeBlock> = Vec::new();
        let mut r = 0;
        while r < rows.len() {
            if !rows[r].is_change() {
                r += 1;
                continue;
            }
            let first = r;
            let mut has_add = false;
            let mut has_del = false;
            while r < rows.len() && rows[r].is_change() {
                match rows[r].kind {
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
                (true, true) => BlockKind::Modified,
                (true, false) => BlockKind::Added,
                (false, true) => BlockKind::Removed,
                (false, false) => BlockKind::Modified,
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
    /// A conflict the user has resolved by taking ours.
    TookOurs,
    /// A conflict the user has resolved by taking theirs.
    TookTheirs,
    /// A conflict the user has resolved by taking both (ours then theirs).
    TookBoth,
    /// A still-open conflict.
    Conflict,
}

impl Resolution {
    pub fn is_conflict(&self) -> bool {
        matches!(self, Resolution::Conflict)
    }
    pub fn is_open_or_conflict(&self) -> bool {
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
}

impl MergeHunk {
    /// The lines this hunk contributes to the merged output given its current
    /// resolution. Conflicts contribute their `ours` side as a provisional
    /// best-effort (so the buffer is always renderable) but report unresolved.
    pub fn resolved_lines(&self) -> Vec<String> {
        match self.resolution {
            Resolution::Unchanged => self.base.clone(),
            Resolution::Ours | Resolution::TookOurs => self.ours.clone(),
            Resolution::Theirs | Resolution::TookTheirs => self.theirs.clone(),
            Resolution::TookBoth => {
                let mut v = self.ours.clone();
                v.extend(self.theirs.clone());
                v
            }
            // Provisional: show ours until the user decides.
            Resolution::Conflict => self.ours.clone(),
        }
    }
}

/// A three-way merge: the hunk list plus convenience counters.
#[derive(Clone, Debug, Default)]
pub struct MergeModel {
    pub hunks: Vec<MergeHunk>,
}

impl MergeModel {
    /// Compute a three-way merge. The algorithm diffs base→ours and base→theirs
    /// into per-side [`SideAlign`] structures (a replacement slot per base line
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
        let mut hunks: Vec<MergeHunk> = Vec::new();

        // We interleave gaps and base lines: gap(0), line(0), gap(1), line(1),
        // …, line(n-1), gap(n). A "unit" is either a gap insertion region or a
        // base line. We classify each unit as clean or divergent, then coalesce
        // consecutive divergent units into a single hunk.
        //
        // `push_clean` flushes a run of clean base lines; `push_change` emits a
        // divergent hunk built from the accumulated segment.
        let mut clean_run: Vec<String> = Vec::new();
        let flush_clean = |hunks: &mut Vec<MergeHunk>, run: &mut Vec<String>| {
            if !run.is_empty() {
                let lines = std::mem::take(run);
                hunks.push(MergeHunk {
                    base: lines.clone(),
                    ours: lines.clone(),
                    theirs: lines,
                    resolution: Resolution::Unchanged,
                    is_change: false,
                });
            }
        };

        // Pending divergent segment accumulators.
        let mut seg_base: Vec<String> = Vec::new();
        let mut seg_ours: Vec<String> = Vec::new();
        let mut seg_theirs: Vec<String> = Vec::new();
        let mut seg_any_ours = false;
        let mut seg_any_theirs = false;
        let mut seg_open = false;

        let close_segment = |hunks: &mut Vec<MergeHunk>,
                             base: &mut Vec<String>,
                             ours: &mut Vec<String>,
                             theirs: &mut Vec<String>,
                             any_ours: &mut bool,
                             any_theirs: &mut bool,
                             open: &mut bool| {
            if !*open {
                return;
            }
            let resolution = if *any_ours && *any_theirs {
                if *ours == *theirs {
                    Resolution::Ours
                } else {
                    Resolution::Conflict
                }
            } else if *any_ours {
                Resolution::Ours
            } else {
                Resolution::Theirs
            };
            hunks.push(MergeHunk {
                base: std::mem::take(base),
                ours: std::mem::take(ours),
                theirs: std::mem::take(theirs),
                resolution,
                is_change: true,
            });
            *any_ours = false;
            *any_theirs = false;
            *open = false;
        };

        // Iterate gaps and lines together.
        for i in 0..=n {
            // --- Gap insertions before base line i (i == n is the tail). ---
            let oi = &ours_align.inserts_before[i];
            let ti = &theirs_align.inserts_before[i];
            if !oi.is_empty() || !ti.is_empty() {
                flush_clean(&mut hunks, &mut clean_run);
                seg_open = true;
                seg_ours.extend(oi.clone());
                seg_theirs.extend(ti.clone());
                seg_any_ours |= !oi.is_empty();
                seg_any_theirs |= !ti.is_empty();
            }

            if i == n {
                break;
            }

            // --- Base line i. ---
            let orep = &ours_align.replace[i];
            let trep = &theirs_align.replace[i];
            let o_changed = orep.is_some();
            let t_changed = trep.is_some();

            if !o_changed && !t_changed {
                // Clean line: close any open divergent segment, accrue clean.
                close_segment(
                    &mut hunks,
                    &mut seg_base,
                    &mut seg_ours,
                    &mut seg_theirs,
                    &mut seg_any_ours,
                    &mut seg_any_theirs,
                    &mut seg_open,
                );
                clean_run.push(base_lines[i].clone());
            } else {
                flush_clean(&mut hunks, &mut clean_run);
                seg_open = true;
                seg_base.push(base_lines[i].clone());
                // Replacement (None => keep base line on that side).
                match orep {
                    Some(lines) => {
                        seg_ours.extend(lines.clone());
                        seg_any_ours = true;
                    }
                    None => seg_ours.push(base_lines[i].clone()),
                }
                match trep {
                    Some(lines) => {
                        seg_theirs.extend(lines.clone());
                        seg_any_theirs = true;
                    }
                    None => seg_theirs.push(base_lines[i].clone()),
                }
            }
        }

        // Final flushes.
        close_segment(
            &mut hunks,
            &mut seg_base,
            &mut seg_ours,
            &mut seg_theirs,
            &mut seg_any_ours,
            &mut seg_any_theirs,
            &mut seg_open,
        );
        flush_clean(&mut hunks, &mut clean_run);

        Self { hunks }
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
                    for k in old_index..(old_index + old_len) {
                        replace[k] = Some(Vec::new());
                    }
                }
                Insert {
                    old_index,
                    new_index,
                    new_len,
                } => {
                    let lines: Vec<String> = (new_index..new_index + new_len)
                        .map(|j| side[j].clone())
                        .collect();
                    // Insertion attaches to the gap before base line old_index.
                    inserts_before[old_index.min(n)].extend(lines);
                }
                Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } => {
                    let lines: Vec<String> = (new_index..new_index + new_len)
                        .map(|j| side[j].clone())
                        .collect();
                    // The whole replacement maps onto the first replaced base
                    // line; the remaining replaced base lines become deletions
                    // so the segment spans the entire base range.
                    if old_len == 0 {
                        inserts_before[old_index.min(n)].extend(lines);
                    } else {
                        replace[old_index] = Some(lines);
                        for k in (old_index + 1)..(old_index + old_len) {
                            replace[k] = Some(Vec::new());
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

    #[test]
    fn identical_inputs_have_no_blocks() {
        let m = DiffModel::compute("a\nb\nc\n", "a\nb\nc\n");
        assert!(m.is_identical());
        assert_eq!(m.added, 0);
        assert_eq!(m.removed, 0);
        assert_eq!(m.modified, 0);
        assert!(m.rows.iter().all(|r| r.kind == RowKind::Equal));
    }

    #[test]
    fn pure_insertion_detected() {
        let m = DiffModel::compute("a\nc\n", "a\nb\nc\n");
        assert_eq!(m.added, 1);
        assert_eq!(m.removed, 0);
        // The inserted row has no left, a right.
        let ins = m.rows.iter().find(|r| r.kind == RowKind::Insert).unwrap();
        assert_eq!(ins.right.as_deref(), Some("b"));
        assert!(ins.left.is_none());
    }

    #[test]
    fn pure_deletion_detected() {
        let m = DiffModel::compute("a\nb\nc\n", "a\nc\n");
        assert_eq!(m.removed, 1);
        assert_eq!(m.added, 0);
        let del = m.rows.iter().find(|r| r.kind == RowKind::Delete).unwrap();
        assert_eq!(del.left.as_deref(), Some("b"));
        assert!(del.right.is_none());
    }

    #[test]
    fn modification_pairs_old_and_new() {
        let m = DiffModel::compute("hello\nworld\n", "hello\nthere\n");
        let modi = m.rows.iter().find(|r| r.kind == RowKind::Modify).unwrap();
        assert_eq!(modi.left.as_deref(), Some("world"));
        assert_eq!(modi.right.as_deref(), Some("there"));
        assert_eq!(m.modified, 1);
    }

    #[test]
    fn blocks_group_contiguous_changes() {
        let m = DiffModel::compute("a\nb\nc\nd\n", "a\nX\nY\nd\n");
        // b,c modified to X,Y form one block.
        assert_eq!(m.blocks.len(), 1);
        let block = m.blocks[0];
        assert_eq!(block.kind, BlockKind::Modified);
    }

    #[test]
    fn merge_auto_resolves_one_sided_change() {
        // Only ours changes line 2; theirs unchanged → auto Ours, no conflict.
        let base = "a\nb\nc\n";
        let ours = "a\nB\nc\n";
        let theirs = "a\nb\nc\n";
        let m = MergeModel::compute(base, ours, theirs);
        assert_eq!(m.conflict_count(), 0);
        assert!(m.fully_resolved());
        assert_eq!(m.merged_lines(), vec!["a", "B", "c"]);
    }

    #[test]
    fn merge_auto_resolves_theirs_change() {
        let base = "a\nb\nc\n";
        let ours = "a\nb\nc\n";
        let theirs = "a\nb\nC\n";
        let m = MergeModel::compute(base, ours, theirs);
        assert_eq!(m.conflict_count(), 0);
        assert_eq!(m.merged_lines(), vec!["a", "b", "C"]);
    }

    #[test]
    fn merge_detects_real_conflict() {
        let base = "a\nb\nc\n";
        let ours = "a\nOURS\nc\n";
        let theirs = "a\nTHEIRS\nc\n";
        let m = MergeModel::compute(base, ours, theirs);
        assert_eq!(m.conflict_count(), 1);
        assert!(!m.fully_resolved());
        // The conflict hunk carries both candidate sides.
        let conflict = m.hunks.iter().find(|h| h.resolution.is_conflict()).unwrap();
        assert_eq!(conflict.ours, vec!["OURS"]);
        assert_eq!(conflict.theirs, vec!["THEIRS"]);
    }

    #[test]
    fn conflict_take_theirs_changes_output() {
        let base = "a\nb\nc\n";
        let ours = "a\nOURS\nc\n";
        let theirs = "a\nTHEIRS\nc\n";
        let mut m = MergeModel::compute(base, ours, theirs);
        let idx = m
            .hunks
            .iter()
            .position(|h| h.resolution.is_conflict())
            .unwrap();
        m.hunks[idx].resolution = Resolution::TookTheirs;
        assert!(m.fully_resolved());
        assert_eq!(m.merged_lines(), vec!["a", "THEIRS", "c"]);
    }

    #[test]
    fn conflict_take_both_concatenates() {
        let base = "a\nb\nc\n";
        let ours = "a\nOURS\nc\n";
        let theirs = "a\nTHEIRS\nc\n";
        let mut m = MergeModel::compute(base, ours, theirs);
        let idx = m
            .hunks
            .iter()
            .position(|h| h.resolution.is_conflict())
            .unwrap();
        m.hunks[idx].resolution = Resolution::TookBoth;
        assert_eq!(m.merged_lines(), vec!["a", "OURS", "THEIRS", "c"]);
    }

    #[test]
    fn merge_identical_sides_no_conflict() {
        // Both sides made the SAME edit — not a conflict.
        let base = "a\nb\nc\n";
        let ours = "a\nSAME\nc\n";
        let theirs = "a\nSAME\nc\n";
        let m = MergeModel::compute(base, ours, theirs);
        assert_eq!(m.conflict_count(), 0);
        assert_eq!(m.merged_lines(), vec!["a", "SAME", "c"]);
    }

    #[test]
    fn merge_trailing_insert_both_sides_conflict() {
        let base = "a\nb\n";
        let ours = "a\nb\nOURS_TAIL\n";
        let theirs = "a\nb\nTHEIRS_TAIL\n";
        let m = MergeModel::compute(base, ours, theirs);
        assert_eq!(m.conflict_count(), 1);
    }
}
