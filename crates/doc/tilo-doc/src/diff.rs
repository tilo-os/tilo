//! Structural version diff between two states of the **same** document.
//!
//! A CRDT-native document has stable block identity — a paragraph keeps its id across every edit
//! and save/load — so two versions align by id instead of fuzzy text matching.
//! The diff includes added, removed, and changed blocks, plus run-level changes
//! within changed blocks. Blocks align by id ([`tilo_resource::diff::align`])
//! when the versions share any; otherwise they pair by position.

use crate::document::MarkSpan;
use crate::snapshot::{AttrVal, BlockView, DocSnapshot};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub use tilo_resource::diff::{RunOp, TextRun, text_runs};
use tilo_resource::diff::{Step, align};

/// The full structural diff of a document between two versions (`old` → `new`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocDiff {
    /// Body-block changes, in reading order (new-version order for kept/added blocks; a removed block sits after the surviving block it followed).
    pub blocks: Vec<BlockDiff>,
    /// Document-property changes (`title`/`author`/… → old/new), keyed by name; only properties that actually changed appear.
    pub properties: Vec<PropChange>,
    /// A one-line tally for a header/summary line.
    pub summary: DiffSummary,
}

impl DocDiff {
    /// Whether the two versions are structurally identical (no block or property changes).
    pub fn is_empty(&self) -> bool {
        self.blocks.iter().all(|b| b.op == BlockOp::Equal) && self.properties.is_empty()
    }

    /// Only the blocks that actually changed (drops the `Equal` context blocks).
    pub fn changed_blocks(&self) -> impl Iterator<Item = &BlockDiff> {
        self.blocks.iter().filter(|b| b.op != BlockOp::Equal)
    }
}

/// Counts for a quick summary line (e.g. "+3 −1 ~2").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DiffSummary {
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
    pub unchanged: usize,
}

/// One document property that changed between versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropChange {
    pub name: String,
    pub old: Option<String>,
    pub new: Option<String>,
}

/// How a block relates the old version to the new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockOp {
    Added,
    Removed,
    Changed,
    /// Present in both, identical (kept as context; filter with [`DocDiff::changed_blocks`]).
    Equal,
}

/// The diff for a single body block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockDiff {
    /// The block id (stable across versions when aligned by id).
    pub id: String,
    pub op: BlockOp,
    /// The block kind in the new version (or the old version for a `Removed` block).
    pub kind: String,
    /// Set on a `Changed` block whose kind changed (e.g. paragraph → heading): `(old_kind, new_kind)`.
    #[serde(default)]
    pub kind_change: Option<(String, String)>,
    /// The old block text (`Removed`/`Changed`; `None` for `Added`).
    #[serde(default)]
    pub old_text: Option<String>,
    /// The new block text (`Added`/`Changed`; `None` for `Removed`).
    #[serde(default)]
    pub new_text: Option<String>,
    /// Character-level run diff of the text (only for `Changed`); for `Added`/`Removed` the whole text is implied by `new_text`/`old_text`.
    #[serde(default)]
    pub runs: Vec<TextRun>,
    /// Inline marks (bold/italic/link/…) that were added going old → new.
    #[serde(default)]
    pub marks_added: Vec<MarkSpan>,
    /// Inline marks that were removed going old → new.
    #[serde(default)]
    pub marks_removed: Vec<MarkSpan>,
    /// Block attribute changes (`style`/`level`/`align`/… → old/new). Only attributes that changed appear.
    #[serde(default)]
    pub attr_changes: Vec<AttrChange>,
}

/// One block attribute that changed between versions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttrChange {
    pub key: String,
    pub old: Option<AttrVal>,
    pub new: Option<AttrVal>,
}

