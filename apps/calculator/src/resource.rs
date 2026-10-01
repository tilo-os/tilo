//! Calculator as a [`tilo_resource::Resource`]: one action vocabulary, one
//! publish path. Durable history and graph rows live in the document; mode,
//! expression and cameras stay local presentation on the surface.

use std::rc::Rc;

use automerge::{AutoCommit, ChangeHash};
use tilo_actions::{ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, RetryPolicy};
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{
    CALCULATOR_ACTION_TYPE, CALCULATOR_OPERATION_TYPE, CalculatorAction, CalculatorOperation,
    GraphKind, add_graph_action, clear_history_ids_action, delete_graph_action,
    delete_history_entry_action, encode, set_graph_expr_action, toggle_graph_visible_action,
};
use crate::app::CalculatorApp;
use crate::doc::{self, FUNCS, HISTORY, SURFACES};
use crate::model::{HistoryEntry, PlotFn};

/// Durable calculator document state: history and saved graphs.
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
}

/// Document resource — record kind matches [`crate::component`]'s
/// `calculator.document`.
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
        doc::initialize_empty(doc).map_err(ModelError::invalid)?;
        Ok(())
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "add-function",
                "Add function",
                vec![ActionParam::required(
                    tilo_actions::PROMPTED_PARAM,
                    ParamType::Text,
                    "Formula",
                )],
                |args, _| {
                    let expr = args
                        .text(tilo_actions::PROMPTED_PARAM)
                        .unwrap_or_default()
                        .trim()
                        .to_owned();
                    Ok(add_graph_action(GraphKind::Curve2d, &expr))
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
                    let expr = args
                        .text(tilo_actions::PROMPTED_PARAM)
                        .unwrap_or_default()
                        .trim()
                        .to_owned();
                    Ok(add_graph_action(GraphKind::Surface3d, &expr))
                },
            )
            .describe("Add a 3D z = f(x, y) surface."),
        ]
    }

    fn bound_actions(state: &CalculatorState) -> Vec<ActionSpec> {
        let ids = state
            .history
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        vec![
            ActionSpec::immediate(
                "clear-history",
                "Clear history",
                CALCULATOR_ACTION_TYPE,
                &clear_history_ids_action(ids),
            )
            .expect("ClearHistory encodes")
            .with_description("Remove every history entry.")
            .reversible()
            .destructive(),
        ]
    }

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
        .map(|(id, label, operation)| {
            ActionSpec::operation(
                id,
                label,
                OperationKind::View,
                CALCULATOR_OPERATION_TYPE,
                &operation,
            )
            .expect("CalculatorOperation encodes")
            .retry_policy(RetryPolicy::Idempotent)
        })
        .collect()
    }

    fn children(state: &CalculatorState) -> Vec<tilo_actions::Resource> {
        let mut children = Vec::new();
        for (id, entry) in &state.history {
            let label = history_label(entry);
            let recall = CalculatorOperation::RecallHistory { id: id.clone() };
            children.push(
                tilo_actions::Resource::new(
                    format!("calculator.history_entry:{id}"),
                    "calculator.history_entry",
                    label,
                )
                .with_action(
                    ActionSpec::operation(
                        "open",
                        "Recall calculation",
                        OperationKind::View,
                        CALCULATOR_OPERATION_TYPE,
                        &recall,
                    )
                    .expect("CalculatorOperation encodes")
                    .retry_policy(RetryPolicy::Idempotent),
                )
                .with_action(
                    ActionSpec::immediate(
                        "delete",
                        "Delete entry",
                        CALCULATOR_ACTION_TYPE,
                        &delete_history_entry_action(id.as_str()),
                    )
                    .expect("encode")
                    .reversible()
                    .destructive(),
                ),
            );
        }
        for (kind, rows) in [
            (GraphKind::Curve2d, &state.funcs),
            (GraphKind::Surface3d, &state.surfaces),
        ] {
            for (id, plot) in rows {
                children.push(graph_child(kind, id, plot));
            }
        }
        children
    }

    fn search(state: &CalculatorState) -> Vec<ui::tilo_resource::SearchEntry> {
        let history = state
            .history
            .iter()
            .map(|(id, entry)| ui::tilo_resource::SearchEntry {
                anchor: Some(format!("calculator.history_entry:{id}")),
                title: history_label(entry),
                text: entry.expr.clone(),
            });
        let graphs = state
            .funcs
            .iter()
            .map(|(id, plot)| graph_search(GraphKind::Curve2d, id, plot))
            .chain(
                state
                    .surfaces
                    .iter()
                    .map(|(id, plot)| graph_search(GraphKind::Surface3d, id, plot)),
            );
        history.chain(graphs).collect()
    }
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

