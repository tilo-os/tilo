//! Calculator as a [`tilo_resource::Resource`]: one action vocabulary, one
//! publish path. Durable history and graph rows live in the document; mode,
//! expression and cameras stay local presentation on the surface.

use std::rc::Rc;

use tilo_actions::{ActionParam, ActionSpec, OperationKind, ParamType, RetryPolicy};
use ui::framework::{Fixture, ResourceApp, ResourceSelection, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_resource::items::{ItemAction, ItemCollection, item_children, item_resource_id};
use ui::tilo_resource::{ActionDecl, Doc, Id, Invoke, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{
    CALCULATOR_ACTION_TYPE, CALCULATOR_OPERATION_TYPE, CalculatorAction, CalculatorOperation,
    GraphKind, add_graph_action, clear_history_ids_action, delete_graph_action,
    delete_history_entry_action, set_graph_expr_action, toggle_graph_visible_action,
};
use crate::app::CalculatorApp;
use crate::doc::{self, FUNCS, HISTORY, SURFACES};
use crate::model::{CalcModel, HistoryEntry, PlotFn};

/// Durable calculator document state: history and saved graphs, each row
/// with its stable id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CalculatorState {
    pub history: Vec<(String, HistoryEntry)>,
    pub funcs: Vec<(String, PlotFn)>,
    pub surfaces: Vec<(String, PlotFn)>,
}

impl CalculatorState {
    fn load(doc: &Doc) -> Self {
        let history = doc::visible_ids(doc, HISTORY)
            .into_iter()
            .zip(doc::load_history(doc))
            .collect();
        let funcs = doc::visible_ids(doc, FUNCS)
            .into_iter()
            .zip(doc::load_graph(doc, FUNCS))
            .collect();
        let surfaces = doc::visible_ids(doc, SURFACES)
            .into_iter()
            .zip(doc::load_graph(doc, SURFACES))
            .collect();
        Self {
            history,
            funcs,
            surfaces,
        }
    }

    /// The rows of one graph list.
    pub fn graphs(&self, kind: GraphKind) -> &[(String, PlotFn)] {
        match kind {
            GraphKind::Curve2d => &self.funcs,
            GraphKind::Surface3d => &self.surfaces,
        }
    }

    /// The id of the `index`-th row of one graph list.
    pub fn graph_id(&self, kind: GraphKind, index: usize) -> Option<&str> {
        self.graphs(kind).get(index).map(|(id, _)| id.as_str())
    }
}

/// The item parameter of each row action, and the collection it names.
const ENTRY: &str = "entry";
const FUNCTION: &str = "function";
const SURFACE: &str = "surface";
const FORMULA: &str = "formula";

/// The published id of a history entry.
pub fn history_entry_id(id: &str) -> String {
    item_resource_id::<Calculator>(HISTORY, &Id::from_raw(id))
}

/// The published id of a graph row.
pub fn graph_row_id(kind: GraphKind, id: &str) -> String {
    item_resource_id::<Calculator>(kind_collection(kind), &Id::from_raw(id))
}

fn kind_collection(kind: GraphKind) -> &'static str {
    match kind {
        GraphKind::Curve2d => FUNCS,
        GraphKind::Surface3d => SURFACES,
    }
}

/// Document resource — the calculator's record kind.
pub struct Calculator;

impl Resource for Calculator {
    const KIND: &'static str = "calculator.document";
    const LABEL: &'static str = "Calculator";
    const ACTION_TYPE: &'static str = CALCULATOR_ACTION_TYPE;

