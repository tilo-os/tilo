//! Calculator as a resource: the evaluated history and the saved graphs
//! are the [`Calculator`] model; mode, expression and cameras stay local
//! presentation on the surface.

use std::rc::Rc;

use ui::framework::{
    Fixture, LiveRow, ResourceApp, ResourceSelection, ResourceSurface, SurfaceCx, ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::items::item_resource_id;
use ui::tilo_resource::{self, ActionCx, Collection, Id, Invoke, Model, Refusal, resource};
use ui::widget::Element;

use crate::actions::{CalculatorEffect, GraphKind, HISTORY_CAP};
use crate::app::CalculatorApp;
use crate::model::{CalcModel, HistoryEntry, PlotFn};

ui::embedded_catalog!(pub(crate) static CATALOG, ["en-US"]);

/// The calculator's record: its evaluated history, newest first, and its
/// saved graphs, each row with its stable id. A formula merges per
/// character; showing or hiding a row is written whole.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Calculator {
    /// History
    ///
    /// Only evaluating a calculation adds to it.
    #[model(readonly)]
    pub history: Collection<HistoryEntry>,
    /// Functions
    pub funcs: Collection<PlotFn>,
    /// Surfaces
    pub surfaces: Collection<PlotFn>,
}

impl Calculator {
    /// The rows of one graph list.
    pub fn graphs(&self, kind: GraphKind) -> &Collection<PlotFn> {
        match kind {
            GraphKind::Curve2d => &self.funcs,
            GraphKind::Surface3d => &self.surfaces,
        }
    }

    fn graphs_mut(&mut self, kind: GraphKind) -> &mut Collection<PlotFn> {
        match kind {
            GraphKind::Curve2d => &mut self.funcs,
            GraphKind::Surface3d => &mut self.surfaces,
        }
    }

    /// The id of the `index`-th row of one graph list.
    pub fn graph_id(&self, kind: GraphKind, index: usize) -> Option<&Id> {
        self.graphs(kind).ids().nth(index)
    }

    fn add(&mut self, kind: GraphKind, id: Id, formula: &str) {
        self.graphs_mut(kind).push(
            id,
            PlotFn {
                expr: formula.trim().to_owned(),
                visible: true,
            },
        );
    }

    fn remove(&mut self, kind: GraphKind, row: &Id) -> Result<(), Refusal> {
        self.graphs_mut(kind)
            .remove(row)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such formula"))
    }
}

#[resource(kind = "calculator.document", label = "Calculator")]
impl Calculator {
    /// Add an evaluated calculation to the front of the history, keeping the
    /// newest [`HISTORY_CAP`].
    #[action]
    pub fn commit_history(
        &mut self,
        cx: &mut ActionCx,
        #[param("Calculation")] expr: String,
        #[param("Result")] result: String,
    ) {
        self.history
            .insert(0, cx.id(), HistoryEntry { expr, result });
        let overflow: Vec<Id> = self.history.ids().skip(HISTORY_CAP).cloned().collect();
        for id in overflow {
            self.history.remove(&id);
        }
    }

    /// Remove one calculation from the history.
    #[action(destructive, item = history)]
    pub fn delete_history_entry(&mut self, entry: &Id) -> Result<(), Refusal> {
        self.history
            .remove(entry)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such calculation"))
    }

    /// Remove every history entry.
    #[action(destructive)]
    pub fn clear_history(&mut self) -> Result<(), Refusal> {
        if self.history.is_empty() {
            return Err(Refusal::new("the history is empty"));
        }
        self.history = Collection::default();
        Ok(())
    }

    /// Add a 2D y = f(x) curve.
    #[action]
    pub fn add_function(&mut self, cx: &mut ActionCx, #[param("Formula")] formula: String) {
        self.add(GraphKind::Curve2d, cx.id(), &formula);
    }

    /// Add a 3D z = f(x, y) surface.
    #[action]
    pub fn add_surface(&mut self, cx: &mut ActionCx, #[param("Formula")] formula: String) {
        self.add(GraphKind::Surface3d, cx.id(), &formula);
    }

    /// Remove a function from the graph.
    #[action(destructive, item = funcs)]
    pub fn delete_function(&mut self, function: &Id) -> Result<(), Refusal> {
        self.remove(GraphKind::Curve2d, function)
    }

    /// Remove a surface from the graph.
    #[action(destructive, item = surfaces)]
    pub fn delete_surface(&mut self, surface: &Id) -> Result<(), Refusal> {
        self.remove(GraphKind::Surface3d, surface)
    }
}

