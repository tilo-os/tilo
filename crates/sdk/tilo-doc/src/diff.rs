//! Structural version diff between two states of the same document. A CRDT-native
//! document has stable block identity, so the versions align by id
//! ([`tilo_resource::diff::align`]) instead of fuzzy text matching, or by position
//! when they share none.

use crate::document::MarkSpan;
use crate::snapshot::{AttrVal, BlockView, DocSnapshot};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub use tilo_resource::diff::{RunOp, TextRun, text_runs};
use tilo_resource::diff::{Step, align};
use tilo_sort::CompactSort as _;

/// The full structural diff between two versions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocDiff {
    /// Body-block changes in reading order (new-version order for kept and added blocks; a removed
    /// block sits after the surviving block it followed).
    pub blocks: Vec<BlockDiff>,
    pub properties: Vec<PropChange>,
    pub summary: DiffSummary,
}

impl DocDiff {
    pub fn is_empty(&self) -> bool {
        self.blocks.iter().all(|b| b.op == BlockOp::Equal) && self.properties.is_empty()
    }

    /// Only the blocks that actually changed.
    pub fn changed_blocks(&self) -> impl Iterator<Item = &BlockDiff> {
        self.blocks.iter().filter(|b| b.op != BlockOp::Equal)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DiffSummary {
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
    pub unchanged: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropChange {
    pub name: String,
    pub old: Option<String>,
    pub new: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockOp {
    Added,
    Removed,
    Changed,
    /// Present in both, identical (kept as context; filter with [`DocDiff::changed_blocks`]).
    Equal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockDiff {
    pub id: String,
    pub op: BlockOp,
    pub kind: String,
    #[serde(default)]
    pub kind_change: Option<(String, String)>,
    #[serde(default)]
    pub old_text: Option<String>,
    #[serde(default)]
    pub new_text: Option<String>,
    #[serde(default)]
    pub runs: Vec<TextRun>,
    #[serde(default)]
    pub marks_added: Vec<MarkSpan>,
    #[serde(default)]
    pub marks_removed: Vec<MarkSpan>,
    #[serde(default)]
    pub attr_changes: Vec<AttrChange>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttrChange {
    pub key: String,
    pub old: Option<AttrVal>,
    pub new: Option<AttrVal>,
}

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

/// Diff two ordered block lists, so a caller can diff header, footer or note sequences with the
/// same primitive.
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

/// Positional fallback: pair blocks by index, used when the versions share no block ids.
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

/// Marks present only in `new` and only in `old`; a mark's identity is its full `(start, end, name,
/// value)` tuple.
fn diff_marks(old: &[MarkSpan], new: &[MarkSpan]) -> (Vec<MarkSpan>, Vec<MarkSpan>) {
    let added = new.iter().filter(|m| !old.contains(m)).cloned().collect();
    let removed = old.iter().filter(|m| !new.contains(m)).cloned().collect();
    (added, removed)
}

fn diff_attrs(old: &BTreeMap<String, AttrVal>, new: &BTreeMap<String, AttrVal>) -> Vec<AttrChange> {
    let mut out = Vec::new();
    let mut keys: Vec<&String> = old.keys().chain(new.keys()).collect();
    keys.compact_sort();
    keys.dedup();
    for k in keys {
        let ov = old.get(k);
        let nv = new.get(k);
        if ov != nv {
            out.push(AttrChange {
                key: k.clone(),
                old: ov.cloned(),
                new: nv.cloned(),
            });
        }
    }
    out
}

fn diff_props(old: &BTreeMap<String, String>, new: &BTreeMap<String, String>) -> Vec<PropChange> {
    let mut out = Vec::new();
    let mut keys: Vec<&String> = old.keys().chain(new.keys()).collect();
    keys.compact_sort();
    keys.dedup();
    for k in keys {
        let ov = old.get(k);
        let nv = new.get(k);
        if ov != nv {
            out.push(PropChange {
                name: k.clone(),
                old: ov.cloned(),
                new: nv.cloned(),
            });
        }
    }
    out
}

// Spreadsheet diff: sheets align by their stable sheet id, cells by their A1 reference.

use crate::sheet::snapshot::{CellView, SheetView, WorkbookSnapshot};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetDiff {
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

/// Diff two workbook snapshots; sheets align by their stable id, and within a shared sheet cells by
/// A1 reference.
pub fn diff_workbooks(old: &WorkbookSnapshot, new: &WorkbookSnapshot) -> SheetDiff {
    let mut summary = SheetDiffSummary::default();
    let mut out = Vec::new();

    let old_by_id: BTreeMap<&str, &SheetView> =
        old.sheets.iter().map(|s| (s.id.as_str(), s)).collect();
    let new_by_id: BTreeMap<&str, &SheetView> =
        new.sheets.iter().map(|s| (s.id.as_str(), s)).collect();

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
    refs.compact_sort();
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