    type State = CalculatorState;
    type Action = CalculatorAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        crate::actions::register(registry);
    }

    fn read(doc: &Doc) -> Result<CalculatorState, ModelError> {
        Ok(CalculatorState::load(doc))
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        doc::initialize(doc).map_err(ModelError::invalid)
    }

    /// The keystrokes typed into one formula are one undo step, until the
    /// field settles (a click, Enter, another row).
    fn coalesces(previous: &CalculatorAction, next: &CalculatorAction) -> bool {
        matches!(
            (previous, next),
            (
                CalculatorAction::SetGraphExpr { kind: a, id: x, .. },
                CalculatorAction::SetGraphExpr { kind: b, id: y, .. },
            ) if a == b && x == y
        )
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        let mut declarations = vec![
            ActionDecl::new(
                "add-function",
                "Add function",
                vec![ActionParam::required(
                    tilo_actions::PROMPTED_PARAM,
                    ParamType::Text,
                    "Formula",
                )],
                |args, _| {
                    let expr = args.text(tilo_actions::PROMPTED_PARAM).unwrap_or_default();
                    Ok(add_graph_action(GraphKind::Curve2d, expr.trim()))
                },
            )
            .describe("Add a 2D y = f(x) curve."),
            ActionDecl::new(
                "add-surface",
                "Add surface",
                vec![ActionParam::required(
                    tilo_actions::PROMPTED_PARAM,
                    ParamType::Text,
                    "Formula",
                )],
                |args, _| {
                    let expr = args.text(tilo_actions::PROMPTED_PARAM).unwrap_or_default();
                    Ok(add_graph_action(GraphKind::Surface3d, expr.trim()))
                },
            )
            .describe("Add a 3D z = f(x, y) surface."),
            ActionDecl::new(
                "delete-history-entry",
                "Delete entry",
                vec![item_param(ENTRY, "Calculation")],
                |args, _| Ok(delete_history_entry_action(item(args, ENTRY)?)),
            )
            .describe("Remove one calculation from the history.")
            .destructive(),
        ];
        for (kind, param, noun) in [
            (GraphKind::Curve2d, FUNCTION, "function"),
            (GraphKind::Surface3d, SURFACE, "surface"),
        ] {
            declarations.extend(graph_declarations(kind, param, noun));
        }
        declarations
    }

    fn bound_declarations(state: &CalculatorState) -> Vec<ActionDecl<Self>> {
        if state.history.is_empty() {
            return Vec::new();
        }
        let ids: Vec<String> = state.history.iter().map(|(id, _)| id.clone()).collect();
        vec![
            ActionDecl::new("clear-history", "Clear history", Vec::new(), move |_, _| {
                Ok(clear_history_ids_action(ids.clone()))
            })
            .describe("Remove every history entry.")
            .destructive(),
        ]
    }

    #[allow(
        clippy::expect_used,
        reason = "compile-time calculator operation payloads always serialize"
    )]
    fn local_operations() -> Vec<ActionSpec> {
        [
            (
                "show-calculator",
                "Show calculator",
                CalculatorOperation::ShowCalculator,
            ),
            (
                "show-graph-2d",
                "Show 2D graph",
                CalculatorOperation::ShowGraph2d,
            ),
            (
                "show-graph-3d",
                "Show 3D graph",
                CalculatorOperation::ShowGraph3d,
            ),
        ]
        .into_iter()
        .map(|(id, label, operation)| view_operation(id, label, &operation))
        .collect()
    }

    /// Every history entry and graph row, with the row actions bound to it;
    /// a history entry also recalls its calculation.
    fn children(state: &CalculatorState) -> Vec<tilo_actions::Resource> {
        let collections = [
            ItemCollection {
                field: HISTORY,
                items: state
                    .history
                    .iter()
                    .map(|(id, entry)| (Id::from_raw(id), history_label(entry)))
                    .collect(),
            },
            ItemCollection {
                field: FUNCS,
                items: graph_items(GraphKind::Curve2d, &state.funcs),
            },
            ItemCollection {
                field: SURFACES,
                items: graph_items(GraphKind::Surface3d, &state.surfaces),
            },
        ];
        let actions = [
            ("delete-history-entry", ENTRY, HISTORY),
            ("edit-function", FUNCTION, FUNCS),
            ("toggle-function", FUNCTION, FUNCS),
            ("delete-function", FUNCTION, FUNCS),
            ("edit-surface", SURFACE, SURFACES),
            ("toggle-surface", SURFACE, SURFACES),
            ("delete-surface", SURFACE, SURFACES),
        ]
        .map(|(action, param, collection)| ItemAction {
            action,
            param,
            collection: Some(collection),
        });
        let mut children = item_children::<Self>(&collections, &actions);
        for child in &mut children {
            let Some((HISTORY, id)) =
                ui::tilo_resource::items::item_of_resource::<Self>(child.id.as_str())
                    .map(|(field, id)| (field, id.as_str().to_owned()))
            else {
                continue;
            };
            child.actions.insert(
                0,
                view_operation(
                    "open",
                    "Recall calculation",
                    &CalculatorOperation::RecallHistory { id },
                ),
            );
        }
        children
    }

    fn search(state: &CalculatorState) -> Vec<ui::tilo_resource::SearchEntry> {
        let history = state
            .history
            .iter()
            .map(|(id, entry)| ui::tilo_resource::SearchEntry {
                anchor: Some(history_entry_id(id)),
                title: history_label(entry),
                text: entry.expr.clone(),
                action: None,
            });
        let graphs = [GraphKind::Curve2d, GraphKind::Surface3d]
            .into_iter()
            .flat_map(|kind| {
                state
                    .graphs(kind)
                    .iter()
                    .map(move |(id, plot)| ui::tilo_resource::SearchEntry {
                        anchor: Some(graph_row_id(kind, id)),
                        title: graph_label(kind, plot),
                        text: plot.expr.clone(),
                        action: None,
                    })
            });
        history.chain(graphs).collect()
    }
}

