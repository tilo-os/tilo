//! The diff and merge model: a two-way line alignment for the side-by-side view, and
//! a three-way merge whose conflicts the person resolves per hunk. No rendering, no IO;
//! hunking is the `similar` crate's Myers/patience diff.

use similar::{Algorithm, ChangeTag, TextDiff};

thread_local! {
    static COMPARISONS: std::cell::RefCell<std::collections::VecDeque<ComparisonCache>> =
        const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
}

thread_local! {
    static DIGEST_PASSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times this thread has digested a comparison's content: the
/// counter a window's test reads to prove an unchanged comparison is not
/// rehashed.
pub fn digest_passes() -> usize {
    DIGEST_PASSES.get()
}

struct ComparisonCache {
    key: (blake3::Hash, blake3::Hash),
    model: std::sync::Arc<DiffModel>,
}

/// The alignment of this exact content, shared by the resource and its view: both
/// source digests are part of it, so an edit cannot reuse the previous comparison.
pub fn comparison(left: &str, right: &str) -> std::sync::Arc<DiffModel> {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// The kind of change one aligned row is.
pub enum RowKind {
    Equal,
    Insert,
    Delete,
    Modify,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// One row of a side-by-side diff: each side's line, or `None` where it has none,
/// so both columns stay aligned.
pub struct AlignedRow {
    /// The row's kind.
    pub kind: RowKind,
    /// The 1-based line number on the left, when the row occupies one.
    pub left_no: Option<usize>,
    /// The 1-based line number on the right, when the row occupies one.
    pub right_no: Option<usize>,
    pub left: Option<String>,
    pub right: Option<String>,
    /// The change block the row belongs to; `None` for an unchanged row.
    pub block: Option<usize>,
}

impl AlignedRow {
    pub fn is_change(&self) -> bool {
        self.kind != RowKind::Equal
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// A contiguous run of changed rows: what prev/next navigation jumps between and the
/// minimap marks.
pub struct ChangeBlock {
    /// The first row, inclusive.
    pub first_row: usize,
    /// The last row, inclusive.
    pub last_row: usize,
    pub kind: BlockKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// What kind of change a block is.
pub enum BlockKind {
    Added,
    Removed,
    Modified,
}

#[derive(Clone, Debug, Default)]
/// A two-way comparison, ready to render.
pub struct DiffModel {
    pub rows: Vec<AlignedRow>,
    pub blocks: Vec<ChangeBlock>,
    pub left_lines: usize,
    pub right_lines: usize,
    pub added: usize,
    pub removed: usize,
    pub modified: usize,
}

#[derive(Clone)]
struct DiffChange {
    tag: ChangeTag,
    text: String,
    old: Option<usize>,
    new: Option<usize>,
}

fn collect_changes(left: &str, right: &str) -> Vec<DiffChange> {
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Patience)
        .diff_lines(left, right);

    diff.iter_all_changes()
        .map(|ch| DiffChange {
            tag: ch.tag(),
            text: ch.value().trim_end_matches(['\n', '\r']).to_string(),
            old: ch.old_index().map(|i| i.saturating_add(1)),
            new: ch.new_index().map(|i| i.saturating_add(1)),
        })
        .collect()
}

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
                i = i.saturating_add(1);
            }
            ChangeTag::Delete => {
                let del_start = i;
                while matches!(changes.get(i), Some(next) if next.tag == ChangeTag::Delete) {
                    i = i.saturating_add(1);
                }
                let del_end = i;
                let ins_start = i;
                while matches!(changes.get(i), Some(next) if next.tag == ChangeTag::Insert) {
                    i = i.saturating_add(1);
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
                i = i.saturating_add(1);
            }
        }
    }
    rows
}

fn group_blocks(rows: &mut [AlignedRow]) -> Vec<ChangeBlock> {
    let mut blocks: Vec<ChangeBlock> = Vec::new();
    let mut r = 0usize;
    while let Some(row) = rows.get(r) {
        if !row.is_change() {
            r = r.saturating_add(1);
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
            r = r.saturating_add(1);
        }
        let last = r.saturating_sub(1);
        let kind = match (has_add, has_del) {
            (true, false) => BlockKind::Added,
            (false, true) => BlockKind::Removed,
            (true, true) | (false, false) => BlockKind::Modified,
        };
        let idx = blocks.len();
        for row in rows.iter_mut().take(last.saturating_add(1)).skip(first) {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// How a merge region was resolved.
pub enum Resolution {
    Unchanged,
    Ours,
    Theirs,
    TookOurs,
    TookTheirs,
    TookBoth,
    Written,
    Conflict,
}

impl Resolution {
    pub fn is_conflict(&self) -> bool {
        matches!(self, Resolution::Conflict)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Ending {
    #[default]
    Lf,
    CrLf,
}

impl Ending {
    fn as_str(self) -> &'static str {
        match self {
            Ending::Lf => "\n",
            Ending::CrLf => "\r\n",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Term {
    #[default]
    Native,
    Lf,
    CrLf,
}

impl Term {
    fn as_str(self, ending: Ending) -> &'static str {
        match self {
            Term::Native => ending.as_str(),
            Term::Lf => "\n",
            Term::CrLf => "\r\n",
        }
    }
}

fn merge3<T: PartialEq + Copy>(base: T, ours: T, theirs: T) -> T {
    if ours == base { theirs } else { ours }
}

fn merge_said<T: PartialEq + Copy>(
    base: Option<T>,
    ours: Option<T>,
    theirs: Option<T>,
    or: T,
) -> T {
    let b = base.or(theirs).or(ours).unwrap_or(or);
    merge3(b, ours.unwrap_or(b), theirs.unwrap_or(b))
}

/// One input of a merge: its lines, and how each of them ends.
struct Parsed {
    lines: Vec<String>,
    raw: Vec<Option<Ending>>,
    terms: Vec<Term>,
    ending: Option<Ending>,
    final_newline: Option<bool>,
}

impl Parsed {
    fn new(s: &str) -> Self {
        let mut lines = Vec::new();
        let mut raw = Vec::new();
        for piece in s.split_inclusive('\n') {
            let (text, end) = match piece.strip_suffix('\n') {
                Some(text) => match text.strip_suffix('\r') {
                    Some(text) => (text, Some(Ending::CrLf)),
                    None => (text, Some(Ending::Lf)),
                },
                None => (piece, None),
            };
            lines.push(text.to_owned());
            raw.push(end);
        }
        let crlf = raw.iter().filter(|e| **e == Some(Ending::CrLf)).count();
        let lf = raw.iter().filter(|e| **e == Some(Ending::Lf)).count();
        let ending = match (crlf, lf) {
            (0, 0) => None,
            (crlf, lf) if crlf > lf => Some(Ending::CrLf),
            _ => Some(Ending::Lf),
        };
        let mut parsed = Self {
            lines,
            raw,
            terms: Vec::new(),
            ending: None,
            final_newline: (!s.is_empty()).then_some(s.ends_with('\n')),
        };
        parsed.relate_to(ending);
        parsed
    }

    fn relate_to(&mut self, ending: Option<Ending>) {
        self.ending = ending;
        self.terms = self
            .raw
            .iter()
            .map(|end| match end {
                Some(end) if Some(*end) != ending => match end {
                    Ending::Lf => Term::Lf,
                    Ending::CrLf => Term::CrLf,
                },
                _ => Term::Native,
            })
            .collect();
    }

    fn ending_against(&self, base: &Parsed, ops: &[similar::DiffOp]) -> Option<Ending> {
        let (mut kept, mut converted) = (0usize, 0usize);
        for op in ops {
            if let similar::DiffOp::Equal {
                old_index,
                new_index,
                len,
            } = *op
            {
                let was = base
                    .raw
                    .get(old_index..old_index.saturating_add(len))
                    .unwrap_or(&[]);
                let now = self
                    .raw
                    .get(new_index..new_index.saturating_add(len))
                    .unwrap_or(&[]);
                for (was, now) in was.iter().zip(now) {
                    if let (Some(was), Some(now)) = (was, now) {
                        kept = kept.saturating_add(1);
                        converted = converted.saturating_add(usize::from(was != now));
                    }
                }
            }
        }
        if kept == 0 || base.ending.is_none() || converted.saturating_mul(2) > kept {
            self.ending
        } else {
            base.ending
        }
    }
}

#[derive(Clone, Debug)]
/// One region of a merge: a clean run carried through, or a place where ours and/or
/// theirs diverged from base.
pub struct MergeHunk {
    pub base: Vec<String>,
    pub ours: Vec<String>,
    pub theirs: Vec<String>,
    pub resolution: Resolution,
    /// Whether this hunk is a real divergence rather than a clean run.
    pub is_change: bool,
    /// The lines a [`Resolution::Written`] hunk contributes.
    pub written: Vec<String>,
    terms: [Vec<Term>; 3],
}

impl MergeHunk {
    /// The lines this hunk contributes given its resolution. A conflict contributes its
    /// ours side provisionally, so the buffer is always renderable.
    pub fn resolved_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.contribute(self.resolution, &self.written, &mut out);
        out.into_iter().map(|(line, _)| line.to_owned()).collect()
    }

    fn contribute<'a>(
        &'a self,
        resolution: Resolution,
        written: &'a [String],
        out: &mut Vec<(&'a str, Term)>,
    ) {
        let [base, ours, theirs] = &self.terms;
        let mut side = |lines: &'a [String], terms: &'a [Term]| {
            out.extend(
                lines
                    .iter()
                    .map(String::as_str)
                    .zip(terms.iter().copied().chain(std::iter::repeat(Term::Native))),
            );
        };
        match resolution {
            Resolution::Unchanged => side(&self.base, base),
            Resolution::Ours | Resolution::TookOurs | Resolution::Conflict => {
                side(&self.ours, ours);
            }
            Resolution::Theirs | Resolution::TookTheirs => side(&self.theirs, theirs),
            Resolution::TookBoth => {
                side(&self.ours, ours);
                side(&self.theirs, theirs);
            }
            Resolution::Written => side(written, &[]),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Run {
    lines: Vec<String>,
    terms: Vec<Term>,
}

impl Run {
    fn of(side: &Parsed, from: usize, len: usize) -> Self {
        let range = from..from.saturating_add(len);
        Self {
            lines: side
                .lines
                .get(range.clone())
                .map(<[String]>::to_vec)
                .unwrap_or_default(),
            terms: side
                .terms
                .get(range)
                .map(<[Term]>::to_vec)
                .unwrap_or_default(),
        }
    }

    fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    fn push(&mut self, line: &str, term: Term) {
        self.lines.push(line.to_owned());
        self.terms.push(term);
    }

    fn extend(&mut self, other: &Run) {
        self.lines.extend(other.lines.iter().cloned());
        self.terms.extend(other.terms.iter().copied());
    }
}

#[derive(Clone, Copy)]
enum SideAt<'a> {
    Kept(Term),
    Replaced(&'a Run),
}

#[derive(Default)]
struct MergeBuilder {
    hunks: Vec<MergeHunk>,
    clean_run: Run,
    seg_base: Run,
    seg_ours: Run,
    seg_theirs: Run,
    seg_any_ours: bool,
    seg_any_theirs: bool,
    seg_open: bool,
}

impl MergeBuilder {
    fn new() -> Self {
        Self::default()
    }

    fn flush_clean(&mut self) {
        if self.clean_run.is_empty() {
            return;
        }
        let Run { lines, terms } = std::mem::take(&mut self.clean_run);
        self.hunks.push(MergeHunk {
            base: lines.clone(),
            ours: lines.clone(),
            theirs: lines,
            resolution: Resolution::Unchanged,
            is_change: false,
            written: Vec::new(),
            terms: [terms.clone(), terms.clone(), terms],
        });
    }

    /// Close the open divergent segment into a hunk. Only the text decides: two sides
    /// that differ in how lines end agree.
    fn close_segment(&mut self) {
        if !self.seg_open {
            return;
        }
        let resolution = if self.seg_any_ours && self.seg_any_theirs {
            if self.seg_ours.lines == self.seg_theirs.lines {
                Resolution::Ours
            } else {
                Resolution::Conflict
            }
        } else if self.seg_any_ours {
            Resolution::Ours
        } else {
            Resolution::Theirs
        };
        let base = std::mem::take(&mut self.seg_base);
        let ours = std::mem::take(&mut self.seg_ours);
        let theirs = std::mem::take(&mut self.seg_theirs);
        self.hunks.push(MergeHunk {
            base: base.lines,
            ours: ours.lines,
            theirs: theirs.lines,
            resolution,
            is_change: true,
            written: Vec::new(),
            terms: [base.terms, ours.terms, theirs.terms],
        });
        self.seg_any_ours = false;
        self.seg_any_theirs = false;
        self.seg_open = false;
    }

    fn push_gap(&mut self, ours_ins: &Run, theirs_ins: &Run) {
        if ours_ins.is_empty() && theirs_ins.is_empty() {
            return;
        }
        self.flush_clean();
        self.seg_open = true;
        self.seg_ours.extend(ours_ins);
        self.seg_theirs.extend(theirs_ins);
        self.seg_any_ours |= !ours_ins.is_empty();
        self.seg_any_theirs |= !theirs_ins.is_empty();
    }

    fn push_line(&mut self, (line, term): (&str, Term), ours: SideAt<'_>, theirs: SideAt<'_>) {
        if let (SideAt::Kept(o), SideAt::Kept(t)) = (ours, theirs) {
            self.close_segment();
            self.clean_run.push(line, merge3(term, o, t));
            return;
        }
        self.flush_clean();
        self.seg_open = true;
        self.seg_base.push(line, term);
        for (seg, any, at) in [
            (&mut self.seg_ours, &mut self.seg_any_ours, ours),
            (&mut self.seg_theirs, &mut self.seg_any_theirs, theirs),
        ] {
            match at {
                SideAt::Replaced(run) => {
                    seg.extend(run);
                    *any = true;
                }
                SideAt::Kept(term) => seg.push(line, term),
            }
        }
    }

    fn finish(mut self) -> Vec<MergeHunk> {
        self.close_segment();
        self.flush_clean();
        self.hunks
    }
}

#[derive(Clone, Debug, Default)]
/// A three-way merge: its hunks, and how the merged file ends its lines.
pub struct MergeModel {
    /// The hunks, in file order.
    pub hunks: Vec<MergeHunk>,
    ending: Ending,
    final_newline: bool,
}

impl MergeModel {
    /// Merge `base` against `ours` and `theirs`: base is diffed against each side, and
    /// the base with the gaps between its lines is swept into hunks. Lines compare by
    /// text alone; how they end is merged beside them.
    pub fn compute(base: &str, ours: &str, theirs: &str) -> Self {
        let base = Parsed::new(base);
        let (ours, ours_align) = SideAlign::of(&base, ours);
        let (theirs, theirs_align) = SideAlign::of(&base, theirs);

        let n = base.lines.len();
        let mut builder = MergeBuilder::new();
        let empty = Run::default();

        for i in 0..=n {
            builder.push_gap(
                ours_align.inserts_before.get(i).unwrap_or(&empty),
                theirs_align.inserts_before.get(i).unwrap_or(&empty),
            );

            let Some(line) = base.lines.get(i) else {
                break;
            };
            let term = base.terms.get(i).copied().unwrap_or_default();
            builder.push_line((line, term), ours_align.at(i), theirs_align.at(i));
        }

        Self {
            hunks: builder.finish(),
            ending: merge_said(base.ending, ours.ending, theirs.ending, Ending::Lf),
            final_newline: merge_said(
                base.final_newline,
                ours.final_newline,
                theirs.final_newline,
                true,
            ),
        }
    }

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

    pub fn merged_text(&self) -> String {
        self.text(None)
    }

    pub fn merged_text_with(
        &self,
        hunk: usize,
        resolution: Resolution,
        written: &[String],
    ) -> String {
        self.text(Some((hunk, resolution, written)))
    }

    fn text(&self, substitute: Option<(usize, Resolution, &[String])>) -> String {
        let mut lines = Vec::new();
        for (i, h) in self.hunks.iter().enumerate() {
            match substitute {
                Some((at, resolution, written)) if at == i => {
                    h.contribute(resolution, written, &mut lines);
                }
                _ => h.contribute(h.resolution, &h.written, &mut lines),
            }
        }
        let size = lines
            .iter()
            .map(|(line, _)| line.len().saturating_add(2))
            .sum();
        let mut out = String::with_capacity(size);
        let last = lines.len().saturating_sub(1);
        for (i, (line, term)) in lines.into_iter().enumerate() {
            out.push_str(line);
            if i < last || self.final_newline {
                out.push_str(term.as_str(self.ending));
            }
        }
        out
    }

    pub fn merged_lines(&self) -> Vec<String> {
        self.hunks
            .iter()
            .flat_map(MergeHunk::resolved_lines)
            .collect()
    }
}

/// One side aligned against base: `replace[i]` is the run that replaced or deleted base
/// line `i`, and `inserts_before[g]` the lines inserted before base line `g`.
struct SideAlign {
    replace: Vec<Option<Run>>,
    kept: Vec<Term>,
    inserts_before: Vec<Run>,
}

impl SideAlign {
    fn of(base: &Parsed, text: &str) -> (Parsed, Self) {
        let mut side = Parsed::new(text);
        let ops = similar::capture_diff_slices(Algorithm::Patience, &base.lines, &side.lines);
        side.relate_to(side.ending_against(base, &ops));
        let align = Self::build(base.lines.len(), &side, &ops);
        (side, align)
    }

    fn build(n: usize, side: &Parsed, ops: &[similar::DiffOp]) -> Self {
        let mut replace: Vec<Option<Run>> = vec![None; n];
        let mut kept: Vec<Term> = vec![Term::Native; n];
        let mut inserts_before: Vec<Run> = vec![Run::default(); n.saturating_add(1)];

        for op in ops {
            use similar::DiffOp::{Delete, Equal, Insert, Replace};
            match *op {
                Equal {
                    old_index,
                    new_index,
                    len,
                } => {
                    let terms = side
                        .terms
                        .get(new_index..new_index.saturating_add(len))
                        .unwrap_or(&[]);
                    if let Some(slots) = kept.get_mut(old_index..old_index.saturating_add(len)) {
                        for (slot, term) in slots.iter_mut().zip(terms) {
                            *slot = *term;
                        }
                    }
                }
                Delete {
                    old_index, old_len, ..
                } => {
                    if let Some(slice) =
                        replace.get_mut(old_index..old_index.saturating_add(old_len))
                    {
                        for slot in slice {
                            *slot = Some(Run::default());
                        }
                    }
                }
                Insert {
                    old_index,
                    new_index,
                    new_len,
                } => {
                    if let Some(slot) = inserts_before.get_mut(old_index.min(n)) {
                        slot.extend(&Run::of(side, new_index, new_len));
                    }
                }
                Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } => {
                    let run = Run::of(side, new_index, new_len);
                    if old_len == 0 {
                        if let Some(slot) = inserts_before.get_mut(old_index.min(n)) {
                            slot.extend(&run);
                        }
                    } else {
                        if let Some(slot) = replace.get_mut(old_index) {
                            *slot = Some(run);
                        }
                        if let Some(slice) = replace
                            .get_mut(old_index.saturating_add(1)..old_index.saturating_add(old_len))
                        {
                            for slot in slice {
                                *slot = Some(Run::default());
                            }
                        }
                    }
                }
            }
        }

        Self {
            replace,
            kept,
            inserts_before,
        }
    }

    fn at(&self, i: usize) -> SideAt<'_> {
        match self.replace.get(i).and_then(Option::as_ref) {
            Some(run) => SideAt::Replaced(run),
            None => SideAt::Kept(self.kept.get(i).copied().unwrap_or_default()),
        }
    }
}
