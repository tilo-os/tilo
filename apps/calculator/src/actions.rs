//! The calculator's exported **actions** and the **resource tree** it publishes
//! to Ctrl+K.
//!
//! Most of a calculator is ephemeral: the expression being typed, the running
//! display, which mode (Calc / 2D / 3D) is showing, the pan/zoom of a graph, the
//! angle-unit strip, the memory scratch register. None of that is a document
//! mutation — it is presentation, computed or spliced live, and it never becomes
//! an action.
//!
//! What *does* live in the shared Automerge document (see [`crate::doc`]) is a
//! small, curated set of collaborative artifacts:
//!
//! * the evaluated **history** list (`root["history"]`), one row per committed
//!   `=`;
//! * the saved **graph formulas** — the 2D `y = f(x)` curves (`root["funcs"]`)
//!   and the 3D `z = f(x, y)` surfaces (`root["surfaces"]`), each a
//!   visibility-toggleable row whose formula is a per-character CRDT `Text`.
//!
//! Those are the things that should persist, replicate to a collaborator, and
//! step under one Ctrl+Z — so each document-mutating operation on them is a
//! [`CalculatorAction`]: a small serializable value that deterministically
//! mutates the CRDT. The exact same [`EncodedAction`] is what the on-screen
//! affordance (the `=` key, the history "Clear" button, a graph row's add /
//! visibility toggle / delete) dispatches and what the Ctrl+K palette dispatches
//! — one source of truth, one replay path.
//!
//! Per-character editing of a graph formula is **not** an action: like the calc
//! expression line and a notes page body, it splices directly into the row's
//! Automerge `Text` (the CRDT-native, multi-cursor path). Only the discrete
//! "replace this formula" ([`CalculatorAction::SetGraphExpr`], offered by Ctrl+K)
//! is an action.
//!
//! ## Determinism contract
//!
//! [`Action::apply`] is a pure function of the payload and the document's base
//! state: no RNG, no wall-clock, stable iteration. History rows carry their
//! `expr`/`result` in the payload; the history cap and the visibility flip are
//! recomputed from the document at the replay's base state, so two replicas at
//! the same heads land on the byte-identical result. A malformed / out-of-range
//! payload is a **no-op** rather than an error, so a bad replay can't wedge a
//! peer.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use ui::time::UNIX_EPOCH;

use tilo_actions::{
    Action, ActionError, ActionRegistry, Doc, EncodedAction, ExecutionLocus, Resource,
};
use ui::doc::{put_if_changed, put_text_diffed};
use ui::tilo_resource::Resource as TiloResource;

use crate::model::{HistoryEntry, Mode, PlotFn};

/// Registry `type_id` for every calculator action.
pub const CALCULATOR_ACTION_TYPE: &str = "calculator.action";
pub const CALCULATOR_OPERATION_TYPE: &str = "calculator.operation";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CalculatorOperation {
    ShowCalculator,
    ShowGraph2d,
    ShowGraph3d,
    RecallHistory { id: String },
}

/// # Errors
///
/// Returns the JSON decoding error when `type_id` identifies a calculator
/// operation but `payload` is not a valid `CalculatorOperation` envelope.
pub fn decode_operation(
    type_id: &str,
    payload: &[u8],
) -> Result<Option<CalculatorOperation>, String> {
    if type_id != CALCULATOR_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

/// The most recent evaluated computations kept in the shared history list. The
/// front is the newest; overflow is dropped from the tail. Mirrors the model's
/// `history.truncate(64)` so the action and the in-memory model agree.
pub const HISTORY_CAP: usize = 64;

/// Which saved-graph list an action targets. The 2D curves and the 3D surfaces
/// are stored identically (`List<Map { expr: Text, visible: Bool }>`) under two
/// different document keys, so one set of action variants serves both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphKind {
    /// The 2D `y = f(x)` curves in `root["funcs"]`.
    Curve2d,
    /// The 3D `z = f(x, y)` surfaces in `root["surfaces"]`.
    Surface3d,
}