fn item_param(name: &'static str, label: &'static str) -> ActionParam {
    ActionParam::required(name, ParamType::Text, label)
}

fn item<'a>(args: &'a tilo_actions::ActionArgs, name: &str) -> Result<&'a str, String> {
    args.text(name)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| format!("`{name}` names no row"))
}

/// Edit, show/hide and delete one row of a graph list.
fn graph_declarations(
    kind: GraphKind,
    param: &'static str,
    noun: &'static str,
) -> [ActionDecl<Calculator>; 3] {
    let (edit, toggle, delete) = match kind {
        GraphKind::Curve2d => ("edit-function", "toggle-function", "delete-function"),
        GraphKind::Surface3d => ("edit-surface", "toggle-surface", "delete-surface"),
    };
    let row = match kind {
        GraphKind::Curve2d => "Function",
        GraphKind::Surface3d => "Surface",
    };
    [
        ActionDecl::new(
            edit,
            "Edit formula",
            vec![
                item_param(param, row),
                ActionParam::required(FORMULA, ParamType::Text, "Formula"),
            ],
            move |args, _| {
                let formula = args.text(FORMULA).unwrap_or_default().trim();
                Ok(set_graph_expr_action(kind, item(args, param)?, formula))
            },
        )
        .describe(format!("Replace the formula of a {noun}.")),
        ActionDecl::new(
            toggle,
            "Show or hide",
            vec![item_param(param, row)],
            move |args, _| Ok(toggle_graph_visible_action(kind, item(args, param)?)),
        )
        .describe(format!("Show or hide a {noun} on the graph.")),
        ActionDecl::new(
            delete,
            "Delete formula",
            vec![item_param(param, row)],
            move |args, _| Ok(delete_graph_action(kind, item(args, param)?)),
        )
        .describe(format!("Remove a {noun} from the graph."))
        .destructive(),
    ]
}

fn graph_items(kind: GraphKind, rows: &[(String, PlotFn)]) -> Vec<(Id, String)> {
    rows.iter()
        .map(|(id, plot)| (Id::from_raw(id), graph_label(kind, plot)))
        .collect()
}

