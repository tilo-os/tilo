//! The calculator's exported **actions** and the **resource tree** it publishes
//! to Ctrl+K.
//!
//! Most of a calculator is ephemeral: the expression being typed, the running
//! display, which mode (Calc / 2D / 3D) is showing, the pan/zoom of a graph, the
//! RAD/DEG toggle, the memory scratch register. None of that is a document
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

use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus, Resource,
    ResourceProvider,
};
use ui::doc::{put_if_changed, put_text_diffed};

use crate::model::{HistoryEntry, Mode, PlotFn};

/// Registry `type_id` for every calculator action.
pub const CALCULATOR_ACTION_TYPE: &str = "calculator.action";

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

    /// The left-hand side shown in a palette label (`y = …` / `z = …`).
    fn lhs(self) -> &'static str {
        match self {
            GraphKind::Curve2d => "y",
            GraphKind::Surface3d => "z",
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
    CommitHistory { expr: String, result: String },
    /// Remove one history entry by position (newest = 0). Out of range is a
    /// no-op.
    DeleteHistoryEntry { index: usize },
    /// Empty the whole history list.
    ClearHistory,
    /// Append a graph row carrying `expr` (empty for a blank starter row, or a
    /// seeded formula) to the 2D or 3D list.
    AddGraph { kind: GraphKind, expr: String },
    /// Replace a graph row's formula, diff-spliced into its CRDT `Text` so the
    /// edit is a bounded, mergeable op rather than a wholesale rewrite.
    SetGraphExpr {
        kind: GraphKind,
        index: usize,
        expr: String,
    },
    /// Flip a graph row's visibility. Recomputed from the document's base state
    /// (an absent flag reads as visible) so replicas converge.
    ToggleGraphVisible { kind: GraphKind, index: usize },
    /// Delete a graph row by position. Out of range is a no-op.
    DeleteGraph { kind: GraphKind, index: usize },
}

/// The list object at `root[key]`, if it is present as a `List`.
fn list_at(doc: &Doc, key: &str) -> Option<ObjId> {
    match doc.get(&ROOT, key) {
        Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
        _ => None,
    }
}

/// The list object at `root[key]`, creating an empty one if absent.
fn ensure_list(doc: &mut Doc, key: &str) -> Result<ObjId, ActionError> {
    match list_at(doc, key) {
        Some(id) => Ok(id),
        None => Ok(doc.put_object(&ROOT, key, ObjType::List)?),
    }
}