impl GraphKind {
    /// The document list key this kind is stored under.
    fn doc_key(self) -> &'static str {
        match self {
            GraphKind::Curve2d => "funcs",
            GraphKind::Surface3d => "surfaces",
        }
    }

    /// A stable slug for the resource id (`calculator.graph:<slug>:<index>`).
    fn slug(self) -> &'static str {
        match self {
            GraphKind::Curve2d => "curve2d",
            GraphKind::Surface3d => "surface3d",
        }
    }
}

/// Every collaborative operation the calculator exports on its shared history +
/// saved-graph state. Presentation (the live expression, the mode, the view, the
/// memory register) never appears here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CalculatorAction {
    /// Commit an evaluated computation to the front of the history list, then
    /// cap the list to [`HISTORY_CAP`] by dropping the oldest overflow.
    CommitHistory {
        id: String,
        rank: i64,
        expr: String,
        result: String,
    },
    /// Remove one history entry by stable id. A missing id is a no-op.
    DeleteHistoryEntry { id: String },
    /// Empty the whole history list.
    ClearHistory { ids: Vec<String> },
    /// Append a graph row carrying `expr` (empty for a blank starter row, or a
    /// seeded formula) to the 2D or 3D list.
    AddGraph {
        kind: GraphKind,
        id: String,
        rank: i64,
        expr: String,
    },
    /// Replace a graph row's formula, diff-spliced into its CRDT `Text` so the
    /// edit is a bounded, mergeable op rather than a wholesale rewrite.
    SetGraphExpr {
        kind: GraphKind,
        id: String,
        expr: String,
    },
    /// Flip a graph row's visibility. Recomputed from the document's base state
    /// (an absent flag reads as visible) so replicas converge.
    ToggleGraphVisible { kind: GraphKind, id: String },
    /// Delete a graph row by stable id. A missing id is a no-op.
    DeleteGraph { kind: GraphKind, id: String },
}

fn rows_at(doc: &Doc, key: &str) -> Option<ObjId> {
    match doc.get(&ROOT, key) {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
}

fn ensure_rows(doc: &mut Doc, key: &str) -> Result<ObjId, ActionError> {
    if doc.get(&ROOT, "calculator_resources_v2")?.is_none() {
        doc.put(&ROOT, "calculator_resources_v2", true)?;
    }
    rows_at(doc, key).map_or_else(|| Ok(doc.put_object(&ROOT, key, ObjType::Map)?), Ok)
}

fn keyed_row(doc: &Doc, rows: &ObjId, id: &str) -> Option<ObjId> {
    match doc.get(rows, id) {
        Ok(Some((Value::Object(ObjType::Map), row))) => Some(row),
        _ => None,
    }
}
fn resolved_row(doc: &Doc, key: &str, id: &str) -> Option<ObjId> {
    let rows = rows_at(doc, key)?;
    let id = id
        .strip_prefix('@')
        .and_then(|n| n.parse::<usize>().ok())
        .and_then(|index| crate::doc::row_id_at(doc, key, index))
        .unwrap_or_else(|| id.to_string());
    keyed_row(doc, &rows, &id)
}

fn ensure_keyed_row(doc: &mut Doc, rows: &ObjId, id: &str) -> Result<ObjId, ActionError> {
    keyed_row(doc, rows, id).map_or_else(|| Ok(doc.put_object(rows, id, ObjType::Map)?), Ok)
}

/// Read a row's boolean `visible` flag, defaulting to `true` (the model's
/// default) when it is absent — so a flip is deterministic against the base.
fn row_visible(doc: &Doc, row: &ObjId) -> bool {
    match doc.get(row, "visible") {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Boolean(b) => *b,
            _ => true,
        },
        _ => true,
    }
}