#[allow(
    clippy::expect_used,
    reason = "compile-time calculator operation payloads always serialize"
)]
fn view_operation(id: &str, label: &str, operation: &CalculatorOperation) -> ActionSpec {
    ActionSpec::operation(
        id,
        label,
        OperationKind::View,
        CALCULATOR_OPERATION_TYPE,
        operation,
    )
    .expect("CalculatorOperation encodes")
    .retry_policy(RetryPolicy::Idempotent)
}

fn history_label(entry: &HistoryEntry) -> String {
    let body = if entry.expr.trim().is_empty() {
        entry.result.clone()
    } else {
        format!("{} = {}", entry.expr.trim(), entry.result.trim())
    };
    clip(&body, 48)
}

fn graph_label(kind: GraphKind, plot: &PlotFn) -> String {
    let rhs = if plot.expr.trim().is_empty() {
        "(empty)"
    } else {
        plot.expr.trim()
    };
    let lhs = match kind {
        GraphKind::Curve2d => "y",
        GraphKind::Surface3d => "z",
    };
    clip(&format!("{lhs} = {rhs}"), 48)
}

fn clip(s: &str, n: usize) -> String {
    let t: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > n {
        t.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    } else {
        t
    }
}

/// Framework app: the calculator widget is the surface over [`Calculator`].
pub struct CalculatorShell;

/// The screenshot scenarios. A plain `--shots` is the lived-in 2D graph.
const SCENARIOS: &[&str] = &[
    "calculator",
    "graph2d",
    "graph3d",
    "history-open",
    "history-closed",
    "empty",
    "narrow",
    "narrow-functions",
    "long-formula",
    "selection",
    "error",
    "remote-cursor",
];

/// A collaborator's pointer in the `remote-cursor` scenario: over the first
/// function's formula, which draws it as their caret in the math.
pub const REMOTE_POINTER: (f32, f32) = (110.0, 175.0);

impl ResourceApp for CalculatorShell {
    type Resource = Calculator;
    const ID: &'static str = "so.tilo.calculator";
    const TITLE: &'static str = "Calculator";
    const ICON: Option<&'static str> = Some("calculator");

    fn view(cx: &mut ViewCx<Calculator>) -> Element {
        cx.surface(CalculatorApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        if scenario.is_some_and(|name| name.starts_with("narrow")) {
            (480, 800)
        } else {
            (1280, 800)
        }
    }

    fn fixture(fixture: &mut Fixture<Calculator>) {
        if fixture.scenario() == Some("remote-cursor") {
            let (x, y) = REMOTE_POINTER;
            fixture.collaborator(
                "Alice",
                ResourceSelection::Pointer {
                    item: graph_row_id(GraphKind::Curve2d, &scenario_row_id(GraphKind::Curve2d, 0)),
                    x,
                    y,
                },
            );
        }
    }

    /// A lived-in calculator: five calculations, four curves and three
    /// surfaces, or one long curve for `long-formula`; nothing for `empty`.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Calculator>> {
        let model = match name {
            Some("empty") => return Vec::new(),
            Some("long-formula") => CalcModel {
                funcs: vec![PlotFn {
                    expr: LONG_FORMULA.into(),
                    visible: true,
                }],
                ..CalcModel::default()
            },
            _ => CalcModel::seeded(),
        };
        scenario_actions(&model)
            .iter()
            .filter_map(|action| Invoke::apply(action).ok())
            .collect()
    }
}

/// A formula longer than its field, which scrolls within it.
const LONG_FORMULA: &str = "sin(x)+cos(2*x)+sin(3*x)+cos(4*x)+sin(5*x)+cos(6*x)+sin(7*x)";

fn scenario_row_id(kind: GraphKind, index: usize) -> String {
    let slug = match kind {
        GraphKind::Curve2d => "curve2d",
        GraphKind::Surface3d => "surface3d",
    };
    format!("{slug}-{}", index + 1)
}