/// The published id of a history entry.
pub fn history_entry_id(id: &Id) -> String {
    item_resource_id::<Calculator>("history", id)
}

/// The published id of a graph row.
pub fn graph_row_id(kind: GraphKind, id: &Id) -> String {
    item_resource_id::<Calculator>(kind.field(), id)
}

/// What a history entry offers beside its verbs: recalling it, merged into
/// its published item by id.
pub(crate) fn history_row(id: &Id, entry: &HistoryEntry) -> LiveRow {
    LiveRow::new(
        history_entry_id(id),
        "calculator.document.history",
        history_label(entry),
    )
    .effect(&CalculatorEffect::Open {
        entry: id.as_str().to_owned(),
    })
}

fn history_label(entry: &HistoryEntry) -> String {
    let body = if entry.expr.trim().is_empty() {
        entry.result.clone()
    } else {
        format!("{} = {}", entry.expr.trim(), entry.result.trim())
    };
    clip(&body, 48)
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

/// The screenshot scenarios. A plain `--shots` is the lived-in 2D graph;
/// every `narrow` one captures at a phone-sized window, where each panel is a
/// floating drawer.
const SCENARIOS: &[&str] = &[
    "calculator",
    "graph2d",
    "graph3d",
    "empty",
    "narrow",
    "narrow-graph2d",
    "narrow-functions",
    "narrow-functions-page",
    "narrow-graph3d",
    "long-formula",
    "error",
    "remote-cursor",
];

/// A collaborator's pointer in the `remote-cursor` scenario: over the first
/// function's formula, which draws it as their caret in the math.
pub const REMOTE_POINTER: (f32, f32) = (145.0, 97.0);

impl ResourceApp for CalculatorShell {
    type Resource = Calculator;
    const ID: &'static str = "so.tilo.calculator";
    const TITLE: &'static str = "Calculator";
    const ICON: Option<&'static str> = Some("calculator");

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

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

    /// A lived-in calculator: its calculations, curves and surfaces, or one
    /// long curve for `long-formula`; nothing for `empty`.
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
        scenario_edits(&model)
    }
}

/// A formula longer than its field, which scrolls within it.
const LONG_FORMULA: &str = "sin(x)+cos(2*x)+sin(3*x)+cos(4*x)+sin(5*x)+cos(6*x)+sin(7*x)";

fn scenario_row_id(kind: GraphKind, index: usize) -> Id {
    let slug = match kind {
        GraphKind::Curve2d => "curve2d",
        GraphKind::Surface3d => "surface3d",
    };
    Id::from_raw(format!("{slug}-{}", index + 1))
}

/// The calculations and edits that write `model`'s history and graphs,
/// the graph rows with fixed ids so a collaborator's pointer finds its row.
fn scenario_edits(model: &CalcModel) -> Vec<Invoke<Calculator>> {
    // Each calculation lands at the front: commit the oldest first.
    let history = model.history.iter().rev().map(|entry| {
        Ok(Calculator::COMMIT_HISTORY.with((entry.expr.clone(), entry.result.clone())))
    });
    let graphs = [
        (GraphKind::Curve2d, &model.funcs),
        (GraphKind::Surface3d, &model.surfaces),
    ]
    .into_iter()
    .flat_map(|(kind, rows)| {
        rows.iter().enumerate().map(move |(at, row)| {
            let row = PlotFn {
                visible: true,
                ..row.clone()
            };
            Invoke::edit(&kind.rows().insert_as(&scenario_row_id(kind, at), at, &row))
        })
    });
    history.chain(graphs).filter_map(Result::ok).collect()
}

impl ResourceSurface<Calculator, CalculatorEffect> for CalculatorApp {
    fn effect(
        &mut self,
        effect: CalculatorEffect,
        _cx: &mut ui::framework::EffectCx<Calculator, Self>,
    ) -> ui::framework::EffectAnswer {
        self.queue_effect(effect)?;
        Ok(ui::framework::Effected::done())
    }