/// Diff two whole-document snapshots (`old` → `new`). See the module docs for the alignment strategy.
pub fn diff_snapshots(old: &DocSnapshot, new: &DocSnapshot) -> DocDiff {
    let blocks = diff_block_lists(&old.blocks, &new.blocks);
    let properties = diff_props(&old.properties, &new.properties);
    let mut summary = DiffSummary::default();
    for b in &blocks {
        match b.op {
            BlockOp::Added => summary.added = summary.added.saturating_add(1),
            BlockOp::Removed => summary.removed = summary.removed.saturating_add(1),
            BlockOp::Changed => summary.changed = summary.changed.saturating_add(1),
            BlockOp::Equal => summary.unchanged = summary.unchanged.saturating_add(1),
        }
    }
    DocDiff {
        blocks,
        properties,
        summary,
    }
}

/// Diff two ordered block lists. Public so a caller can diff header/footer/note block sequences with the same primitive.
pub fn diff_block_lists(old: &[BlockView], new: &[BlockView]) -> Vec<BlockDiff> {
    let old_ids: Vec<&str> = old.iter().map(|b| b.id.as_str()).collect();
    let new_ids: Vec<&str> = new.iter().map(|b| b.id.as_str()).collect();

    // Do the two versions share any block identity at all?
    let shared = {
        let oldset: std::collections::HashSet<&str> = old_ids.iter().copied().collect();
        new_ids.iter().any(|id| oldset.contains(id))
    };
    if !shared && !old.is_empty() && !new.is_empty() {
        return diff_positional(old, new);
    }

    let old_by_id: BTreeMap<&str, &BlockView> = old.iter().map(|b| (b.id.as_str(), b)).collect();
    let new_by_id: BTreeMap<&str, &BlockView> = new.iter().map(|b| (b.id.as_str(), b)).collect();

    let mut out = Vec::new();
    for step in align(&old_ids, &new_ids) {
        match step {
            Step::Both(id) => {
                if let (Some(&ob), Some(&nb)) = (old_by_id.get(*id), new_by_id.get(*id)) {
                    out.push(compare_blocks(ob, nb));
                }
            }
            Step::OldOnly(id) => {
                if let Some(&ob) = old_by_id.get(*id) {
                    out.push(removed_block(ob));
                }
            }
            Step::NewOnly(id) => {
                if let Some(&nb) = new_by_id.get(*id) {
                    out.push(added_block(nb));
                }
            }
        }
    }
    out
}

/// Positional fallback: pair blocks by index (used when the two versions share no block ids). Extra blocks on either side are added/removed.
fn diff_positional(old: &[BlockView], new: &[BlockView]) -> Vec<BlockDiff> {
    let mut out = Vec::new();
    let n = old.len().min(new.len());
    for i in 0..n {
        if let (Some(ob), Some(nb)) = (old.get(i), new.get(i)) {
            out.push(compare_blocks(ob, nb));
        }
    }
    for b in new.get(n..).unwrap_or(&[]) {
        out.push(added_block(b));
    }
    for b in old.get(n..).unwrap_or(&[]) {
        out.push(removed_block(b));
    }
    out
}

fn added_block(b: &BlockView) -> BlockDiff {
    BlockDiff {
        id: b.id.clone(),
        op: BlockOp::Added,
        kind: b.kind.clone(),
        kind_change: None,
        old_text: None,
        new_text: Some(b.text.clone()),
        runs: Vec::new(),
        marks_added: b.marks.clone(),
        marks_removed: Vec::new(),
        attr_changes: b
            .attrs
            .iter()
            .map(|(k, v)| AttrChange {
                key: k.clone(),
                old: None,
                new: Some(v.clone()),
            })
            .collect(),
    }
}

fn removed_block(b: &BlockView) -> BlockDiff {
    BlockDiff {
        id: b.id.clone(),
        op: BlockOp::Removed,
        kind: b.kind.clone(),
        kind_change: None,
        old_text: Some(b.text.clone()),
        new_text: None,
        runs: Vec::new(),
        marks_added: Vec::new(),
        marks_removed: b.marks.clone(),
        attr_changes: b
            .attrs
            .iter()
            .map(|(k, v)| AttrChange {
                key: k.clone(),
                old: Some(v.clone()),
                new: None,
            })
            .collect(),
    }
}