fn graph_search(kind: GraphKind, id: &str, plot: &PlotFn) -> ui::tilo_resource::SearchEntry {
    let slug = match kind {
        GraphKind::Curve2d => "curve2d",
        GraphKind::Surface3d => "surface3d",
    };
    ui::tilo_resource::SearchEntry {
        anchor: Some(format!("calculator.graph:{slug}:{id}")),
        title: graph_label(kind, plot),
        text: plot.expr.clone(),
    }
}

fn graph_child(kind: GraphKind, id: &str, plot: &PlotFn) -> tilo_actions::Resource {
    let slug = match kind {
        GraphKind::Curve2d => "curve2d",
        GraphKind::Surface3d => "surface3d",
    };
    let edit_id = id.to_owned();
    let vis_label = if plot.visible { "Hide" } else { "Show" };
    tilo_actions::Resource::new(
        format!("calculator.graph:{slug}:{id}"),
        "calculator.graph",
        graph_label(kind, plot),
    )
    .with_action(ActionSpec::prompted(
        "edit",
        "Edit formula",
        "Formula",
        "e.g. sin(x)…",
        tilo_actions::ExecutionLocus::ClientLocal,
        move |text: &str| encode(&set_graph_expr_action(kind, edit_id.as_str(), text.trim())),
    ))
    .with_action(
        ActionSpec::immediate(
            "toggle-visibility",
            vis_label,
            CALCULATOR_ACTION_TYPE,
            &toggle_graph_visible_action(kind, id),
        )
        .expect("encode")
        .reversible(),
    )
    .with_action(
        ActionSpec::immediate(
            "delete",
            "Delete formula",
            CALCULATOR_ACTION_TYPE,
            &delete_graph_action(kind, id),
        )
        .expect("encode")
        .reversible()
        .destructive(),
    )
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

impl ResourceApp for CalculatorShell {
    type Resource = Calculator;
    const ID: &'static str = "so.tilo.calculator";
    const TITLE: &'static str = "Calculator";
    const ICON: Option<&'static str> = Some("calculator");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Calculator>) -> Element {
        cx.surface(CalculatorApp::new())
    }
}

/// Where the calculator's edits go: framework dispatcher when hosted, or the
/// local action host for standalone tests and benches.
#[derive(Default)]
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Calculator>>,
    last: Option<EncodedAction>,
    #[cfg(test)]
    test_hosted: bool,
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Calculator>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        #[cfg(test)]
        if self.test_hosted {
            return true;
        }
        self.framework.is_some()
    }

    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.test_hosted = true;
    }

    pub(crate) fn dispatch(&mut self, doc: &mut AutoCommit, action: &CalculatorAction) {
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(encode(action));
                let _ = dispatcher.apply_now(doc, action);
            }
            None => {
                self.local.dispatch_with_context(
                    doc,
                    CALCULATOR_ACTION_TYPE,
                    action,
                    tilo_actions::ActionContext::for_action(
                        "so.tilo.calculator",
                        "Calculator",
                        action,
                    ),
                );
            }
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<ChangeHash>> {
        self.local.take_committed()
    }
}

impl ResourceSurface<Calculator> for CalculatorApp {
    fn present(&mut self, state: &Rc<CalculatorState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Calculator>) {
        self.attach_framework(cx.dispatcher());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::Action;

    #[test]
    fn hosted_history_entry_is_the_published_row() {
        let mut doc = AutoCommit::new();
        CalculatorAction::CommitHistory {
            id: "h1".into(),
            rank: 1,
            expr: "1+1".into(),
            result: "2".into(),
        }
        .apply(&mut doc)
        .unwrap();
        doc.commit();
        let mut app = CalculatorApp::new();
        app.mark_hosted_for_test();
        assert!(ui::resources::resource(&app, &doc, Calculator::KIND).is_none());
        let entry = ui::resources::resource(&app, &doc, "calculator.history_entry:h1")
            .expect("hosted history entry");
        assert!(
            entry.actions.iter().any(|action| action.id == "open"),
            "a hosted history entry stays the published row"
        );
    }
}