    fn present(&mut self, state: &Rc<Calculator>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Calculator>) {
        self.attach_framework(cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    /// Each history entry's recall, merged into its item.
    fn live_rows(&self) -> Vec<LiveRow> {
        self.history_rows()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::prelude::TestApp;
    use ui::tilo_resource::{Edit, Replica, Resource as _};

    fn edit(calculator: &mut Replica<Calculator>, edit: &Edit<Calculator>) {
        calculator.apply(Invoke::edit(edit).unwrap()).unwrap();
    }

    fn history(calculator: &Replica<Calculator>) -> Vec<(String, String)> {
        calculator
            .state()
            .unwrap()
            .history
            .values()
            .map(|entry| (entry.expr.clone(), entry.result.clone()))
            .collect()
    }

    fn graph(calculator: &Replica<Calculator>, kind: GraphKind) -> Vec<(String, bool)> {
        calculator
            .state()
            .unwrap()
            .graphs(kind)
            .values()
            .map(|row| (row.expr.clone(), row.visible))
            .collect()
    }

    fn commit(calculator: &mut Replica<Calculator>, expr: &str, result: &str) {
        calculator
            .apply(Calculator::COMMIT_HISTORY.with((expr.to_owned(), result.to_owned())))
            .unwrap();
    }

    fn add(calculator: &mut Replica<Calculator>, kind: GraphKind, formula: &str) {
        let verb = match kind {
            GraphKind::Curve2d => Calculator::ADD_FUNCTION.with(formula),
            GraphKind::Surface3d => Calculator::ADD_SURFACE.with(formula),
        };
        calculator.apply(verb).unwrap();
    }

    fn nth(calculator: &Replica<Calculator>, kind: GraphKind, n: usize) -> Id {
        calculator
            .state()
            .unwrap()
            .graph_id(kind, n)
            .cloned()
            .unwrap()
    }

    /// Committing pushes to the front and keeps the newest `HISTORY_CAP`;
    /// deleting removes one entry, clearing empties it, and both ask first.
    #[test]
    fn history_commits_capped_deletes_and_clears() {
        let mut calculator = Replica::<Calculator>::new();
        commit(&mut calculator, "2+3", "5");
        commit(&mut calculator, "sqrt(2)", "1.41");
        assert_eq!(
            history(&calculator),
            [
                ("sqrt(2)".to_owned(), "1.41".to_owned()),
                ("2+3".to_owned(), "5".to_owned())
            ],
            "newest is at the front"
        );
        for i in 0..(HISTORY_CAP + 10) {
            commit(&mut calculator, &i.to_string(), &i.to_string());
        }
        let capped = history(&calculator);
        assert_eq!(capped.len(), HISTORY_CAP, "history is capped");
        assert_eq!(capped[0].0, (HISTORY_CAP + 10 - 1).to_string());
        assert!(
            calculator
                .apply(
                    Invoke::edit(
                        &Calculator::paths()
                            .history
                            .remove(calculator.state().unwrap().history.ids().next().unwrap())
                    )
                    .unwrap()
                )
                .is_err(),
            "only the verbs change the history"
        );

        let mut calculator = Replica::<Calculator>::new();
        for i in 0..4 {
            commit(&mut calculator, &format!("e{i}"), &i.to_string());
        }
        let e2 = calculator
            .state()
            .unwrap()
            .history
            .ids()
            .nth(1)
            .cloned()
            .unwrap();
        let delete = || Calculator::DELETE_HISTORY_ENTRY.with(e2.clone());
        calculator
            .apply(delete())
            .expect("Undo takes a deleted entry back, so deleting asks nothing");
        assert_eq!(
            history(&calculator)
                .into_iter()
                .map(|(expr, _)| expr)
                .collect::<Vec<_>>(),
            ["e3", "e1", "e0"]
        );
        assert!(calculator.apply_confirmed(delete()).is_err(), "it is gone");
        calculator
            .apply_confirmed(Calculator::CLEAR_HISTORY.with(()))
            .unwrap();
        assert!(history(&calculator).is_empty());
        assert!(
            calculator
                .apply_confirmed(Calculator::CLEAR_HISTORY.with(()))
                .is_err(),
            "an empty history has nothing to clear"
        );
    }

    /// Graph rows are added by verb, edited and shown or hidden at their
    /// paths, and deleted by verb, the same for curves and surfaces.
    #[test]
    fn graph_rows_add_edit_toggle_and_delete() {
        for kind in [GraphKind::Curve2d, GraphKind::Surface3d] {
            let mut calculator = Replica::<Calculator>::new();
            add(&mut calculator, kind, "");
            add(&mut calculator, kind, "sin(x)");
            assert_eq!(
                graph(&calculator, kind),
                [(String::new(), true), ("sin(x)".to_owned(), true)],
                "{kind:?}: add"
            );
            let (first, second) = (nth(&calculator, kind, 0), nth(&calculator, kind, 1));
            edit(
                &mut calculator,
                &kind.rows().item(&first).expr.replace("x^2"),
            );
            edit(
                &mut calculator,
                &kind.rows().item(&second).visible.set(false),
            );
            assert_eq!(
                graph(&calculator, kind),
                [("x^2".to_owned(), true), ("sin(x)".to_owned(), false)],
                "{kind:?}: edit and toggle"
            );
            let delete = match kind {
                GraphKind::Curve2d => Calculator::DELETE_FUNCTION.with(first),
                GraphKind::Surface3d => Calculator::DELETE_SURFACE.with(first),
            };
            calculator.apply_confirmed(delete).unwrap();
            assert_eq!(
                graph(&calculator, kind),
                [("sin(x)".to_owned(), false)],
                "{kind:?}: delete"
            );
        }
    }

    /// Two people at once converge: formulas merge per character, a row
    /// one deleted stays deleted whatever the other typed into it.
    #[test]
    fn concurrent_edits_converge_and_delete_wins() {
        let mut ours = Replica::<Calculator>::new();
        add(&mut ours, GraphKind::Curve2d, "x");
        add(&mut ours, GraphKind::Curve2d, "sin(x)");
        let (first, second) = (
            nth(&ours, GraphKind::Curve2d, 0),
            nth(&ours, GraphKind::Curve2d, 1),
        );
        let mut theirs = ours.fork();
        let rows = GraphKind::Curve2d.rows();
        edit(&mut ours, &rows.item(&second).expr.splice(0, 0, "2*"));
        edit(&mut theirs, &rows.item(&second).expr.splice(6, 0, "+1"));
        ours.apply_confirmed(Calculator::DELETE_FUNCTION.with(first.clone()))
            .unwrap();
        edit(&mut theirs, &rows.item(&first).expr.replace("x^2"));
        ours.sync_with(&mut theirs).unwrap();
        assert_eq!(ours.state().unwrap(), theirs.state().unwrap());
        assert_eq!(
            graph(&ours, GraphKind::Curve2d),
            [("2*sin(x)+1".to_owned(), true)],
            "the deleted row is not resurrected by a late edit"
        );
    }

    /// Each history entry and graph row is a published item with its
    /// verbs bound; a history entry also recalls its calculation.
    #[test]
    fn history_and_graph_rows_are_published_items() {
        let mut app = TestApp::<CalculatorShell>::new().unwrap();
        app.call(
            "commit-history",
            serde_json::json!({"expr": "1+1", "result": "2"}),
        )
        .unwrap();
        app.call("add-surface", serde_json::json!({"formula": "x*y"}))
            .unwrap();
        app.settle();
        let id = app.state().history.ids().next().cloned().unwrap();
        let resources = app.harness().resources();
        let root = resources
            .iter()
            .find(|resource| resource.kind == Calculator::KIND)
            .unwrap();
        for id in ["add-function", "add-surface", "clear-history", "field-edit"] {
            assert!(root.actions.iter().any(|action| action.id == id), "{id}");
        }
        let entry = root
            .children
            .iter()
            .find(|child| child.id.as_str() == history_entry_id(&id))
            .expect("the history entry is a published item");
        let open = entry
            .actions
            .iter()
            .find(|action| action.id == "open")
            .expect("an entry recalls its calculation");
        assert_eq!(open.kind, ui::tilo_actions::OperationKind::View);
        let delete = entry
            .actions
            .iter()
            .find(|action| action.id == "delete-history-entry")
            .unwrap();
        // Undo takes a deleted entry back: it runs at once and asks nothing.
        assert!(delete.contract.can_be_undone());
        assert_eq!(
            delete.contract.confirmation,
            ui::tilo_actions::ConfirmationPolicy::None
        );
        let surface = app.state().surfaces.ids().next().cloned().unwrap();
        let row = root
            .children
            .iter()
            .find(|child| child.id.as_str() == graph_row_id(GraphKind::Surface3d, &surface))
            .expect("a surface row");
        assert!(
            row.actions
                .iter()
                .any(|action| action.id == "delete-surface")
        );
    }

    /// The keystrokes typed into one formula are one undo step; editing
    /// another row starts another.
    #[test]
    fn formula_keystrokes_coalesce_per_row() {
        let rows = GraphKind::Curve2d.rows();
        let action = |edit: &Edit<Calculator>| Invoke::edit(edit).unwrap().build(0).unwrap();
        let (a, b) = (Id::from_raw("a"), Id::from_raw("b"));
        let set = |row: &Id, expr: &str| action(&rows.item(row).expr.replace(expr));
        assert!(Calculator::coalesces(&set(&a, "x"), &set(&a, "x+")));
        assert!(!Calculator::coalesces(&set(&a, "x"), &set(&b, "x+")));
        assert!(!Calculator::coalesces(
            &set(&a, "x"),
            &action(&GraphKind::Surface3d.rows().item(&a).expr.replace("x+"))
        ));
        assert!(!Calculator::coalesces(
            &set(&a, "x"),
            &action(&rows.item(&a).visible.set(false))
        ));
    }

    /// What an agent does in the calculator is history the person reads as
    /// the app's own sentences.
    #[test]
    fn history_entries_render_as_the_apps_sentences() {
        use ui::tilo_actions::{ActionInvoker, ActorKind};

        let mut app = TestApp::<CalculatorShell>::new().unwrap();
        let agent = ActionInvoker::actor("agent:claude")
            .with_kind(ActorKind::Agent)
            .acting_for("ada");
        app.call_by(
            agent.clone(),
            Calculator::KIND,
            "commit-history",
            serde_json::json!({"expr": "6*7", "result": "42"}),
        )
        .unwrap();
        app.call_by(
            agent.clone(),
            Calculator::KIND,
            "add-function",
            serde_json::json!({"formula": "sin(x)"}),
        )
        .unwrap();
        let row = app.state().funcs.ids().next().cloned().unwrap();
        for edit in [
            GraphKind::Curve2d.rows().item(&row).visible.set(false),
            GraphKind::Curve2d.rows().item(&row).expr.replace("cos(x)"),
        ] {
            app.call_by(
                agent.clone(),
                Calculator::KIND,
                "field-edit",
                serde_json::json!({ "edit": edit }),
            )
            .unwrap();
        }
        let entries: Vec<_> = app
            .history()
            .into_iter()
            .filter(|entry| {
                entry
                    .invoker
                    .as_ref()
                    .is_some_and(|invoker| invoker.actor == "agent:claude")
            })
            .collect();
        let sentences = app.sentences(&entries);
        assert_eq!(
            sentences,
            [
                "Calculated 6*7 = 42",
                "Added the function y = sin(x)",
                "Hid sin(x)",
                "Changed the formula sin(x)",
            ]
        );
        let invoker = entries[0].invoker.as_ref().unwrap();
        assert_eq!(
            (invoker.kind, invoker.on_behalf_of.as_deref()),
            (ActorKind::Agent, Some("ada"))
        );
    }

    /// Every screenshot is the framework's capture: the scenario's edits
    /// write the document.
    #[test]
    fn captures_stage_through_edits() {
        let mut graph = TestApp::<CalculatorShell>::capture(None).unwrap();
        let state = graph.state();
        assert_eq!(state.history.len(), crate::model::DEMO_HISTORY.len());
        assert_eq!(
            state.history.values().next().unwrap().expr,
            "12*(4+3)",
            "newest first"
        );
        assert_eq!(state.funcs.len(), crate::model::DEMO_CURVES.len());
        assert_eq!(state.surfaces.len(), crate::model::DEMO_SURFACES.len());
        let frame = graph.harness().inspection_json().unwrap();
        assert!(frame.contains("Functions"), "{frame}");

        let empty = TestApp::<CalculatorShell>::capture(Some("empty")).unwrap();
        assert_eq!(empty.state(), Calculator::default());
    }
}

#[cfg(test)]
mod scenario_tests {
    use super::*;

    /// Every scenario stages a lived-in calculator through the document's own
    /// actions, and each narrow one captures at a phone-sized window.
    #[test]
    fn scenarios_stage_lived_in_content_and_declare_narrow_viewports() {
        let full = crate::model::DEMO_HISTORY.len()
            + crate::model::DEMO_CURVES.len()
            + crate::model::DEMO_SURFACES.len();
        for name in CalculatorShell::scenarios() {
            let expected = match *name {
                "empty" => 0,
                "long-formula" => 1,
                _ => full,
            };
            assert_eq!(
                CalculatorShell::scenario(Some(name)).len(),
                expected,
                "scenario {name}"
            );
            let narrow = name.starts_with("narrow");
            assert_eq!(
                CalculatorShell::shot_viewport(Some(name)).0 < 600,
                narrow,
                "viewport of {name}"
            );
        }
        assert_eq!(
            CalculatorShell::scenario(None).len(),
            full,
            "a plain capture is lived in too"
        );
    }
}

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::CalculatorShell::scenarios() {
            let mut app = TestApp::<super::CalculatorShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}