/// Compare two blocks that align (same id, or same position in the fallback).
fn compare_blocks(old: &BlockView, new: &BlockView) -> BlockDiff {
    let kind_change = (old.kind != new.kind).then(|| (old.kind.clone(), new.kind.clone()));
    let text_changed = old.text != new.text;
    let (marks_added, marks_removed) = diff_marks(&old.marks, &new.marks);
    let attr_changes = diff_attrs(&old.attrs, &new.attrs);

    let changed = kind_change.is_some()
        || text_changed
        || !marks_added.is_empty()
        || !marks_removed.is_empty()
        || !attr_changes.is_empty();

    BlockDiff {
        id: new.id.clone(),
        op: if changed {
            BlockOp::Changed
        } else {
            BlockOp::Equal
        },
        kind: new.kind.clone(),
        kind_change,
        old_text: text_changed.then(|| old.text.clone()),
        new_text: text_changed.then(|| new.text.clone()),
        runs: if text_changed {
            text_runs(&old.text, &new.text)
        } else {
            Vec::new()
        },
        marks_added,
        marks_removed,
        attr_changes,
    }
}

/// Marks present only in `new` (added) and only in `old` (removed). A mark's identity is its full `(start, end, name, value)` tuple.
fn diff_marks(old: &[MarkSpan], new: &[MarkSpan]) -> (Vec<MarkSpan>, Vec<MarkSpan>) {
    let added = new.iter().filter(|m| !old.contains(m)).cloned().collect();
    let removed = old.iter().filter(|m| !new.contains(m)).cloned().collect();
    (added, removed)
}

fn diff_attrs(old: &BTreeMap<String, AttrVal>, new: &BTreeMap<String, AttrVal>) -> Vec<AttrChange> {
    diff_maps(old, new, |key, old, new| AttrChange { key, old, new })
}

fn diff_props(old: &BTreeMap<String, String>, new: &BTreeMap<String, String>) -> Vec<PropChange> {
    diff_maps(old, new, |name, old, new| PropChange { name, old, new })
}

