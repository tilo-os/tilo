//! The calculator's exported **actions**: the one vocabulary every control,
//! the palette, agents and scripts change its document with.
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
//! Typing into a graph formula is [`CalculatorAction::SetGraphExpr`] per
//! keystroke, diff-spliced into the row's Automerge `Text`; the keystrokes of
//! one row coalesce into one undo step (`Resource::coalesces`).
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
use automerge::{ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use tilo_actions::{Action, ActionError, ActionRegistry, Doc, EncodedAction, ExecutionLocus};
use ui::doc::{put_if_changed, put_text_diffed};
use ui::tilo_resource::{self, Effect};

/// Registry `type_id` for every calculator action.
pub const CALCULATOR_ACTION_TYPE: &str = "calculator.action";
/// Which part of the calculator is shown, and recalling a past calculation:
/// what this window shows, not the shared document.
#[derive(Debug, Clone, PartialEq, Eq, Effect)]
#[effect(operation = "calculator.effect")]
pub enum CalculatorEffect {
    /// Show calculator.
    #[effect(view, idempotent)]
    ShowCalculator,
    /// Show 2D graph.
    #[effect(view, idempotent)]
    ShowGraph2d,
    /// Show 3D graph.
    #[effect(view, idempotent)]
    ShowGraph3d,
    /// Recall calculation.
    #[effect(view, row, idempotent)]
    Open {
        #[param("The history entry's id")]
        entry: String,
    },
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
    rows_at(doc, key).map_or_else(|| Ok(doc.put_object(&ROOT, key, ObjType::Map)?), Ok)
}

fn keyed_row(doc: &Doc, rows: &ObjId, id: &str) -> Option<ObjId> {
    match doc.get(rows, id) {
        Ok(Some((Value::Object(ObjType::Map), row))) => Some(row),
        _ => None,
    }
}
fn resolved_row(doc: &Doc, key: &str, id: &str) -> Option<ObjId> {
    keyed_row(doc, &rows_at(doc, key)?, id)
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
// affordances (see `crate::app`) and the published declarations
// (see `crate::resource`).
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

/// A row an action addresses by its stable id.
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
/// Delete one history entry.
pub fn delete_history_entry_action(id: impl RowTarget) -> CalculatorAction {
    CalculatorAction::DeleteHistoryEntry { id: id.target() }
}

/// Empty the history list.
pub fn clear_history_ids_action(ids: Vec<String>) -> CalculatorAction {
    CalculatorAction::ClearHistory { ids }
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

/// Replace the formula of one graph row.
pub fn set_graph_expr_action(kind: GraphKind, id: impl RowTarget, expr: &str) -> CalculatorAction {
    CalculatorAction::SetGraphExpr {
        kind,
        id: id.target(),
        expr: expr.to_string(),
    }
}

/// Flip the visibility of one graph row.
pub fn toggle_graph_visible_action(kind: GraphKind, id: impl RowTarget) -> CalculatorAction {
    CalculatorAction::ToggleGraphVisible {
        kind,
        id: id.target(),
    }
}

/// Delete one graph row.
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
static LAST_RANK: AtomicI64 = AtomicI64::new(0);
/// Ranks order rows by when they were made: wall-clock microseconds, kept
/// strictly increasing within the process, so a row added after a restart
/// still sorts after every row added before it.
fn next_rank() -> i64 {
    let now = i64::try_from(ui::time::unix_now_ns() / 1_000).unwrap_or(i64::MAX);
    let previous = LAST_RANK
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |last| {
            Some(now.max(last.saturating_add(1)))
        })
        .unwrap_or(now);
    now.max(previous.saturating_add(1))
}
fn new_id(kind: &str) -> String {
    // Identity is chosen before action encoding, never during replay.  The
    // timestamp makes independently running collaborator windows disjoint;
    // the process counter also distinguishes two additions in the same tick.
    let tick = ui::time::unix_now_ns();
    format!("{kind}-{tick}-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::Calculator;
    use tilo_actions::{ActionInput, LocalTransport, Resource};
    use ui::automerge::AutoCommit;
    use ui::tilo_resource::Resource as _;

    fn registry() -> ActionRegistry {
        ActionRegistry::with(register)
    }

    /// Apply an action through the executor exactly as the app's dispatch does.
    fn dispatch(doc: &mut AutoCommit, action: &CalculatorAction) {
        let encoded = encode(action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    // -- test-local readers over the raw document -------------------------------

    fn read_history(doc: &AutoCommit) -> Vec<(String, String)> {
        crate::doc::load_history(doc)
            .into_iter()
            .map(|e| (e.expr, e.result))
            .collect()
    }

    fn read_graph(doc: &AutoCommit, kind: GraphKind) -> Vec<(String, bool)> {
        crate::doc::load_graph(doc, kind.doc_key())
            .into_iter()
            .map(|f| (f.expr, f.visible))
            .collect()
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
            // History front→back is e3,e2,e1,e0. Delete the middle one (e2).
            let e2 = crate::doc::visible_ids(&doc, "history")[1].clone();
            dispatch(&mut doc, &delete_history_entry_action(&e2));
            let h = read_history(&doc);
            assert_eq!(
                h.iter().map(|(e, _)| e.clone()).collect::<Vec<_>>(),
                vec!["e3", "e1", "e0"],
                "delete: middle entry removed"
            );
            // Clear empties it.
            let ids = crate::doc::visible_ids(&doc, "history");
            dispatch(&mut doc, &clear_history_ids_action(ids));
            assert!(read_history(&doc).is_empty(), "clear: history empty");
            // Deleting an unknown entry is a no-op (dispatched last: the app never
            // issues a no-op action — affordances gate on a real change — so the
            // executor's empty-change handling is never exercised mid-sequence).
            dispatch(&mut doc, &delete_history_entry_action("missing"));
            assert!(
                read_history(&doc).is_empty(),
                "deleting an unknown entry changed nothing"
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

            let ids = crate::doc::visible_ids(&doc, kind.doc_key());
            // Set replaces a formula (diff-spliced into the row's Text).
            dispatch(&mut doc, &set_graph_expr_action(kind, &ids[0], "x^2"));
            assert_eq!(
                read_graph(&doc, kind)[0],
                ("x^2".into(), true),
                "{kind:?}: set"
            );

            // Toggle flips visibility.
            dispatch(&mut doc, &toggle_graph_visible_action(kind, &ids[1]));
            assert_eq!(
                read_graph(&doc, kind)[1],
                ("sin(x)".into(), false),
                "{kind:?}: toggle"
            );

            // Delete removes a row.
            dispatch(&mut doc, &delete_graph_action(kind, &ids[0]));
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
            let id = crate::doc::visible_ids(&doc, "funcs")[0].clone();
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

    fn seed() -> AutoCommit {
        let mut doc = AutoCommit::new();
        crate::doc::initialize(&mut doc).unwrap();
        // History is inserted at the front, so author it in reverse display order.
        dispatch(&mut doc, &commit_history_action("sqrt(2)", "1.41"));
        dispatch(&mut doc, &commit_history_action("2^10", "1024"));
        dispatch(&mut doc, &add_graph_action(GraphKind::Curve2d, "sin(x)"));
        dispatch(&mut doc, &add_graph_action(GraphKind::Curve2d, "x^2"));
        let second = nth_graph_id(&doc, GraphKind::Curve2d, 1);
        dispatch(
            &mut doc,
            &toggle_graph_visible_action(GraphKind::Curve2d, &second),
        );
        dispatch(&mut doc, &add_graph_action(GraphKind::Surface3d, "x*y"));
        doc
    }

    fn published(doc: &AutoCommit) -> Vec<Resource> {
        let state = Calculator::read(doc).unwrap();
        vec![ui::tilo_resource::publish::<Calculator>(
            Calculator::KIND,
            "Calculator",
            &state,
        )]
    }

    /// The stable row id an item resource's address ends in.
    fn row_id(res: &Resource) -> &str {
        res.id.as_str().rsplit('/').next().expect("stable row id")
    }

    fn nth_graph_id(doc: &AutoCommit, kind: GraphKind, n: usize) -> String {
        crate::doc::visible_ids(doc, kind.doc_key())[n].clone()
    }

    ui::action_contract_tests! {
        seed: seed(),
        registry: registry(),
        resources: |doc| published(doc),
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
            toggle_visibility => ("calculator.document.funcs", "toggle-function", |_, res| {
                encode(&toggle_graph_visible_action(GraphKind::Curve2d, row_id(res)))
            }),
            delete_formula => ("calculator.document.funcs", "delete-function", |_, res| {
                encode(&delete_graph_action(GraphKind::Curve2d, row_id(res)))
            }),
            edit_formula => ("calculator.document.funcs", "edit-function", arg: "cos(x)", |_, res| {
                encode(&set_graph_expr_action(GraphKind::Curve2d, row_id(res), "cos(x)"))
            }),
            delete_entry => ("calculator.document.history", "delete-history-entry", |_, res| {
                encode(&delete_history_entry_action(row_id(res)))
            }),
        ],
    }

    /// The published tree (what Ctrl+K, search and agents browse): the
    /// history and graph rows as items with their row actions, the
    /// add-function prompt building the canonical `AddGraph`, and addresses
    /// that follow CRDT rows across a delete.
    #[test]
    fn published_items_expose_stable_canonical_affordances() {
        let mut doc = seed();
        let resources = published(&doc);
        let root = &resources[0];
        assert_eq!(root.kind, "calculator.document");
        let ids: Vec<&str> = root.actions.iter().map(|a| a.id.as_str()).collect();
        for id in ["add-function", "add-surface", "clear-history"] {
            assert!(ids.contains(&id), "{id}: {ids:?}");
        }
        let of_kind = |resources: &[Resource], kind: &str| -> Vec<Resource> {
            resources[0]
                .children
                .iter()
                .filter(|r| r.kind == kind)
                .cloned()
                .collect()
        };
        let history = of_kind(&resources, "calculator.document.history");
        assert_eq!(history.len(), 2, "two history entries");
        assert_eq!(
            of_kind(&resources, "calculator.document.funcs").len(),
            2,
            "two curves"
        );
        let surfaces = of_kind(&resources, "calculator.document.surfaces");
        assert_eq!(surfaces.len(), 1, "one surface");
        let actions: Vec<&str> = surfaces[0].actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            actions,
            ["edit-surface", "toggle-surface", "delete-surface"],
            "a surface row carries its own actions"
        );

        // The add-function prompt builds the same `AddGraph` the panel's "+"
        // does; its fresh stable id rules out a byte comparison.
        let add = root
            .actions
            .iter()
            .find(|a| a.id == "add-function")
            .unwrap();
        let ActionInput::Prompted { build, .. } = &add.input else {
            panic!("add-function asks for its formula");
        };
        let decoded: CalculatorAction =
            serde_json::from_slice(&build("x^2").payload).expect("decode canonical action");
        assert!(
            matches!(decoded, CalculatorAction::AddGraph { kind: GraphKind::Curve2d, expr, .. } if expr == "x^2"),
            "add-function uses the canonical AddGraph action with a fresh stable id"
        );

        // Deleting the newest entry leaves the other at the address it had.
        dispatch(&mut doc, &delete_history_entry_action(row_id(&history[0])));
        let after = of_kind(&published(&doc), "calculator.document.history");
        assert_eq!(after.len(), 1);
        assert_eq!(
            after[0].id, history[1].id,
            "addresses follow the CRDT row after a delete"
        );
    }
}