impl Action for CalculatorAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            CalculatorAction::CommitHistory {
                id,
                rank,
                expr,
                result,
            } => {
                let rows = ensure_rows(doc, "history")?;
                let row = ensure_keyed_row(doc, &rows, id)?;
                // A tombstone is permanent. Concurrent old writes can update
                // text, but can never resurrect a row a collaborator deleted.
                if !row_deleted(doc, &row) {
                    put_text_diffed(doc, &row, "expr", expr);
                    put_text_diffed(doc, &row, "result", result);
                    put_if_changed(doc, &row, "rank", *rank);
                    put_if_changed(doc, &row, "deleted", false);
                    // Preserve bounded history without physically removing a
                    // resource: old entries remain tombstoned, so a delayed
                    // peer edit can never rebind or resurrect them.
                    for old_id in crate::doc::visible_ids(doc, "history")
                        .into_iter()
                        .skip(HISTORY_CAP)
                    {
                        if let Some(old) = resolved_row(doc, "history", &old_id) {
                            doc.put(&old, "deleted", true)?;
                        }
                    }
                }
            }
            CalculatorAction::DeleteHistoryEntry { id } => {
                if let Some(row) = resolved_row(doc, "history", id) {
                    doc.put(&row, "deleted", true)?;
                }
            }
            CalculatorAction::ClearHistory { ids } => {
                let targets = if ids.is_empty() {
                    crate::doc::visible_ids(doc, "history")
                } else {
                    ids.clone()
                };
                for id in &targets {
                    if let Some(row) = resolved_row(doc, "history", id) {
                        doc.put(&row, "deleted", true)?;
                    }
                }
            }
            CalculatorAction::AddGraph {
                kind,
                id,
                rank,
                expr,
            } => {
                let rows = ensure_rows(doc, kind.doc_key())?;
                let row = ensure_keyed_row(doc, &rows, id)?;
                // Match the model's row shape: a diff-spliced `Text` formula and
                // a visible flag, so `store`'s reconcile is a no-op afterward.
                if !row_deleted(doc, &row) {
                    put_text_diffed(doc, &row, "expr", expr);
                    put_if_changed(doc, &row, "visible", true);
                    put_if_changed(doc, &row, "rank", *rank);
                    put_if_changed(doc, &row, "deleted", false);
                }
            }
            CalculatorAction::SetGraphExpr { kind, id, expr } => {
                if let Some(row) = resolved_row(doc, kind.doc_key(), id)
                    && !row_deleted(doc, &row)
                {
                    put_text_diffed(doc, &row, "expr", expr);
                }
            }
            CalculatorAction::ToggleGraphVisible { kind, id } => {
                if let Some(row) = resolved_row(doc, kind.doc_key(), id)
                    && !row_deleted(doc, &row)
                {
                    let now = !row_visible(doc, &row);
                    doc.put(&row, "visible", now)?;
                }
            }
            CalculatorAction::DeleteGraph { kind, id } => {
                if let Some(row) = resolved_row(doc, kind.doc_key(), id) {
                    doc.put(&row, "deleted", true)?;
                }
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        // Personal calculator state — applied locally and replicated to any
        // invited collaborator; no server ordering needed.
        ExecutionLocus::ClientLocal
    }
}

/// Register the calculator's action decoder for palette execution + remote
/// replay. Called once at app start.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<CalculatorAction>(CALCULATOR_ACTION_TYPE);
}