/// The map at `list[index]`, if that element is a `Map`.
fn row_at(doc: &Doc, list: &ObjId, index: usize) -> Option<ObjId> {
    match doc.get(list, index) {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
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
            CalculatorAction::CommitHistory { expr, result } => {
                let list = ensure_list(doc, "history")?;
                let row = doc.insert_object(&list, 0, ObjType::Map)?;
                doc.put(&row, "expr", expr.as_str())?;
                doc.put(&row, "result", result.as_str())?;
                // Cap from the tail so the newest [`HISTORY_CAP`] survive.
                while doc.length(&list) > HISTORY_CAP {
                    let last = doc.length(&list) - 1;
                    doc.delete(&list, last)?;
                }
            }
            CalculatorAction::DeleteHistoryEntry { index } => {
                if let Some(list) = list_at(doc, "history")
                    && *index < doc.length(&list)
                {
                    doc.delete(&list, *index)?;
                }
            }
            CalculatorAction::ClearHistory => {
                if let Some(list) = list_at(doc, "history") {
                    for i in (0..doc.length(&list)).rev() {
                        doc.delete(&list, i)?;
                    }
                }
            }
            CalculatorAction::AddGraph { kind, expr } => {
                let list = ensure_list(doc, kind.doc_key())?;
                let index = doc.length(&list);
                let row = doc.insert_object(&list, index, ObjType::Map)?;
                // Match the model's row shape: a diff-spliced `Text` formula and
                // a visible flag, so `store`'s reconcile is a no-op afterward.
                put_text_diffed(doc, &row, "expr", expr);
                put_if_changed(doc, &row, "visible", true);
            }
            CalculatorAction::SetGraphExpr { kind, index, expr } => {
                if let Some(list) = list_at(doc, kind.doc_key())
                    && let Some(row) = row_at(doc, &list, *index)
                {
                    put_text_diffed(doc, &row, "expr", expr);
                }
            }
            CalculatorAction::ToggleGraphVisible { kind, index } => {
                if let Some(list) = list_at(doc, kind.doc_key())
                    && let Some(row) = row_at(doc, &list, *index)
                {
                    let now = !row_visible(doc, &row);
                    doc.put(&row, "visible", now)?;
                }
            }
            CalculatorAction::DeleteGraph { kind, index } => {
                if let Some(list) = list_at(doc, kind.doc_key())
                    && *index < doc.length(&list)
                {
                    doc.delete(&list, *index)?;
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
        expr: expr.to_string(),
        result: result.to_string(),
    }
}

/// Delete the history entry at `index` (newest = 0).
pub fn delete_history_entry_action(index: usize) -> CalculatorAction {
    CalculatorAction::DeleteHistoryEntry { index }
}

/// Empty the history list.
pub fn clear_history_action() -> CalculatorAction {
    CalculatorAction::ClearHistory
}

/// Append a graph row carrying `expr` (use `""` for a blank starter row).
pub fn add_graph_action(kind: GraphKind, expr: &str) -> CalculatorAction {
    CalculatorAction::AddGraph {
        kind,
        expr: expr.to_string(),
    }
}

/// Replace the formula of the `index`-th graph row.
pub fn set_graph_expr_action(kind: GraphKind, index: usize, expr: &str) -> CalculatorAction {
    CalculatorAction::SetGraphExpr {
        kind,
        index,
        expr: expr.to_string(),
    }
}

/// Flip the visibility of the `index`-th graph row.
pub fn toggle_graph_visible_action(kind: GraphKind, index: usize) -> CalculatorAction {
    CalculatorAction::ToggleGraphVisible { kind, index }
}

/// Delete the `index`-th graph row.
pub fn delete_graph_action(kind: GraphKind, index: usize) -> CalculatorAction {
    CalculatorAction::DeleteGraph { kind, index }
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

/// A one-line, length-capped palette label.
fn clip(s: &str, n: usize) -> String {
    let t: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > n {
        t.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    } else {
        t
    }
}

fn history_label(e: &HistoryEntry) -> String {
    let body = if e.expr.trim().is_empty() {
        e.result.clone()
    } else {
        format!("{} = {}", e.expr.trim(), e.result.trim())
    };
    clip(&body, 48)
}

fn graph_label(kind: GraphKind, f: &PlotFn) -> String {
    let rhs = if f.expr.trim().is_empty() {
        "(empty)"
    } else {
        f.expr.trim()
    };
    clip(&format!("{} = {}", kind.lhs(), rhs), 48)
}

fn immediate_spec(id: &str, label: &str, action: &CalculatorAction) -> ActionSpec {
    ActionSpec::immediate(id, label, CALCULATOR_ACTION_TYPE, action)
        .expect("encode calculator action")
}

/// Build one graph list's resources (one `calculator.graph` per row).
fn graph_resources(kind: GraphKind, rows: &[PlotFn]) -> Vec<Resource> {
    rows.iter()
        .enumerate()
        .map(|(i, f)| {
            let mut res = Resource::new(
                format!("calculator.graph:{}:{}", kind.slug(), i),
                "calculator.graph",
                graph_label(kind, f),
            );
            // Replace the formula — a prompted edit, diff-spliced into the row's
            // CRDT Text (the discrete, replayable twin of live typing).
            res = res.with_action(ActionSpec::prompted(
                "edit",
                "Edit formula",
                "Formula",
                "e.g. sin(x)…",
                ExecutionLocus::ClientLocal,
                move |text: &str| encode(&set_graph_expr_action(kind, i, text.trim())),
            ));
            let vis_label = if f.visible { "Hide" } else { "Show" };
            res = res.with_action(immediate_spec(
                "toggle-visibility",
                vis_label,
                &toggle_graph_visible_action(kind, i),
            ));
            res = res.with_action(immediate_spec(
                "delete",
                "Delete formula",
                &delete_graph_action(kind, i),
            ));
            res
        })
        .collect()
}

/// Build the calculator's resource tree: one `calculator.workspace` root that
/// owns a `calculator.history_entry` per committed computation and a
/// `calculator.graph` per saved 2D curve / 3D surface. Every document-mutating
/// operation is exported as the same action its on-screen affordance dispatches.
pub fn provide_resources(ctx: &CalcResourceCtx, _doc: &AutoCommit) -> Vec<Resource> {
    let mut root = Resource::new("calculator.workspace", "calculator.workspace", "Calculator");

    // Workspace-level: add a graph formula (prompted). Offer the kind matching
    // the current mode first; both remain reachable so Ctrl+K can add to either.
    let (primary, secondary) = match ctx.mode {
        Mode::Graph3d => (GraphKind::Surface3d, GraphKind::Curve2d),
        _ => (GraphKind::Curve2d, GraphKind::Surface3d),
    };
    for kind in [primary, secondary] {
        let (id, label) = match kind {
            GraphKind::Curve2d => ("add-function", "Add function"),
            GraphKind::Surface3d => ("add-surface", "Add surface"),
        };
        root = root.with_action(ActionSpec::prompted(
            id,
            label,
            "Formula",
            "e.g. sin(x)…",
            ExecutionLocus::ClientLocal,
            move |text: &str| encode(&add_graph_action(kind, text.trim())),
        ));
    }

    // Workspace-level: clear the whole history (only when there is any).
    if !ctx.history.is_empty() {
        root = root.with_action(immediate_spec(
            "clear-history",
            "Clear history",
            &clear_history_action(),
        ));
    }

    // History entries → one resource each, deletable.
    for (i, e) in ctx.history.iter().enumerate() {
        root = root.with_child(
            Resource::new(
                format!("calculator.history_entry:{i}"),
                "calculator.history_entry",
                history_label(e),
            )
            .with_action(immediate_spec(
                "delete",
                "Delete entry",
                &delete_history_entry_action(i),
            )),
        );
    }

    // Saved graph formulas → 2D curves then 3D surfaces.
    for child in graph_resources(GraphKind::Curve2d, &ctx.funcs) {
        root = root.with_child(child);
    }
    for child in graph_resources(GraphKind::Surface3d, &ctx.surfaces) {
        root = root.with_child(child);
    }

    vec![root]
}

impl ResourceProvider for crate::app::CalculatorApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&self.calc_resource_ctx(), doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CalcModel;
    use qos_actions::{ActionInput, LocalTransport};

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// A shared base document seeded through the real `doc::store` write path
    /// (plain ops, one actor) — exactly how the live app's document is built
    /// before any action is dispatched.
    fn seeded_graph_doc(kind: GraphKind, exprs: &[&str]) -> AutoCommit {
        let mut m = CalcModel::default();
        let rows: Vec<PlotFn> = exprs
            .iter()
            .map(|e| PlotFn {
                expr: e.to_string(),
                visible: true,
            })
            .collect();
        match kind {
            GraphKind::Curve2d => m.funcs = rows,
            GraphKind::Surface3d => m.surfaces = rows,
        }
        let mut doc = AutoCommit::new();
        crate::doc::store(&mut doc, &m);
        doc
    }

    /// Apply an action through the executor exactly as the app's dispatch does.
    fn dispatch(doc: &mut AutoCommit, action: &CalculatorAction) {
        let encoded = encode(action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            _ => panic!("expected an immediate action: {}", spec.id),
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            _ => panic!("expected a prompted action: {}", spec.id),
        }
    }

    // -- test-local readers over the raw document -------------------------------

    /// Read a plain `Str` scalar's raw value (not `ScalarValue`'s quoted Display).
    fn scalar_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> String {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) => s.to_string(),
                other => other.to_string(),
            },
            _ => String::new(),
        }
    }

    fn read_history(doc: &AutoCommit) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Some(list) = list_at(doc, "history") {
            for i in 0..doc.length(&list) {
                if let Some(row) = row_at(doc, &list, i) {
                    out.push((
                        scalar_str(doc, &row, "expr"),
                        scalar_str(doc, &row, "result"),
                    ));
                }
            }
        }
        out
    }

    fn read_graph(doc: &AutoCommit, kind: GraphKind) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        if let Some(list) = list_at(doc, kind.doc_key()) {
            for i in 0..doc.length(&list) {
                if let Some(row) = row_at(doc, &list, i) {
                    let expr = doc.text(row_text(doc, &row)).unwrap_or_default();
                    out.push((expr, row_visible(doc, &row)));
                }
            }
        }
        out
    }

    /// The `expr` Text object of a graph row (panics if it isn't a Text — which
    /// is exactly what we want to assert about the stored shape).
    fn row_text(doc: &AutoCommit, row: &ObjId) -> ObjId {
        match doc.get(row, "expr") {
            Ok(Some((Value::Object(ObjType::Text), id))) => id,
            other => panic!("graph row expr must be a CRDT Text, got {other:?}"),
        }
    }

    #[test]
    fn commit_history_pushes_front_and_caps() {
        let mut doc = AutoCommit::new();
        dispatch(&mut doc, &commit_history_action("2+3", "5"));
        dispatch(&mut doc, &commit_history_action("sqrt(2)", "1.41"));
        let h = read_history(&doc);
        assert_eq!(h.len(), 2);
        assert_eq!(
            h[0],
            ("sqrt(2)".into(), "1.41".into()),
            "newest is at the front"
        );
        assert_eq!(h[1], ("2+3".into(), "5".into()));

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

    #[test]
    fn clear_and_delete_history() {
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
            vec!["e3", "e1", "e0"]
        );
        // Clear empties it.
        dispatch(&mut doc, &clear_history_action());
        assert!(read_history(&doc).is_empty());
        // Out-of-range delete is a no-op (dispatched last: the app never issues a
        // no-op action — affordances gate on a real change — so the executor's
        // empty-change handling is never exercised mid-sequence).
        dispatch(&mut doc, &delete_history_entry_action(99));
        assert!(
            read_history(&doc).is_empty(),
            "out-of-range delete changed nothing"
        );
    }

    #[test]
    fn add_toggle_set_delete_graph_rows() {
        for kind in [GraphKind::Curve2d, GraphKind::Surface3d] {
            let mut doc = AutoCommit::new();
            dispatch(&mut doc, &add_graph_action(kind, ""));
            dispatch(&mut doc, &add_graph_action(kind, "sin(x)"));
            assert_eq!(
                read_graph(&doc, kind),
                vec![(String::new(), true), ("sin(x)".into(), true)]
            );

            // Set replaces a formula (diff-spliced into the row's Text).
            dispatch(&mut doc, &set_graph_expr_action(kind, 0, "x^2"));
            assert_eq!(read_graph(&doc, kind)[0], ("x^2".into(), true));

            // Toggle flips visibility.
            dispatch(&mut doc, &toggle_graph_visible_action(kind, 1));
            assert_eq!(read_graph(&doc, kind)[1], ("sin(x)".into(), false));

            // Delete removes a row.
            dispatch(&mut doc, &delete_graph_action(kind, 0));
            assert_eq!(read_graph(&doc, kind), vec![("sin(x)".into(), false)]);
        }
    }

    #[test]
    fn graph_expr_is_stored_as_crdt_text() {
        // The formula must be a character-level CRDT Text (multi-cursor), not a
        // plain Str — `read_graph`'s `row_text` asserts exactly this shape.
        let mut doc = AutoCommit::new();
        dispatch(&mut doc, &add_graph_action(GraphKind::Curve2d, "cos(x)"));
        assert_eq!(
            read_graph(&doc, GraphKind::Curve2d),
            vec![("cos(x)".into(), true)]
        );
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the document and records exactly one undo boundary, and Ctrl+Z
    /// (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        // Seed one history entry via the real `store` write path, then move it
        // into a host State so undo is the real host path.
        let mut seeded = AutoCommit::new();
        let mut model = CalcModel::default();
        model.history = vec![HistoryEntry {
            expr: "1+1".into(),
            result: "2".into(),
        }];
        crate::doc::store(&mut seeded, &model);
        let mut state = ui::State::new();
        state.doc = seeded;
        let before_heads = state.doc.get_heads();
        let before = read_history(&state.doc).len();

        // Commit a computation via the action + the host's out-of-band undo
        // recording (the exact path `ui::App` takes after an affordance fires).
        let action = commit_history_action("6*7", "42");
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert_eq!(
            read_history(&state.doc).len(),
            before + 1,
            "the action added a history row"
        );
        assert_eq!(read_history(&state.doc)[0], ("6*7".into(), "42".into()));
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        // Ctrl+Z reverts exactly this action.
        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            read_history(&state.doc).len(),
            before,
            "undo reverted only the commit"
        );
        assert_eq!(
            read_history(&state.doc)[0],
            ("1+1".into(), "2".into()),
            "the prior entry survives"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a second
    /// independent replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same values.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        // A shared base with a couple of graph rows, built the way the live app
        // builds its document (plain `store` writes), then forked to a peer.
        let mut doc_a = seeded_graph_doc(GraphKind::Curve2d, &["x", "x^2"]);
        doc_a.commit();
        let bytes = doc_a.save();
        let mut doc_b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            doc_a.get_heads(),
            doc_b.get_heads(),
            "precondition: same base state"
        );

        // A applies a formula edit locally; B replays the envelope.
        let action = set_graph_expr_action(GraphKind::Curve2d, 1, "x^3+1");
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut doc_a, &action, encoded).unwrap();
        qos_actions::replay(&mut doc_b, &env, &registry()).unwrap();

        assert_eq!(
            doc_a.get_heads(),
            doc_b.get_heads(),
            "replay reproduced the identical change"
        );
        assert_eq!(
            read_graph(&doc_b, GraphKind::Curve2d)[1].0,
            "x^3+1",
            "edit landed on the peer"
        );
    }

    /// A toggle also replays byte-identically — the read-then-flip path, proving
    /// the deterministic "recompute from base" branch converges.
    #[test]
    fn toggle_replays_byte_identical() {
        let mut doc_a = seeded_graph_doc(GraphKind::Surface3d, &["x*y"]);
        doc_a.commit();
        let mut doc_b = AutoCommit::load(&doc_a.save()).unwrap();

        let action = toggle_graph_visible_action(GraphKind::Surface3d, 0);
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut doc_a, &action, encoded).unwrap();
        qos_actions::replay(&mut doc_b, &env, &registry()).unwrap();

        assert_eq!(
            doc_a.get_heads(),
            doc_b.get_heads(),
            "toggle replayed identically"
        );
        assert!(
            !read_graph(&doc_b, GraphKind::Surface3d)[0].1,
            "hidden on the peer"
        );
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

    #[test]
    fn ctrl_k_publishes_workspace_history_and_graph_tree() {
        let doc = AutoCommit::new();
        let resources = provide_resources(&ctx(), &doc);
        assert_eq!(resources.len(), 1, "one workspace root");
        let ws = &resources[0];
        assert_eq!(ws.kind, "calculator.workspace");
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
        assert_eq!(hist.len(), 2);
        assert!(hist[0].actions.iter().any(|a| a.id == "delete"));
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

        // The one palette surfaces the tree's actions as rows.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Add function")),
            "{labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("Clear history")),
            "{labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("Delete formula")),
            "{labels:?}"
        );
    }

    /// Single source of truth: the palette specs encode byte-identically to the
    /// builders the on-screen affordances call.
    #[test]
    fn palette_and_builder_encode_identically() {
        let doc = AutoCommit::new();
        let resources = provide_resources(&ctx(), &doc);
        let ws = &resources[0];

        // Clear-history spec == the button's builder.
        let clear = ws.actions.iter().find(|a| a.id == "clear-history").unwrap();
        assert_eq!(
            immediate_encode(clear).payload,
            encode(&clear_history_action()).payload,
            "clear-history is one source of truth"
        );

        // A curve's toggle/delete specs == the row affordance builders.
        let graphs: Vec<&Resource> = ws
            .children
            .iter()
            .filter(|r| r.kind == "calculator.graph")
            .collect();
        let curve0 = graphs[0];
        let toggle = curve0
            .actions
            .iter()
            .find(|a| a.id == "toggle-visibility")
            .unwrap();
        assert_eq!(
            immediate_encode(toggle).payload,
            encode(&toggle_graph_visible_action(GraphKind::Curve2d, 0)).payload,
            "toggle-visibility is one source of truth"
        );
        let delete = curve0.actions.iter().find(|a| a.id == "delete").unwrap();
        assert_eq!(
            immediate_encode(delete).payload,
            encode(&delete_graph_action(GraphKind::Curve2d, 0)).payload,
            "delete-formula is one source of truth"
        );

        // The curve's edit prompt builds the same action as the set builder.
        let edit = curve0.actions.iter().find(|a| a.id == "edit").unwrap();
        assert_eq!(
            prompted_encode(edit, "cos(x)").payload,
            encode(&set_graph_expr_action(GraphKind::Curve2d, 0, "cos(x)")).payload,
            "edit-formula is one source of truth"
        );

        // The workspace add-function prompt builds the same AddGraph the keypad
        // "+" affordance would when given the same formula.
        let add = ws.actions.iter().find(|a| a.id == "add-function").unwrap();
        assert_eq!(
            prompted_encode(add, "x^2").payload,
            encode(&add_graph_action(GraphKind::Curve2d, "x^2")).payload,
            "add-function is one source of truth"
        );
    }

    /// The retained accessibility tree surfaces the calculator's operable
    /// resources as stable nodes, and a node's exposed affordance is the
    /// byte-identical `EncodedAction` the on-screen control dispatches. (The
    /// calculator custom-draws its keypad, so its *operable* semantics come from
    /// the resource + action model, not per-key widgets.)
    #[test]
    fn a11y_tree_surfaces_history_delete_as_the_affordance_action() {
        let resources = provide_resources(&ctx(), &AutoCommit::new());

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
            .node(&ui::SemanticId("r:calculator.workspace".into()))
            .expect("workspace node present");
        assert_eq!(ws.name.as_deref(), Some("Calculator"));
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
        let entry = tree
            .node(&ui::SemanticId("r:calculator.history_entry:0".into()))
            .expect("history entry node");
        let exposed = match &entry.action("delete").expect("delete affordance").kind {
            ui::SemanticActionKind::App(enc) => enc.clone(),
            other => panic!("expected an app action, got {other:?}"),
        };
        assert_eq!(
            exposed,
            encode(&delete_history_entry_action(0)),
            "the a11y affordance and the on-screen builder are one action"
        );

        // Invoking the exposed action drives the doc exactly as the on-screen
        // affordance's own builder does: seed two identical docs, delete through
        // each path, and compare the results.
        let seed = |doc: &mut AutoCommit| {
            dispatch(doc, &commit_history_action("e0", "0"));
            dispatch(doc, &commit_history_action("e1", "1"));
        };
        let mut via_a11y = AutoCommit::new();
        seed(&mut via_a11y);
        let mut via_affordance = AutoCommit::new();
        seed(&mut via_affordance);

        let decoded = registry().decode(&exposed).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut via_a11y, decoded.as_ref(), exposed, &mut tx).unwrap();
        dispatch(&mut via_affordance, &delete_history_entry_action(0));

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