/// The actions that write `model`'s history and graphs, with fixed ids and
/// ranks so every capture is the same document.
fn scenario_actions(model: &CalcModel) -> Vec<CalculatorAction> {
    let newest = model.history.len();
    let history =
        model
            .history
            .iter()
            .enumerate()
            .map(|(index, entry)| CalculatorAction::CommitHistory {
                id: format!("history-{}", index + 1),
                rank: i64::try_from(newest - index).unwrap_or(i64::MAX),
                expr: entry.expr.clone(),
                result: entry.result.clone(),
            });
    let graphs = [
        (GraphKind::Curve2d, &model.funcs),
        (GraphKind::Surface3d, &model.surfaces),
    ]
    .into_iter()
    .flat_map(|(kind, rows)| {
        rows.iter()
            .enumerate()
            .map(move |(index, row)| CalculatorAction::AddGraph {
                kind,
                id: scenario_row_id(kind, index),
                rank: i64::try_from(index + 1).unwrap_or(i64::MAX),
                expr: row.expr.clone(),
            })
    });
    history.chain(graphs).collect()
}

impl ResourceSurface<Calculator> for CalculatorApp {
    fn present(&mut self, state: &Rc<CalculatorState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Calculator>) {
        self.attach_framework(cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::Action;
    use ui::prelude::TestApp;

    fn published(doc: &Doc) -> tilo_actions::Resource {
        let state = Calculator::read(doc).unwrap();
        ui::tilo_resource::publish::<Calculator>(Calculator::KIND, "Calculator", &state)
    }

    #[test]
    fn history_entries_are_published_items_that_recall_and_delete() {
        let mut doc = ui::automerge::AutoCommit::new();
        CalculatorAction::CommitHistory {
            id: "h1".into(),
            rank: 1,
            expr: "1+1".into(),
            result: "2".into(),
        }
        .apply(&mut doc)
        .unwrap();
        let root = published(&doc);
        let entry = root
            .children
            .iter()
            .find(|child| child.id.as_str() == history_entry_id("h1"))
            .expect("the history entry is a published item");
        let ids: Vec<&str> = entry.actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["open", "delete-history-entry"], "{ids:?}");
        assert_eq!(
            entry.actions[0].kind,
            OperationKind::View,
            "recall is presentation"
        );
        assert!(
            root.actions.iter().any(|a| a.id == "clear-history"),
            "a non-empty history can be cleared"
        );
    }

    #[test]
    fn formula_keystrokes_coalesce_per_row() {
        let set = |id: &str, expr: &str| set_graph_expr_action(GraphKind::Curve2d, id, expr);
        assert!(Calculator::coalesces(&set("a", "x"), &set("a", "x+")));
        assert!(!Calculator::coalesces(&set("a", "x"), &set("b", "x+")));
        assert!(!Calculator::coalesces(
            &set("a", "x"),
            &set_graph_expr_action(GraphKind::Surface3d, "a", "x+")
        ));
        assert!(!Calculator::coalesces(
            &set("a", "x"),
            &toggle_graph_visible_action(GraphKind::Curve2d, "a")
        ));
    }

    /// Every screenshot is the framework's capture: the scenario's actions
    /// write the document.
    #[test]
    fn captures_stage_through_actions() {
        let mut graph = TestApp::<CalculatorShell>::capture(None).unwrap();
        let state = graph.state();
        assert_eq!(state.history.len(), 5);
        assert_eq!(state.history[0].1.expr, "12*(4+3)", "newest first");
        assert_eq!(state.funcs.len(), 4);
        assert_eq!(state.surfaces.len(), 3);
        let frame = graph.harness().inspection_json().unwrap();
        assert!(frame.contains("Functions"), "{frame}");

        let empty = TestApp::<CalculatorShell>::capture(Some("empty")).unwrap();
        assert_eq!(empty.state(), CalculatorState::default());
    }
}