/// Encode any [`CalculatorAction`] into the wire form the palette and the
/// on-screen affordances both dispatch — the single point that turns a
/// calculator action into a replayable envelope.
///
/// # Panics
///
/// Panics if `action` fails to serialize. `CalculatorAction`'s fields are
/// plain `String`/`usize`/`GraphKind` data with a derived `Serialize` impl,
/// so this is not reachable in practice.
#[allow(
    clippy::expect_used,
    reason = "CalculatorAction always serializes (derived impl over plain \
              data); returning Result here would ripple through every call \
              site in app.rs and this module for an error path that can't \
              occur"
)]
pub fn encode(action: &CalculatorAction) -> EncodedAction {
    EncodedAction::encode(CALCULATOR_ACTION_TYPE, action)
        .expect("CalculatorAction is always serializable")
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

/// Commit `expr = result` to the shared history.
pub fn commit_history_action(expr: &str, result: &str) -> CalculatorAction {
    CalculatorAction::CommitHistory {
        id: new_id("history"),
        rank: next_rank(),
        expr: expr.to_string(),
        result: result.to_string(),
    }
}

/// Delete the history entry at `index` (newest = 0).
pub trait RowTarget {
    fn target(self) -> String;
}
impl RowTarget for &str {
    fn target(self) -> String {
        self.to_string()
    }
}
impl RowTarget for &String {
    fn target(self) -> String {
        self.clone()
    }
}
impl RowTarget for usize {
    fn target(self) -> String {
        format!("@{self}")
    }
}
pub fn delete_history_entry_action(id: impl RowTarget) -> CalculatorAction {
    CalculatorAction::DeleteHistoryEntry { id: id.target() }
}

/// Empty the history list.
pub fn clear_history_ids_action(ids: Vec<String>) -> CalculatorAction {
    CalculatorAction::ClearHistory { ids }
}
/// Compatibility helper for callers that have no snapshot. UI and resources
/// always use `clear_history_ids_action`, which carries the exact stable set.
pub fn clear_history_action() -> CalculatorAction {
    CalculatorAction::ClearHistory { ids: Vec::new() }
}

/// Append a graph row carrying `expr` (use `""` for a blank starter row).
pub fn add_graph_action(kind: GraphKind, expr: &str) -> CalculatorAction {
    CalculatorAction::AddGraph {
        kind,
        id: new_id(kind.slug()),
        rank: next_rank(),
        expr: expr.to_string(),
    }
}

/// Replace the formula of the `index`-th graph row.
pub fn set_graph_expr_action(kind: GraphKind, id: impl RowTarget, expr: &str) -> CalculatorAction {
    CalculatorAction::SetGraphExpr {
        kind,
        id: id.target(),
        expr: expr.to_string(),
    }
}

/// Flip the visibility of the `index`-th graph row.
pub fn toggle_graph_visible_action(kind: GraphKind, id: impl RowTarget) -> CalculatorAction {
    CalculatorAction::ToggleGraphVisible {
        kind,
        id: id.target(),
    }
}

/// Delete the `index`-th graph row.
pub fn delete_graph_action(kind: GraphKind, id: impl RowTarget) -> CalculatorAction {
    CalculatorAction::DeleteGraph {
        kind,
        id: id.target(),
    }
}

fn row_deleted(doc: &Doc, row: &ObjId) -> bool {
    matches!(doc.get(row, "deleted"), Ok(Some((Value::Scalar(v), _))) if matches!(v.as_ref(), ScalarValue::Boolean(true)))
}
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
fn next_rank() -> i64 {
    i64::try_from(NEXT_ID.fetch_add(1, Ordering::Relaxed)).unwrap_or(i64::MAX)
}
fn new_id(kind: &str) -> String {
    // Identity is chosen before action encoding, never during replay.  The
    // timestamp makes independently running collaborator windows disjoint;
    // the process counter also distinguishes two additions in the same tick.
    let tick = ui::time::wall()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{kind}-{tick}-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// The presentation context the calculator hands [`provide_resources`]: the
/// history + saved graphs it currently has (the in-memory mirror of the shared
/// document). Nothing here is fabricated — it is exactly what the CRDT holds.
#[derive(Debug, Clone)]
pub struct CalcResourceCtx {
    pub mode: Mode,
    pub history: Vec<HistoryEntry>,
    pub funcs: Vec<PlotFn>,
    pub surfaces: Vec<PlotFn>,
}

/// Publish the calculator resource tree through [`tilo_resource::publish`].
/// `ctx` is retained for call-site compatibility; the document is the source
/// of truth for durable rows.
pub fn provide_resources(_ctx: &CalcResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
    let state = <crate::resource::Calculator as TiloResource>::read(doc).unwrap_or_default();
    vec![ui::tilo_resource::publish::<crate::resource::Calculator>(
        <crate::resource::Calculator as TiloResource>::KIND,
        "Calculator",
        &state,
    )]
}

// Inherent resource publication (was ResourceProvider)
impl crate::app::CalculatorApp {
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&self.calc_resource_ctx(), doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::{ActionInput, LocalTransport};

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// Apply an action through the executor exactly as the app's dispatch does.
    fn dispatch(doc: &mut AutoCommit, action: &CalculatorAction) {
        let encoded = encode(action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    fn prompted_encode(spec: &tilo_actions::ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) | ActionInput::Deferred { .. } => {
                panic!("expected a prompted action: {}", spec.id)
            }
        }
    }

    // -- test-local readers over the raw document -------------------------------

    fn read_history(doc: &AutoCommit) -> Vec<(String, String)> {
        crate::doc::load(doc).map_or_else(Vec::new, |m| {
            m.history.into_iter().map(|e| (e.expr, e.result)).collect()
        })
    }

    fn read_graph(doc: &AutoCommit, kind: GraphKind) -> Vec<(String, bool)> {
        crate::doc::load(doc).map_or_else(Vec::new, |m| {
            match kind {
                GraphKind::Curve2d => m.funcs,
                GraphKind::Surface3d => m.surfaces,
            }
            .into_iter()
            .map(|f| (f.expr, f.visible))
            .collect()
        })
    }

    /// History actions: commit pushes to the front and caps at `HISTORY_CAP`;
    /// delete removes one entry, clear empties, out-of-range delete is a no-op.
    /// Each phase uses a fresh document.
    #[test]
    fn history_actions_commit_capped_delete_and_clear() {
        // phase commit_history_pushes_front_and_caps
        {
            let mut doc = AutoCommit::new();
            dispatch(&mut doc, &commit_history_action("2+3", "5"));
            dispatch(&mut doc, &commit_history_action("sqrt(2)", "1.41"));
            let h = read_history(&doc);
            assert_eq!(h.len(), 2, "commit: two entries");
            assert_eq!(
                h[0],
                ("sqrt(2)".into(), "1.41".into()),
                "newest is at the front"
            );
            assert_eq!(h[1], ("2+3".into(), "5".into()), "commit: older second");

            // Overflow drops from the tail, keeping the newest HISTORY_CAP.
            for i in 0..(HISTORY_CAP + 10) {
                dispatch(
                    &mut doc,
                    &commit_history_action(&format!("{i}"), &format!("{i}")),
                );
            }
            let h = read_history(&doc);
            assert_eq!(h.len(), HISTORY_CAP, "history is capped");
            assert_eq!(
                h[0].0,
                format!("{}", HISTORY_CAP + 10 - 1),
                "front is the newest commit"
            );
        }

        // phase clear_and_delete_history
        {
            let mut doc = AutoCommit::new();
            for i in 0..4 {
                dispatch(
                    &mut doc,
                    &commit_history_action(&format!("e{i}"), &format!("{i}")),
                );
            }
            // History front→back is e3,e2,e1,e0. Delete the middle (index 1 = e2).
            dispatch(&mut doc, &delete_history_entry_action(1));
            let h = read_history(&doc);
            assert_eq!(
                h.iter().map(|(e, _)| e.clone()).collect::<Vec<_>>(),
                vec!["e3", "e1", "e0"],
                "delete: middle entry removed"
            );
            // Clear empties it.
            dispatch(&mut doc, &clear_history_action());
            assert!(read_history(&doc).is_empty(), "clear: history empty");
            // Out-of-range delete is a no-op (dispatched last: the app never issues a
            // no-op action — affordances gate on a real change — so the executor's
            // empty-change handling is never exercised mid-sequence).
            dispatch(&mut doc, &delete_history_entry_action(99));
            assert!(
                read_history(&doc).is_empty(),
                "out-of-range delete changed nothing"
            );
        }
    }

    /// Graph-row actions add, set (diff-spliced into CRDT Text), toggle and
    /// delete rows for both kinds; formulas are stored as CRDT Text; a tombstoned
    /// row is not resurrected by a late edit. Fresh document per phase/kind.
    #[test]
    fn graph_row_actions_edit_crdt_text_and_respect_tombstones() {
        // phase add_toggle_set_delete_graph_rows
        for kind in [GraphKind::Curve2d, GraphKind::Surface3d] {
            let mut doc = AutoCommit::new();
            dispatch(&mut doc, &add_graph_action(kind, ""));
            dispatch(&mut doc, &add_graph_action(kind, "sin(x)"));
            assert_eq!(
                read_graph(&doc, kind),
                vec![(String::new(), true), ("sin(x)".into(), true)],
                "{kind:?}: add"
            );

            // Set replaces a formula (diff-spliced into the row's Text).
            dispatch(&mut doc, &set_graph_expr_action(kind, 0, "x^2"));
            assert_eq!(
                read_graph(&doc, kind)[0],
                ("x^2".into(), true),
                "{kind:?}: set"
            );

            // Toggle flips visibility.
            dispatch(&mut doc, &toggle_graph_visible_action(kind, 1));
            assert_eq!(
                read_graph(&doc, kind)[1],
                ("sin(x)".into(), false),
                "{kind:?}: toggle"
            );

            // Delete removes a row.
            dispatch(&mut doc, &delete_graph_action(kind, 0));
            assert_eq!(
                read_graph(&doc, kind),
                vec![("sin(x)".into(), false)],
                "{kind:?}: delete"
            );
        }

        // phase graph_expr_is_stored_as_crdt_text
        {
            // The formula must be a character-level CRDT Text (multi-cursor), not a
            // plain Str — `read_graph`'s `row_text` asserts exactly this shape.
            let mut doc = AutoCommit::new();
            dispatch(&mut doc, &add_graph_action(GraphKind::Curve2d, "cos(x)"));
            assert_eq!(
                read_graph(&doc, GraphKind::Curve2d),
                vec![("cos(x)".into(), true)],
                "crdt text: cos(x) row"
            );
        }

        // phase tombstoned_graph_row_cannot_be_resurrected_by_a_late_edit
        {
            let mut doc = AutoCommit::new();
            dispatch(&mut doc, &add_graph_action(GraphKind::Curve2d, "x"));
            let id = crate::doc::row_id_at(&doc, "funcs", 0).expect("stable graph id");
            dispatch(
                &mut doc,
                &delete_graph_action(GraphKind::Curve2d, id.as_str()),
            );
            dispatch(
                &mut doc,
                &set_graph_expr_action(GraphKind::Curve2d, id.as_str(), "x^2"),
            );
            assert!(
                read_graph(&doc, GraphKind::Curve2d).is_empty(),
                "delete wins over a delayed edit"
            );
        }
    }

    fn ctx() -> CalcResourceCtx {
        CalcResourceCtx {
            mode: Mode::Graph2d,
            history: vec![
                HistoryEntry {
                    expr: "2^10".into(),
                    result: "1024".into(),
                },
                HistoryEntry {
                    expr: "sqrt(2)".into(),
                    result: "1.41".into(),
                },
            ],
            funcs: vec![
                PlotFn {
                    expr: "sin(x)".into(),
                    visible: true,
                },
                PlotFn {
                    expr: "x^2".into(),
                    visible: false,
                },
            ],
            surfaces: vec![PlotFn {
                expr: "x*y".into(),
                visible: true,
            }],
        }
    }

    fn doc_for_ctx() -> AutoCommit {
        let mut doc = AutoCommit::new();
        // History is inserted at the front, so author it in reverse display order.
        dispatch(&mut doc, &commit_history_action("sqrt(2)", "1.41"));
        dispatch(&mut doc, &commit_history_action("2^10", "1024"));
        dispatch(&mut doc, &add_graph_action(GraphKind::Curve2d, "sin(x)"));
        dispatch(&mut doc, &add_graph_action(GraphKind::Curve2d, "x^2"));
        dispatch(
            &mut doc,
            &toggle_graph_visible_action(GraphKind::Curve2d, 1),
        );
        dispatch(&mut doc, &add_graph_action(GraphKind::Surface3d, "x*y"));
        doc
    }

    /// The stable row id a graph resource's address ends in.
    fn row_id(res: &Resource) -> &str {
        res.id.as_str().rsplit(':').next().expect("stable row id")
    }

    fn nth_graph_id(doc: &AutoCommit, kind: GraphKind, n: usize) -> String {
        crate::doc::row_id_at(doc, kind.doc_key(), n).expect("seeded graph row")
    }

    ui::action_contract_tests! {
        seed: doc_for_ctx(),
        registry: registry(),
        resources: |doc| provide_resources(&ctx(), doc),
        palette: ["Add function", "Clear history", "Delete formula"],
        replay: [
            set_formula => |doc| encode(&set_graph_expr_action(
                GraphKind::Curve2d,
                nth_graph_id(doc, GraphKind::Curve2d, 1).as_str(),
                "x^3+1",
            )),
            toggle_surface => |doc| encode(&toggle_graph_visible_action(
                GraphKind::Surface3d,
                nth_graph_id(doc, GraphKind::Surface3d, 0).as_str(),
            )),
        ],
        undo: [
            commit_history => |_| encode(&commit_history_action("6*7", "42")),
        ],
        encodes: [
            clear_history => ("calculator.document", "clear-history", |doc, _| {
                encode(&clear_history_ids_action(crate::doc::visible_ids(doc, "history")))
            }),
            toggle_visibility => ("calculator.graph", "toggle-visibility", |_, res| {
                encode(&toggle_graph_visible_action(GraphKind::Curve2d, row_id(res)))
            }),
            delete_formula => ("calculator.graph", "delete", |_, res| {
                encode(&delete_graph_action(GraphKind::Curve2d, row_id(res)))
            }),
            edit_formula => ("calculator.graph", "edit", arg: "cos(x)", |_, res| {
                encode(&set_graph_expr_action(GraphKind::Curve2d, row_id(res), "cos(x)"))
            }),
        ],
    }

    /// The published resource tree (what Ctrl+K, search and a11y browse): the
    /// workspace/history/graph hierarchy with its actions, the add-function
    /// prompt building the canonical `AddGraph`, addresses that follow CRDT rows
    /// across a positional delete, and a11y affordances that are the
    /// byte-identical actions the on-screen controls dispatch. Each phase seeds a
    /// fresh document.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn published_resources_expose_stable_canonical_affordances() {
        // phase ctrl_k_publishes_workspace_history_and_graph_tree
        {
            let doc = doc_for_ctx();
            let resources = provide_resources(&ctx(), &doc);
            assert_eq!(resources.len(), 1, "one workspace root");
            let ws = &resources[0];
            assert_eq!(ws.kind, "calculator.document", "ctrl_k: document kind");
            let ws_actions: Vec<&str> = ws.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(ws_actions.contains(&"add-function"), "{ws_actions:?}");
            assert!(ws_actions.contains(&"add-surface"), "{ws_actions:?}");
            assert!(ws_actions.contains(&"clear-history"), "{ws_actions:?}");

            // Two history entries + two curves + one surface = five children.
            let hist: Vec<&Resource> = ws
                .children
                .iter()
                .filter(|r| r.kind == "calculator.history_entry")
                .collect();
            assert_eq!(hist.len(), 2, "ctrl_k: two history entries");
            assert!(
                hist[0].actions.iter().any(|a| a.id == "delete"),
                "ctrl_k: history entry affords delete"
            );
            let graphs: Vec<&Resource> = ws
                .children
                .iter()
                .filter(|r| r.kind == "calculator.graph")
                .collect();
            assert_eq!(graphs.len(), 3, "2 curves + 1 surface");
            let g0 = &graphs[0];
            let g0_actions: Vec<&str> = g0.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(g0_actions.contains(&"edit"), "{g0_actions:?}");
            assert!(g0_actions.contains(&"toggle-visibility"), "{g0_actions:?}");
            assert!(g0_actions.contains(&"delete"), "{g0_actions:?}");
        }

        // phase add_function_prompt_builds_the_canonical_add_graph
        {
            // The workspace add-function prompt builds the same `AddGraph` the keypad
            // "+" affordance does; its fresh stable id rules out a byte comparison.
            let doc = doc_for_ctx();
            let resources = provide_resources(&ctx(), &doc);
            let ws = &resources[0];
            let add = ws.actions.iter().find(|a| a.id == "add-function").unwrap();
            let encoded = prompted_encode(add, "x^2");
            let decoded: CalculatorAction =
                serde_json::from_slice(&encoded.payload).expect("decode canonical action");
            assert!(
                matches!(decoded, CalculatorAction::AddGraph { kind: GraphKind::Curve2d, expr, .. } if expr == "x^2"),
                "add-function uses the canonical AddGraph action with a fresh stable id"
            );
        }

        // phase resource_addresses_follow_crdt_rows_after_a_positional_delete
        {
            let mut doc = doc_for_ctx();
            let mut context = ctx();
            let before = provide_resources(&context, &doc);
            let history_before: Vec<String> = before[0]
                .children
                .iter()
                .filter(|resource| resource.kind == "calculator.history_entry")
                .map(|resource| resource.id.0.clone())
                .collect();

            dispatch(&mut doc, &delete_history_entry_action(0));
            context.history.remove(0);
            let after = provide_resources(&context, &doc);
            let remaining = after[0]
                .children
                .iter()
                .find(|resource| resource.kind == "calculator.history_entry")
                .expect("the second history row remains");
            assert_eq!(
                remaining.id.0, history_before[1],
                "addresses follow the CRDT row after a positional delete"
            );
        }

        // phase a11y_tree_surfaces_history_delete_as_the_affordance_action
        {
            // The retained accessibility tree surfaces the calculator's operable
            // resources as stable nodes, and a node's exposed affordance is the
            // byte-identical `EncodedAction` the on-screen control dispatches. (The
            // calculator custom-draws its keypad, so its *operable* semantics come from
            // the resource + action model, not per-key widgets.)
            let mut resource_doc = doc_for_ctx();
            let resources = provide_resources(&ctx(), &resource_doc);

            // An inert root; the operable nodes come from the published resources.
            // `HashMap::new()`'s type is inferred from `refresh`'s signature.
            let root = ui::widget::Column::new();
            let node_map = std::collections::HashMap::new();
            let mut tree = ui::SemanticTree::new();

            // First refresh populates the tree; a second, unchanged refresh is a
            // no-op with identical node identity (retained, not rebuilt each frame).
            let d1 = tree.refresh(&root, &node_map, None, &resources);
            assert!(!d1.added.is_empty(), "the first refresh populates the tree");
            let ws = tree
                .node(&ui::SemanticId("r:calculator.document".into()))
                .expect("workspace node present");
            assert_eq!(
                ws.name.as_deref(),
                Some("Calculator"),
                "a11y: workspace name"
            );
            assert!(
                ws.action("clear-history").is_some(),
                "workspace affords clear-history"
            );
            let ids1: Vec<ui::SemanticId> = tree.iter().map(|n| n.id.clone()).collect();

            let d2 = tree.refresh(&root, &node_map, None, &resources);
            assert!(d2.is_empty(), "an unchanged frame yields no delta: {d2:?}");
            let ids2: Vec<ui::SemanticId> = tree.iter().map(|n| n.id.clone()).collect();
            assert_eq!(ids1, ids2, "node identity survives across frames");

            // The newest history entry is a node whose 'delete' affordance carries
            // the exact EncodedAction the on-screen delete control dispatches.
            let history_id = resources
                .first()
                .expect("calculator workspace resource")
                .children
                .iter()
                .find(|resource| resource.kind == "calculator.history_entry")
                .map(|resource| ui::SemanticId(format!("r:{}", resource.id.as_str())))
                .expect("published history entry resource");
            let entry = tree.node(&history_id).expect("history entry node");
            let exposed = match &entry.action("delete").expect("delete affordance").kind {
                ui::SemanticActionKind::App(enc) => enc.clone(),
                other => panic!("expected an app action, got {other:?}"),
            };
            assert_eq!(
                exposed,
                encode(&delete_history_entry_action(
                    resources[0]
                        .children
                        .iter()
                        .find(|r| r.kind == "calculator.history_entry")
                        .and_then(|r| r.id.as_str().rsplit(':').next())
                        .expect("stable history id")
                )),
                "the a11y affordance and the on-screen builder are one action"
            );

            // Invoking the exposed action drives the doc exactly as the on-screen
            // affordance's own builder does: seed two identical docs, delete through
            // each path, and compare the results.
            let bytes = resource_doc.save();
            let mut via_a11y = AutoCommit::load(&bytes).expect("clone resource document");
            let mut via_affordance = AutoCommit::load(&bytes).expect("clone resource document");

            let decoded = registry().decode(&exposed).unwrap();
            let mut tx = LocalTransport::new();
            tilo_actions::dispatch(&mut via_a11y, decoded.as_ref(), exposed, &mut tx).unwrap();
            let id = crate::doc::row_id_at(&via_affordance, "history", 0).expect("stable id");
            dispatch(
                &mut via_affordance,
                &delete_history_entry_action(id.as_str()),
            );

            assert_eq!(
                read_history(&via_a11y),
                read_history(&via_affordance),
                "invoking the a11y affordance deletes the same entry the control does"
            );
            assert_eq!(
                read_history(&via_a11y).len(),
                1,
                "one entry remains after the delete"
            );
        }
    }
}