/// One `change(key, old, new)` per key whose value differs between `old` and `new`, in key order.
fn diff_maps<V: Clone + PartialEq, C>(
    old: &BTreeMap<String, V>,
    new: &BTreeMap<String, V>,
    change: impl Fn(String, Option<V>, Option<V>) -> C,
) -> Vec<C> {
    let mut keys: Vec<&String> = old.keys().chain(new.keys()).collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .filter_map(|k| {
            let ov = old.get(k);
            let nv = new.get(k);
            (ov != nv).then(|| change(k.clone(), ov.cloned(), nv.cloned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Document;

    fn snap_of(d: &Document) -> DocSnapshot {
        d.materialize()
    }

    #[test]
    fn block_diffs_pair_by_id_or_position_and_keep_runs_minimal() {
        // Phase `detects_added_removed_changed_blocks`.
        {
            let mut doc = Document::new();
            let alpha_id = doc.insert_block(None, "paragraph");
            doc.splice_text(&alpha_id, 0, 0, "alpha");
            let beta_id = doc.insert_block(Some(&alpha_id), "paragraph");
            doc.splice_text(&beta_id, 0, 0, "beta");
            let gamma_id = doc.insert_block(Some(&beta_id), "paragraph");
            doc.splice_text(&gamma_id, 0, 0, "gamma");
            let v1 = snap_of(&doc);

            doc.splice_text(&beta_id, 0, 4, "BETA"); // "beta" -> "BETA"
            doc.remove_block(&gamma_id);
            let epsilon_id = doc.insert_block(Some(&alpha_id), "paragraph");
            doc.splice_text(&epsilon_id, 0, 0, "epsilon");
            let v2 = snap_of(&doc);

            let diff = diff_snapshots(&v1, &v2);
            assert_eq!(diff.summary.added, 1, "one new paragraph");
            assert_eq!(diff.summary.removed, 1, "one removed paragraph");
            assert_eq!(diff.summary.changed, 1, "one edited paragraph");

            let changed: Vec<_> = diff.changed_blocks().collect();
            let edited = changed.iter().find(|b| b.op == BlockOp::Changed).unwrap();
            assert_eq!(edited.id, beta_id);
            assert_eq!(edited.old_text.as_deref(), Some("beta"));
            assert_eq!(edited.new_text.as_deref(), Some("BETA"));
            assert!(
                edited
                    .runs
                    .iter()
                    .any(|r| r.op == RunOp::Insert && r.text == "BETA")
            );

            let added = changed.iter().find(|x| x.op == BlockOp::Added).unwrap();
            assert_eq!(added.new_text.as_deref(), Some("epsilon"));
            let removed = changed.iter().find(|x| x.op == BlockOp::Removed).unwrap();
            assert_eq!(removed.id, gamma_id);
            assert_eq!(removed.old_text.as_deref(), Some("gamma"));
        }
        // Phase `identical_snapshots_diff_empty`.
        {
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "same");
            let v = snap_of(&d);
            let diff = diff_snapshots(&v, &v);
            assert!(diff.is_empty(), "a snapshot vs itself has no changes");
            assert_eq!(diff.summary.changed, 0);
        }
        // Phase `positional_fallback_across_independent_docs`.
        {
            // Two docs with no shared ids still diff by position.
            let mut a = Document::new();
            let pa = a.insert_block(None, "paragraph");
            a.splice_text(&pa, 0, 0, "first line");
            let mut b = Document::new();
            let pb = b.insert_block(None, "paragraph");
            b.splice_text(&pb, 0, 0, "first LINE");

            let diff = diff_snapshots(&snap_of(&a), &snap_of(&b));
            assert_eq!(
                diff.summary.changed, 1,
                "positional pairing found the edited paragraph"
            );
            assert!(diff.blocks[0].runs.iter().any(|r| r.op == RunOp::Insert));
        }
        // Phase `char_run_diff_is_minimal`.
        {
            let runs = text_runs("the quick fox", "the quick brown fox");
            assert!(
                runs.iter()
                    .any(|r| r.op == RunOp::Insert && r.text.contains("brown"))
            );
            let equal: String = runs
                .iter()
                .filter(|r| r.op == RunOp::Equal)
                .map(|r| r.text.clone())
                .collect();
            assert!(equal.contains("the quick"));
            assert!(equal.contains("fox"));
        }
    }

    #[test]
    fn mark_attr_and_property_changes_are_reported() {
        // Phase `detects_mark_and_attr_changes`.
        {
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "hello world");
            let v1 = snap_of(&d);
            d.add_mark(&p, 0, 5, "strong", true);
            let v2 = snap_of(&d);

            let diff = diff_snapshots(&v1, &v2);
            let ch = diff.changed_blocks().next().unwrap();
            assert_eq!(ch.op, BlockOp::Changed);
            assert!(
                ch.marks_added.iter().any(|m| m.name == "strong"),
                "bold added surfaced"
            );
            assert!(ch.marks_removed.is_empty());
        }
        // Phase `property_changes_are_reported`.
        {
            let mut d = Document::new();
            d.set_doc_prop("title", "Draft");
            let v1 = snap_of(&d);
            d.set_doc_prop("title", "Final");
            d.set_doc_prop("author", "Ada");
            let v2 = snap_of(&d);

            let diff = diff_snapshots(&v1, &v2);
            let title = diff.properties.iter().find(|p| p.name == "title").unwrap();
            assert_eq!(title.old.as_deref(), Some("Draft"));
            assert_eq!(title.new.as_deref(), Some("Final"));
            assert!(
                diff.properties
                    .iter()
                    .any(|p| p.name == "author" && p.new.as_deref() == Some("Ada"))
            );
        }
    }
}
// Spreadsheet (workbook) structural diff, appended here: same "stable identity → clean structural
// diff" idea as the document diff — sheets align by their stable sheet id, cells by their A1 ref.

use crate::sheet::snapshot::{CellView, SheetView, WorkbookSnapshot};

/// The full structural diff of a workbook between two versions (`old` → `new`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetDiff {
    /// Per-sheet changes, in new-version tab order (a removed sheet trails).
    pub sheets: Vec<SheetChange>,
    pub summary: SheetDiffSummary,
}

impl SheetDiff {
    pub fn is_empty(&self) -> bool {
        self.sheets.iter().all(|s| s.op == SheetOp::Equal)
    }
    pub fn changed_sheets(&self) -> impl Iterator<Item = &SheetChange> {
        self.sheets.iter().filter(|s| s.op != SheetOp::Equal)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SheetDiffSummary {
    pub sheets_added: usize,
    pub sheets_removed: usize,
    pub cells_added: usize,
    pub cells_removed: usize,
    pub cells_changed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SheetOp {
    Added,
    Removed,
    Changed,
    Equal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetChange {
    pub id: String,
    pub name: String,
    pub op: SheetOp,
    #[serde(default)]
    pub name_change: Option<(String, String)>,
    #[serde(default)]
    pub cells: Vec<CellChange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CellOp {
    Added,
    Removed,
    Changed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellChange {
    pub reference: String,
    pub op: CellOp,
    #[serde(default)]
    pub old: Option<CellView>,
    #[serde(default)]
    pub new: Option<CellView>,
}

/// Diff two workbook snapshots (`old` → `new`). Sheets align by their stable id; within a shared sheet, cells align by A1 reference.
pub fn diff_workbooks(old: &WorkbookSnapshot, new: &WorkbookSnapshot) -> SheetDiff {
    let mut summary = SheetDiffSummary::default();
    let mut out = Vec::new();

    let old_by_id: BTreeMap<&str, &SheetView> =
        old.sheets.iter().map(|s| (s.id.as_str(), s)).collect();
    let new_by_id: BTreeMap<&str, &SheetView> =
        new.sheets.iter().map(|s| (s.id.as_str(), s)).collect();

    // New + shared sheets, in new tab order.
    for ns in &new.sheets {
        if let Some(os) = old_by_id.get(ns.id.as_str()) {
            let change = diff_sheet(os, ns);
            if change.op == SheetOp::Changed {
                for c in &change.cells {
                    match c.op {
                        CellOp::Added => {
                            summary.cells_added = summary.cells_added.saturating_add(1);
                        }
                        CellOp::Removed => {
                            summary.cells_removed = summary.cells_removed.saturating_add(1);
                        }
                        CellOp::Changed => {
                            summary.cells_changed = summary.cells_changed.saturating_add(1);
                        }
                    }
                }
            }
            out.push(change);
        } else {
            summary.sheets_added = summary.sheets_added.saturating_add(1);
            summary.cells_added = summary.cells_added.saturating_add(ns.cells.len());
            out.push(SheetChange {
                id: ns.id.clone(),
                name: ns.name.clone(),
                op: SheetOp::Added,
                name_change: None,
                cells: ns
                    .cells
                    .iter()
                    .map(|c| CellChange {
                        reference: c.reference.clone(),
                        op: CellOp::Added,
                        old: None,
                        new: Some(c.clone()),
                    })
                    .collect(),
            });
        }
    }
    // Removed sheets (present only in old).
    for os in &old.sheets {
        if !new_by_id.contains_key(os.id.as_str()) {
            summary.sheets_removed = summary.sheets_removed.saturating_add(1);
            summary.cells_removed = summary.cells_removed.saturating_add(os.cells.len());
            out.push(SheetChange {
                id: os.id.clone(),
                name: os.name.clone(),
                op: SheetOp::Removed,
                name_change: None,
                cells: os
                    .cells
                    .iter()
                    .map(|c| CellChange {
                        reference: c.reference.clone(),
                        op: CellOp::Removed,
                        old: Some(c.clone()),
                        new: None,
                    })
                    .collect(),
            });
        }
    }
    SheetDiff {
        sheets: out,
        summary,
    }
}

fn diff_sheet(old: &SheetView, new: &SheetView) -> SheetChange {
    let name_change = (old.name != new.name).then(|| (old.name.clone(), new.name.clone()));
    let old_cells: BTreeMap<&str, &CellView> = old
        .cells
        .iter()
        .map(|c| (c.reference.as_str(), c))
        .collect();
    let new_cells: BTreeMap<&str, &CellView> = new
        .cells
        .iter()
        .map(|c| (c.reference.as_str(), c))
        .collect();

    let mut refs: Vec<&str> = old_cells.keys().chain(new_cells.keys()).copied().collect();
    refs.sort_unstable();
    refs.dedup();

    let mut cells = Vec::new();
    for r in refs {
        match (old_cells.get(r), new_cells.get(r)) {
            (Some(oc), Some(nc)) if oc != nc => cells.push(CellChange {
                reference: r.to_string(),
                op: CellOp::Changed,
                old: Some((*oc).clone()),
                new: Some((*nc).clone()),
            }),
            (None, Some(nc)) => cells.push(CellChange {
                reference: r.to_string(),
                op: CellOp::Added,
                old: None,
                new: Some((*nc).clone()),
            }),
            (Some(oc), None) => cells.push(CellChange {
                reference: r.to_string(),
                op: CellOp::Removed,
                old: Some((*oc).clone()),
                new: None,
            }),
            (Some(_), Some(_)) | (None, None) => {}
        }
    }
    let op = if name_change.is_some() || !cells.is_empty() {
        SheetOp::Changed
    } else {
        SheetOp::Equal
    };
    SheetChange {
        id: new.id.clone(),
        name: new.name.clone(),
        op,
        name_change,
        cells,
    }
}

#[cfg(test)]
mod sheet_diff_tests {
    use super::*;
    use crate::{CellInput, CellType, Workbook};

    #[test]
    fn workbook_diffs_report_cell_and_sheet_changes() {
        // Phase `workbook_diff_reports_added_changed_removed_cells`.
        {
            let mut a = Workbook::new();
            let s = a.add_sheet("S");
            a.set_cell(&s, "A1", CellInput::number("1"));
            a.set_cell(&s, "A2", CellInput::formula("A1+1", "2", CellType::Number));
            let v1 = a.materialize();

            a.set_cell(&s, "A1", CellInput::number("9")); // changed
            a.set_cell(&s, "B1", CellInput::text("new")); // added
            a.clear_cell(&s, "A2"); // removed
            let v2 = a.materialize();

            let diff = diff_workbooks(&v1, &v2);
            assert_eq!(diff.summary.cells_changed, 1, "A1 changed");
            assert_eq!(diff.summary.cells_added, 1, "B1 added");
            assert_eq!(diff.summary.cells_removed, 1, "A2 removed");
            let sheet = diff.changed_sheets().next().unwrap();
            assert!(
                sheet
                    .cells
                    .iter()
                    .any(|c| c.reference == "A1" && c.op == CellOp::Changed)
            );
            assert!(
                sheet
                    .cells
                    .iter()
                    .any(|c| c.reference == "B1" && c.op == CellOp::Added)
            );
            assert!(
                sheet
                    .cells
                    .iter()
                    .any(|c| c.reference == "A2" && c.op == CellOp::Removed)
            );
            let json = serde_json::to_string(&diff).expect("sheet diff serializes to JSON");
            assert!(json.contains("A1") && json.contains("B1"));
        }
        // Phase `workbook_diff_detects_added_and_removed_sheets`.
        {
            let mut a = Workbook::new();
            a.add_sheet("Keep");
            let v1 = a.materialize();
            let s2 = a.add_sheet("Added");
            a.set_cell(&s2, "A1", CellInput::number("1"));
            let v2 = a.materialize();
            let diff = diff_workbooks(&v1, &v2);
            assert_eq!(diff.summary.sheets_added, 1);
            assert!(
                diff.sheets
                    .iter()
                    .any(|s| s.name == "Added" && s.op == SheetOp::Added)
            );
            assert!(
                diff_workbooks(&v1, &v1).is_empty(),
                "a snapshot vs itself is empty"
            );
        }
    }
}
