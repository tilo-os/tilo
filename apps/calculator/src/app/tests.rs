use super::*;
use crate::resource::{CalculatorShell, history_entry_id};
use ui::prelude::TestApp;
use ui::tilo_resource::Invoke;

include!("../local_capture_tests.rs");
include!("../typing_tests.rs");
include!("../history_tests.rs");

fn keyboard(key: WKey) -> WindowEvent {
    let text = if let WKey::Character(text) = &key {
        Some(text.clone())
    } else {
        None
    };
    ui::synthetic_input::keyboard_input(winit::event::KeyEvent::synthetic(
        winit::event::RawKeyEvent {
            physical_key: winit::keyboard::PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified,
            ),
            state: ElementState::Pressed,
        },
        key.clone(),
        text.clone(),
        winit::keyboard::KeyLocation::Standard,
        false,
        text,
        key,
    ))
}

#[test]
fn calculator_mathfield_edits_fractions_roots_and_continues_results() {
    let mut app = CalculatorApp::new();
    let mut state = ui::State::new();
    app.set_focus(true);
    for key in [
        WKey::Character("1/2".into()),
        WKey::Named(NamedKey::ArrowRight),
        WKey::Character("+sqrt(9)".into()),
    ] {
        send(&mut app, &mut state, &keyboard(key));
    }
    assert!((engine::evaluate(&app.model.expr, &app.model.ctx()).unwrap() - 3.5).abs() < 1e-10);
    app.commit_equals(&mut Edits::lend(&mut state.doc));
    assert_eq!(app.model.ans, 3.5);
    app.apply_calc_key(&model::Key::Ins("+"));
    app.apply_calc_key(&model::Key::Digit('2'));
    app.commit_equals(&mut Edits::lend(&mut state.doc));
    assert_eq!(app.model.ans, 5.5);
    app.apply_calc_key(&model::Key::Digit('7'));
    assert_eq!(app.model.expr, "7");
    app.apply_calc_key(&model::Key::Backspace);
    assert!(app.model.expr.is_empty());
}

/// A hosted calculator: the framework's live app with its resource.
fn hosted() -> TestApp<CalculatorShell> {
    let mut app = TestApp::<CalculatorShell>::new().unwrap();
    app.harness().resize((1280, 800)).unwrap();
    app
}

/// The calculator's retained node in the inspection tree: its path, and
/// the semantic tree it publishes.
fn calculator(app: &mut TestApp<CalculatorShell>) -> (String, ui::a11y::AccessNode) {
    fn walk(node: &ui::headless::InspectionNode) -> Option<(String, ui::a11y::AccessNode)> {
        match &node.semantics {
            Some(semantics)
                if semantics.description.as_deref()
                    == Some("Calculator and graphing workspace") =>
            {
                Some((node.path.clone(), semantics.clone()))
            }
            _ => node.children.iter().find_map(walk),
        }
    }
    walk(&app.harness().inspection().root).expect("the calculator is laid out")
}

/// The semantic node the calculator publishes for `target`.
fn node(app: &mut TestApp<CalculatorShell>, target: &str) -> ui::a11y::AccessNode {
    fn find(node: &ui::a11y::AccessNode, target: &str) -> Option<ui::a11y::AccessNode> {
        if node.action_target.as_deref() == Some(target) {
            return Some(node.clone());
        }
        node.children.iter().find_map(|child| find(child, target))
    }
    find(&calculator(app).1, target).unwrap_or_else(|| panic!("no semantic node {target}"))
}

/// Press `id` in the window's bar, as the desktop hands the press to
/// the app.
fn press_bar(app: &mut TestApp<CalculatorShell>, id: &str) {
    let declared = ui::window_toolbar::declared().expect("the window declares its slots");
    let offered = declared
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .chain(
            declared
                .views
                .iter()
                .flat_map(|views| views.choices.iter().map(|view| view.id.as_str())),
        )
        .any(|offered| offered == id);
    assert!(offered, "{id} is in the window's bar: {declared:?}");
    app.harness()
        .press_window_slot(ui::window_toolbar::ToolbarEvent::Action { id: id.into() });
    app.settle();
}

/// Click the painted control `target`, as a pointer does.
fn click(app: &mut TestApp<CalculatorShell>, target: &str) {
    let bounds = node(app, target)
        .bounds
        .unwrap_or_else(|| panic!("{target} is laid out"));
    app.harness().click_at(
        f64::midpoint(bounds.x0, bounds.x1),
        f64::midpoint(bounds.y0, bounds.y1),
    );
    app.settle();
}

/// A capture stages its view over the scenario's rows: the mode, an
/// open drawer, a collaborator's pointer over the first formula.
#[test]
fn captures_stage_mode_drawers_and_collaborators() {
    let _calc = TestApp::<CalculatorShell>::capture(Some("calculator")).unwrap();
    let toolbar = ui::window_toolbar::declared().expect("the window declares its slots");
    assert_eq!(
        toolbar.views.as_ref().map(|views| views.chosen.as_str()),
        Some("calculator.mode:0")
    );
    assert_eq!(
        toolbar
            .location
            .as_ref()
            .map(|location| location.label.as_str()),
        Some("Calculator")
    );

    let mut narrow = TestApp::<CalculatorShell>::capture(Some("narrow-functions")).unwrap();
    assert_eq!(narrow.harness().inspection().viewport, [480, 800]);
    assert_eq!(
        ui::window_toolbar::declared().and_then(|toolbar| toolbar.sidebar),
        Some(true),
        "the narrow functions drawer is staged open"
    );

    let mut remote = TestApp::<CalculatorShell>::capture(Some("remote-cursor")).unwrap();
    let field = node(&mut remote, "calculator.graph.field:2d:0")
        .bounds
        .expect("the first formula is laid out");
    let (x, y) = crate::resource::REMOTE_POINTER;
    let (x, y) = (f64::from(x), f64::from(y));
    assert!(
        field.x0 <= x && x <= field.x1 && field.y0 <= y && y <= field.y1,
        "the collaborator's pointer is over the first formula: {field:?}"
    );
}

#[test]
fn keyboard_and_ime_only_mutate_the_host_focused_calculator() {
    let mut app = CalculatorApp::new();
    let mut state = ui::State::new();

    assert!(
        !send(&mut app, &mut state, &keyboard(WKey::Character("2".into())),),
        "an unfocused Calculator must leave keyboard input for the focused sibling"
    );
    assert!(app.model.expr.is_empty());
    assert!(
        !send(
            &mut app,
            &mut state,
            &WindowEvent::Ime(winit::event::Ime::Commit("3".into())),
        ),
        "an unfocused Calculator must leave IME commits for the focused sibling"
    );
    assert!(app.model.expr.is_empty());
    assert!(
        !send(
            &mut app,
            &mut state,
            &WindowEvent::Ime(winit::event::Ime::Preedit("3".into(), Some((1, 1)))),
        ),
        "an unfocused Calculator must not capture an IME preedit"
    );

    app.set_focus(true);
    assert!(send(
        &mut app,
        &mut state,
        &keyboard(WKey::Character("2".into())),
    ));
    assert_eq!(app.model.expr, "2");

    app.set_mode(Mode::Graph2d);
    app.model.funcs = vec![PlotFn {
        expr: String::new(),
        visible: true,
    }];
    app.model.active_fn = 0;
    assert!(send(
        &mut app,
        &mut state,
        &keyboard(WKey::Character("x".into())),
    ));
    assert_eq!(app.model.funcs[0].expr, "x");

    app.set_mode(Mode::Graph3d);
    app.model.surfaces = vec![PlotFn {
        expr: "x+y".into(),
        visible: true,
    }];
    app.model.active_surface = 0;
    assert!(send(
        &mut app,
        &mut state,
        &keyboard(WKey::Named(NamedKey::Backspace)),
    ));
    assert_eq!(app.model.surfaces[0].expr, "x+");

    app.set_focus(false);
    let graph_expression = app.model.surfaces[0].expr.clone();
    assert!(
        !send(&mut app, &mut state, &keyboard(WKey::Character("y".into())),),
        "an unfocused Calculator must not consume graph keyboard edits"
    );
    assert_eq!(app.model.surfaces[0].expr, graph_expression);
}

#[test]
fn integral_keypad_and_keyboard_commit_a_typeset_history_expression() {
    let mut app = CalculatorApp::new();
    let mut state = ui::State::new();
    app.set_focus(true);
    app.apply_calc_key(&model::Key::Ins("int("));
    assert_eq!(app.model.expr, "int(,,,)");
    for character in "sin(x),x,0,pi)".chars() {
        send(
            &mut app,
            &mut state,
            &keyboard(WKey::Character(character.to_string().into())),
        );
    }
    assert_eq!(app.model.expr, "int(sin(x),x,0,π)");
    send(
        &mut app,
        &mut state,
        &keyboard(WKey::Named(NamedKey::Enter)),
    );
    assert!((app.model.ans - 2.0).abs() < 1e-9);
    assert_eq!(app.model.history.len(), 1);
    assert_eq!(app.model.history[0].expr, "int(sin(x),x,0,π)");
    let mut field = MathField::new(&app.model.history[0].expr);
    let mut text = ui::text_render::TextRenderer::new();
    assert!(field.content_height(&mut text) > 0.0);
}

#[test]
fn power_input_physical_keys_evaluates_once() {
    let mut app = CalculatorApp::new();
    let mut state = ui::State::new();
    app.set_focus(true);
    for text in ["2", "^", "2"] {
        send(
            &mut app,
            &mut state,
            &keyboard(WKey::Character(text.into())),
        );
    }
    assert_eq!(app.model.expr, "2^(2)");
    send(
        &mut app,
        &mut state,
        &keyboard(WKey::Named(NamedKey::Enter)),
    );
    assert_eq!(app.model.ans, 4.0);
    assert_eq!(app.model.history.len(), 1);
}

#[test]
fn power_input_dead_key_commit_preserves_caret() {
    use winit::event::Ime;
    let mut app = CalculatorApp::new();
    let mut state = ui::State::new();
    app.set_focus(true);
    send(&mut app, &mut state, &keyboard(WKey::Character("2".into())));
    send(&mut app, &mut state, &keyboard(WKey::Dead(Some('^'))));
    send(
        &mut app,
        &mut state,
        &WindowEvent::Ime(Ime::Preedit("^".into(), Some((1, 1)))),
    );
    assert_eq!(app.model.expr, "2", "preedit must not insert an operator");
    send(
        &mut app,
        &mut state,
        &WindowEvent::Ime(Ime::Commit("^".into())),
    );
    send(&mut app, &mut state, &keyboard(WKey::Character("2".into())));
    assert_eq!(app.model.expr, "2^(2)");
    send(
        &mut app,
        &mut state,
        &keyboard(WKey::Named(NamedKey::Enter)),
    );
    assert_eq!(app.model.ans, 4.0);
    assert_eq!(app.model.history.len(), 1);
}

#[test]
fn power_input_browser_commits_evaluate_once() {
    let mut app = CalculatorApp::new();
    let mut state = ui::State::new();
    app.set_focus(true);
    for text in ["2", "^", "2"] {
        send(
            &mut app,
            &mut state,
            &WindowEvent::Ime(winit::event::Ime::Commit(text.into())),
        );
    }
    assert_eq!(app.model.expr, "2^(2)");
    send(
        &mut app,
        &mut state,
        &keyboard(WKey::Named(NamedKey::Enter)),
    );
    assert_eq!(app.model.ans, 4.0);
    assert_eq!(app.model.history.len(), 1);
}

#[test]
fn power_input_focused_calculator_enables_browser_text_editor() {
    let mut app = CalculatorApp::new();
    app.area = Rect::new(0.0, 0.0, 800.0, 600.0);
    app.relayout();
    app.layout_active();
    assert!(app.ime_cursor_rect().is_none());
    app.set_focus(true);
    let caret = app
        .ime_cursor_rect()
        .expect("text entry must enable the browser editor and platform IME");
    assert!(caret.height() > 0.0 && app.area.contains(caret.center()));
    assert_eq!(app.ime_content(), Some(ui::ime::InputContent::TEXT));
    app.set_focus(false);
    assert!(app.ime_cursor_rect().is_none());
    assert_eq!(app.ime_content(), None);
}

#[test]
fn power_input_paste_shortcuts_evaluate_once() {
    for primary in [
        winit::keyboard::ModifiersState::CONTROL,
        winit::keyboard::ModifiersState::SUPER,
    ] {
        let mut app = CalculatorApp::new();
        let mut state = ui::State::new();
        app.set_focus(true);
        app.on_key_with_paste(
            &WKey::Character("v".into()),
            primary.into(),
            &mut Edits::lend(&mut state.doc),
            || Some("2^2".into()),
        );
        assert_eq!(app.model.expr, "2^(2)");
        send(
            &mut app,
            &mut state,
            &keyboard(WKey::Named(NamedKey::Enter)),
        );
        assert_eq!(app.model.ans, 4.0);
        assert_eq!(app.model.history.len(), 1);
    }
}

/// A history row's published `open` recalls exactly that row by its
/// stable identity — surviving index shifts, refusing a deleted row —
/// and is presentation only, open to a participant who may only view.
#[test]
fn history_recall_by_published_action_uses_stable_identity() {
    // A window someone uses, where effects run, over the scenario's book.
    let mut app = TestApp::<CalculatorShell>::new().unwrap();
    for invoke in <CalculatorShell as ui::framework::ResourceApp>::scenario(Some("graph3d")) {
        app.invoke(invoke).unwrap();
    }
    app.settle();
    let (id, expression) = {
        let state = app.state();
        let (id, entry) = state.history.iter().nth(1).unwrap();
        (id.clone(), entry.expr.clone())
    };
    // A new calculation moves every positional index.
    app.invoke(Calculator::COMMIT_HISTORY.with(("999+1".to_owned(), "1000".to_owned())))
        .unwrap();
    let heads = app.read_document(|doc| doc.clone().get_heads());
    app.call_on(&history_entry_id(&id), "open", serde_json::json!({}))
        .unwrap();
    app.settle();
    assert_eq!(
        app.read_document(|doc| doc.clone().get_heads()),
        heads,
        "recall is presentation only"
    );
    assert_eq!(
        calculator(&mut app).1.value.as_deref(),
        Some(expression.as_str()),
        "the recalled calculation is the expression"
    );
    assert_eq!(
        ui::window_toolbar::declared()
            .and_then(|toolbar| toolbar.views)
            .map(|views| views.chosen),
        Some("calculator.mode:0".to_owned()),
        "recall shows the calculator"
    );

    app.invoke_confirmed(Calculator::DELETE_HISTORY_ENTRY.with(id.clone()))
        .unwrap();
    assert!(
        app.call_on(&history_entry_id(&id), "open", serde_json::json!({}))
            .is_err(),
        "a deleted row is no longer offered"
    );
}

/// Launch and mode-switch state: the live app boots empty in Calc, and
/// entering an empty graph mode adds an editable starter row through
/// the resource's action.
#[test]
fn launch_state_and_mode_switch_seeding() {
    let app = CalculatorApp::new();
    assert_eq!(app.model.mode, Mode::Calc, "live launch: Calc mode");
    assert!(app.model.expr.is_empty(), "live launch: empty expression");
    assert!(app.model.history.is_empty(), "live launch: no history");
    assert!(app.model.funcs.is_empty(), "live launch: nothing plotted");
    assert_eq!(app.model.memory, 0.0, "live launch: empty memory");
    assert_eq!(app.model.ans, 0.0, "live launch: ans is 0");

    let mut app = hosted();
    assert_eq!(app.state(), Calculator::default());
    press_bar(&mut app, "calculator.mode:1");
    let state = app.state();
    assert_eq!(state.funcs.len(), 1, "Graph 2D adds one empty row");
    assert!(state.funcs.values().next().unwrap().expr.is_empty());
    press_bar(&mut app, "calculator.mode:2");
    let state = app.state();
    assert_eq!(state.surfaces.len(), 1, "Graph 3D adds one starter surface");
    assert_eq!(
        state.surfaces.values().next().unwrap().expr,
        model::DEFAULT_SURFACE
    );
    // A second visit adds nothing.
    press_bar(&mut app, "calculator.mode:1");
    assert_eq!(app.state().funcs.len(), 1);
    assert!(app.undo(), "the starter is an undo step");
    assert_eq!(app.state().surfaces.len(), 0);
}

/// The semantic tree exposes the header, keypad, expression and graph
/// formula controls, and activating/setting them through
/// `perform_semantic_action` drives the model. Fresh app per phase.
#[test]
#[allow(
    clippy::items_after_statements,
    reason = "the local sibling-key assertion reads most clearly next to the tree it validates"
)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn accessibility_exposes_and_activates_calculator_controls() {
    use ui::a11y::Action;
    use ui::widget::Widget as _;

    // phase accessibility_activates_the_window_bars_controls
    {
        let mut app = CalculatorApp::new();
        let tree = app.accessibility().expect("calculator semantics");
        assert_eq!(
            tree.name.as_deref(),
            Some("Calculator"),
            "header: root name"
        );
        fn siblings_are_consistently_keyed(nodes: &[ui::a11y::AccessNode]) -> bool {
            nodes.is_empty()
                || nodes.iter().all(|node| node.key.is_some())
                || nodes.iter().all(|node| node.key.is_none())
        }
        assert!(
            siblings_are_consistently_keyed(&tree.children),
            "synthetic Calculator siblings must not mix stable and ordinal keys"
        );
        // The modes and the angle unit are the window bar's: the bar's
        // slots run these same semantic actions.

        let mut doc = AutoCommit::new();
        assert!(
            app.perform_semantic_action(
                "calculator.mode:1",
                &Action::Activate,
                &mut Edits::lend(&mut doc),
                &automerge::ROOT,
            ),
            "header: mode activate handled"
        );
        assert_eq!(app.model.mode, Mode::Graph2d, "header: mode switched");
        assert!(
            app.perform_semantic_action(
                "calculator.angle:degrees",
                &Action::ToggleCheck,
                &mut Edits::lend(&mut doc),
                &automerge::ROOT,
            ),
            "header: angle toggle handled"
        );
        assert!(app.model.deg, "header: degrees on");
    }

    // phase accessibility_exposes_keypad_and_graph_formula_controls
    {
        let mut app = CalculatorApp::new();
        app.area = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        app.layout_active();
        let tree = app.accessibility().expect("calculator semantics");
        let keypad = tree
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Scientific keypad"))
            .expect("all painted keypad keys are exposed");
        let expression = tree
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Expression"))
            .expect("expression is a semantic text input");
        assert_eq!(
            expression.role,
            ui::a11y::Role::TextInput,
            "keypad: expression role"
        );
        let seven = keypad
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Calculator key 7"))
            .expect("digit key is actionable");
        let mut doc = AutoCommit::new();
        assert!(
            app.perform_semantic_action(
                seven.action_target.as_deref().unwrap_or_default(),
                &Action::Activate,
                &mut Edits::lend(&mut doc),
                &automerge::ROOT,
            ),
            "keypad: key 7 handled"
        );
        assert_eq!(app.model.expr, "7", "keypad: key 7 appended");
        assert!(
            app.perform_semantic_action(
                expression.action_target.as_deref().unwrap_or_default(),
                &Action::SetValue("sqrt(9)".into()),
                &mut Edits::lend(&mut doc),
                &automerge::ROOT,
            ),
            "keypad: expression SetValue handled"
        );
        assert_eq!(app.model.expr, "sqrt(9)", "keypad: expression set");

        app.set_mode(Mode::Graph2d);
        app.model.funcs = vec![PlotFn {
            expr: String::new(),
            visible: true,
        }];
        app.layout_active();
        let graph = app.accessibility().expect("graph semantics");
        let panel = graph
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Functions"))
            .expect("functions panel is exposed");
        let formula = panel
            .children
            .iter()
            .flat_map(|node| node.children.iter())
            .find(|node| node.role == ui::a11y::Role::TextInput)
            .expect("formula editor is a text input");
        assert!(
            app.perform_semantic_action(
                formula.action_target.as_deref().unwrap_or_default(),
                &Action::SetValue("2*x".into()),
                &mut Edits::lend(&mut doc),
                &automerge::ROOT,
            ),
            "graph: formula SetValue handled"
        );
        assert_eq!(app.model.funcs[0].expr, "2*x", "graph: formula set");
    }
}

impl CalculatorApp {
    /// Screen-space center of the Calc keypad key labelled `label`, after
    /// the current layout. `None` if the key is absent or not laid out.
    fn key_center(&self, label: &str) -> Option<Point> {
        let i = self.calc.key_index_for_label(label)?;
        self.calc.key_center(i)
    }
}

/// The modes, the angle unit and the panel's show/hide button are the
/// window bar's slots, and the screens start at the top of the window:
/// no header row of the calculator's own.
#[test]
fn the_modes_units_and_panel_button_are_the_window_bars_and_no_row_is_drawn() {
    let mut app = CalculatorApp::seeded();
    app.area = Rect::new(0.0, 0.0, 1280.0, 720.0);
    app.set_mode(Mode::Graph2d);
    app.relayout();
    assert!(
        (app.g2.canvas.y0 - app.area.y0).abs() < 0.5,
        "the plot starts at the top: {:?}",
        app.g2.canvas
    );
    app.declare_window_toolbar();
    let toolbar = ui::window_toolbar::declared().expect("the window declares its slots");
    let views = toolbar.views.as_ref().expect("the modes switch in the bar");
    assert_eq!(views.choices.len(), 3);
    assert_eq!(views.chosen, "calculator.mode:1");
    assert_eq!(toolbar.sidebar, Some(true), "the formulas are the sidebar");
    let menu: Vec<&str> = toolbar
        .actions
        .iter()
        .filter(|action| action.menu)
        .map(|action| action.name.as_str())
        .collect();
    assert_eq!(menu, ["Radians", "Degrees"]);

    app.set_mode(Mode::Calc);
    app.relayout();
    app.declare_window_toolbar();
    let toolbar = ui::window_toolbar::declared().expect("the window declares its slots");
    assert_eq!(toolbar.sidebar, None);
    assert!(
        toolbar.actions.iter().all(|action| action.menu),
        "past calculations are the window's History, not a button of the app's own"
    );
}

/// The canvas hint teaches navigation only while the pointer is over a
/// plot the person has not navigated yet; the trace follows the pointer
/// over Graph 2D only.
#[test]
fn navigation_hint_shows_over_an_unnavigated_plot_only() {
    let mut app = wide(CalculatorApp::seeded()); // Graph 2D
    assert!(
        app.plot_pointer().trace.is_none(),
        "a plot nobody points at traces nothing, even at the origin"
    );
    let over_plot = app.g2.canvas.center();
    app.mouse = over_plot;
    app.pointer_seen = true;
    let pointer = app.plot_pointer();
    assert!(pointer.hint.is_some(), "hint over a fresh plot");
    assert_eq!(
        pointer.trace,
        Some(over_plot),
        "Graph 2D traces the pointer"
    );

    app.mouse = Point::new(10.0, app.g2.canvas.y0 + 10.0);
    let pointer = app.plot_pointer();
    assert!(
        pointer.hint.is_none() && pointer.trace.is_none(),
        "no hint over the panel"
    );

    app.mouse = over_plot;
    app.pointer_seen = true;
    app.on_drag(Vec2::new(5.0, 0.0));
    assert!(
        app.plot_pointer().hint.is_none(),
        "no hint once the person has panned"
    );

    let mut app = wide(CalculatorApp::seeded());
    app.set_mode(Mode::Graph3d);
    app.relayout();
    app.mouse = app.g3.canvas.center();
    app.pointer_seen = true;
    let pointer = app.plot_pointer();
    assert!(pointer.hint.is_some(), "hint over a fresh surface plot");
    assert!(pointer.trace.is_none(), "only Graph 2D traces");

    app.set_mode(Mode::Calc);
    app.relayout();
    assert!(app.plot_pointer().hint.is_none(), "no plot in Calculator");
}

/// A scenario stages the view a capture shows: the mode, the expression
/// being typed, and the panel as an open floating drawer when it asks for
/// one. A plain capture shows the lived-in 2D graph.
#[test]
fn scenarios_stage_the_view_they_name() {
    let stage = |name: Option<&str>, w: f64| {
        let mut app = CalculatorApp::seeded();
        app.area = Rect::new(0.0, 0.0, w, 800.0);
        app.stage_scenario(name);
        app
    };
    assert_eq!(stage(None, 1280.0).model.mode, Mode::Graph2d);
    assert_eq!(stage(Some("graph2d"), 1280.0).model.mode, Mode::Graph2d);
    assert_eq!(stage(Some("graph3d"), 1280.0).model.mode, Mode::Graph3d);
    let calc = stage(Some("calculator"), 1280.0);
    assert_eq!(calc.model.mode, Mode::Calc);
    assert_eq!(calc.model.expr, "sin(0.5)+cos(0.5)");
    let error = stage(Some("error"), 1280.0);
    assert!(engine::evaluate(&error.model.expr, &error.model.ctx()).is_err());
    let drawer = stage(Some("narrow-functions"), 480.0);
    assert!(drawer.g2.panel.side().is_overlay() && drawer.g2.panel.side().is_open());
}

/// The real key path (`on_key`): Ctrl+digit switches mode, typing and
/// Backspace edit the active 2D function / 3D surface, and Enter appends and
/// focuses a fresh row. Fresh app + state per phase.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn keyboard_path_switches_mode_and_edits_graph_rows() {
    let none = winit::event::Modifiers::default();

    // phase ctrl_digit_switches_mode
    {
        let mut app = CalculatorApp::new();
        let mut st = ui::State::new();
        let ctrl = winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
        app.on_key(
            &WKey::Character("3".into()),
            ctrl,
            &mut Edits::lend(&mut st.doc),
        );
        assert_eq!(app.model.mode, Mode::Graph3d, "ctrl+3 switches to Graph 3D");
        assert_eq!(app.model.surfaces.len(), 1, "with its starter surface");
    }

    // phase escape_clears_then_leaves_an_empty_calculator_to_the_host
    {
        let mut app = CalculatorApp::new();
        let mut st = ui::State::new();
        let escape = WKey::Named(NamedKey::Escape);
        app.on_key(
            &WKey::Character("7".into()),
            none,
            &mut Edits::lend(&mut st.doc),
        );
        assert!(!app.escape_is_idle(&escape), "Escape clears the expression");
        app.on_key(&escape, none, &mut Edits::lend(&mut st.doc));
        assert!(app.model.expr.is_empty());
        assert!(app.escape_is_idle(&escape), "nothing left for Escape to do");
        assert!(!app.escape_is_idle(&WKey::Named(NamedKey::Backspace)));
    }

    // phase typing_into_graph2d_field_updates_active_func
    {
        let mut app = CalculatorApp::new();
        let mut st = ui::State::new();
        app.set_mode(Mode::Graph2d);
        // start from a clean active row
        app.model.funcs = vec![crate::model::PlotFn {
            expr: String::new(),
            visible: true,
        }];
        app.model.active_fn = 0;
        for ch in ["2", "*", "x"] {
            app.on_key(
                &WKey::Character(ch.into()),
                none,
                &mut Edits::lend(&mut st.doc),
            );
        }
        assert_eq!(app.model.funcs[0].expr, "2*x", "typing into 2D field");
    }

    // phase backspace_in_graph3d_field_edits_surface
    {
        let mut app = CalculatorApp::new();
        let mut st = ui::State::new();
        app.set_mode(Mode::Graph3d);
        app.model.surfaces = vec![PlotFn {
            expr: "x+y".into(),
            visible: true,
        }];
        app.model.active_surface = 0;
        // on_key_graph resyncs the active field to the surface before editing.
        app.on_key(
            &WKey::Named(NamedKey::Backspace),
            none,
            &mut Edits::lend(&mut st.doc),
        );
        assert_eq!(app.model.surfaces[0].expr, "x+", "backspace in 3D field");
    }

    // phase enter_in_graph2d_adds_and_focuses_a_new_function
    {
        // Real key path: after editing a function, Enter appends a fresh empty
        // row and focuses it (the "next expression" gesture).
        let mut app = CalculatorApp::new();
        let mut st = ui::State::new();
        app.set_mode(Mode::Graph2d);
        app.model.funcs = vec![PlotFn {
            expr: String::new(),
            visible: true,
        }];
        app.model.active_fn = 0;
        for ch in ["s", "i", "n", "(", "x", ")"] {
            app.on_key(
                &WKey::Character(ch.into()),
                none,
                &mut Edits::lend(&mut st.doc),
            );
        }
        assert_eq!(app.model.funcs[0].expr, "sin(x)", "enter 2D: typed row");
        app.on_key(
            &WKey::Named(NamedKey::Enter),
            none,
            &mut Edits::lend(&mut st.doc),
        );
        assert_eq!(
            app.model.funcs.len(),
            2,
            "Enter must add a new function row"
        );
        assert_eq!(app.model.active_fn, 1, "the new row must be focused");
        assert!(
            app.model.funcs[1].expr.is_empty(),
            "the new row starts empty"
        );
        assert_eq!(
            app.model.funcs[0].expr, "sin(x)",
            "the edited row is preserved"
        );
    }

    // phase enter_in_graph3d_adds_and_focuses_a_new_surface
    {
        let mut app = CalculatorApp::new();
        let mut st = ui::State::new();
        app.switch_mode(Mode::Graph3d, &mut Edits::lend(&mut st.doc)); // adds one starter surface
        let start = app.model.surfaces.len();
        assert_eq!(start, 1);
        app.on_key(
            &WKey::Named(NamedKey::Enter),
            none,
            &mut Edits::lend(&mut st.doc),
        );
        assert_eq!(
            app.model.surfaces.len(),
            start + 1,
            "Enter must add a new surface row"
        );
        assert_eq!(
            app.model.active_surface,
            app.model.surfaces.len() - 1,
            "the new surface is focused"
        );
        assert!(
            app.model.surfaces.last().unwrap().expr.is_empty(),
            "the new surface starts empty"
        );
    }
}

/// Typing a formula writes the document keystroke by keystroke, and the
/// keystrokes of one row are one undo step; a collaborator's edit to the
/// formula is presented.
#[test]
fn formula_keystrokes_are_one_undo_step_and_peer_edits_are_presented() {
    let mut app = hosted();
    app.invoke(Calculator::ADD_FUNCTION.with("")).unwrap();
    press_bar(&mut app, "calculator.mode:1");
    click(&mut app, "calculator.graph.field:2d:0");
    // Keyboard input goes to the focused calculator: Tab to it.
    for _ in 0..32 {
        if calculator(&mut app).1.state.focused {
            break;
        }
        app.harness().tab(false);
    }
    assert!(
        calculator(&mut app).1.state.focused,
        "Tab reaches the calculator"
    );
    app.harness().type_text("2*x");
    app.settle();
    assert_eq!(
        app.state().funcs.values().next().unwrap().expr,
        "2*x",
        "every keystroke is written"
    );

    // Enter starts the next row: the typed formula is settled.
    app.harness().key("Enter");
    app.settle();
    assert_eq!(app.state().funcs.len(), 2, "Enter adds the next row");
    assert!(app.undo(), "undo the new row");
    assert_eq!(app.state().funcs.len(), 1);
    assert!(app.undo(), "undo the typing");
    assert_eq!(
        app.state().funcs.values().next().unwrap().expr,
        "",
        "one undo takes back the whole typed formula"
    );
    assert!(app.redo());
    assert_eq!(app.state().funcs.values().next().unwrap().expr, "2*x");

    let id = app.state().funcs.ids().next().cloned().unwrap();
    app.merge_peer_edit(|doc| {
        let mut peer = ui::tilo_resource::Replica::<Calculator>::from_doc(doc.clone());
        peer.apply(
            Invoke::edit(&GraphKind::Curve2d.rows().item(&id).expr.replace("x*x+1")).unwrap(),
        )
        .unwrap();
        *doc = peer.doc().clone();
    });
    app.settle();
    assert_eq!(
        node(&mut app, "calculator.graph.field:2d:0")
            .value
            .as_deref(),
        Some("x*x+1"),
        "the collaborator's formula is shown"
    );
}

// -- pointer-path harness: drive the REAL production handle_event route -----
//
// These exercise the live pointer pipeline end-to-end — hit-testing against
// the same rects the last layout painted, then the wired handler — rather
// than calling the model-level `apply_key` / view mutators directly. Node /
// taffy are unused by the (single-leaf) calculator, so a throwaway leaf is
// fine.

fn send(app: &mut CalculatorApp, st: &mut ui::State, ev: &WindowEvent) -> bool {
    ui::testing::Dispatch::new()
        .doc(&mut st.doc)
        .path(&st.root)
        .send(app, ev)
        .handled
}

fn cursor(p: Point) -> WindowEvent {
    ui::synthetic_input::cursor_moved(p.x, p.y)
}
fn btn(state: ElementState) -> WindowEvent {
    ui::synthetic_input::mouse_input(state, ui::winit::event::MouseButton::Left)
}
fn wheel(y: f64) -> WindowEvent {
    ui::synthetic_input::mouse_wheel(0.0, y as f32)
}

/// A full click (move → press → release) at scene point `p`.
fn tap(app: &mut CalculatorApp, st: &mut ui::State, p: Point) {
    send(app, st, &cursor(p));
    send(app, st, &btn(ElementState::Pressed));
    send(app, st, &btn(ElementState::Released));
}

/// A press-drag: press at `a`, move to `b` with the button held, release.
fn drag(app: &mut CalculatorApp, st: &mut ui::State, a: Point, b: Point) {
    send(app, st, &cursor(a));
    send(app, st, &btn(ElementState::Pressed));
    send(app, st, &cursor(b));
    send(app, st, &btn(ElementState::Released));
}

/// An app at the wide 1280×800 area used by the pointer tests, after one
/// real frame: the controls are measured at the size they paint at.
fn wide(mut app: CalculatorApp) -> CalculatorApp {
    app.area = Rect::new(0.0, 0.0, 1280.0, 800.0);
    app.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        &PresenceCtx::empty(),
    );
    app.relayout();
    app
}

/// Calculator-mode pointer path: keycaps append, a history row recalls,
/// Clear empties history, and memory keys accumulate/clear. Fresh app +
/// state per phase.
#[test]
fn calc_pointer_clicks_drive_keypad_history_and_memory() {
    // phase calc_keypad_click_appends_to_expression
    {
        // handle_event → on_press → on_press_calc → key_at → apply_key on the
        // real keycap rects (not a model-level apply_key shortcut).
        let mut app = wide(CalculatorApp::new()); // Calc mode, empty
        let mut st = ui::State::new();
        let seven = app.key_center("7").expect("7 keycap laid out");
        tap(&mut app, &mut st, seven);
        assert_eq!(app.model.expr, "7", "tapping a digit keycap must append it");
        let plus = app.key_center("+").expect("+ keycap laid out");
        tap(&mut app, &mut st, plus);
        assert_eq!(
            app.model.expr, "7+",
            "tapping an operator keycap must append it"
        );
    }

    // phase memory_button_click_accumulates_via_pointer
    {
        // on_press_calc → mem_at → apply_key(MemAdd/MemClear) via the mem rects.
        let mut app = CalculatorApp::new();
        let mut st = ui::State::new();
        app.model.expr = "10".into();
        let mut app = wide(app);
        // MEM_KEYS = [MC, MR, M+, M-]; index 2 is M+, index 0 is MC.
        let m_add = app.calc.mem_center(2).expect("M+ button laid out");
        tap(&mut app, &mut st, m_add);
        assert_eq!(
            app.model.memory, 10.0,
            "M+ must add the current value to memory"
        );
        tap(&mut app, &mut st, m_add);
        assert_eq!(app.model.memory, 20.0, "M+ again accumulates");
        let m_clr = app.calc.mem_center(0).expect("MC button laid out");
        tap(&mut app, &mut st, m_clr);
        assert_eq!(app.model.memory, 0.0, "MC must clear memory");
    }
}

/// The window bar's Degrees and Radians switch the angle unit, and trig
/// evaluates in the unit chosen.
#[test]
fn the_bars_angle_unit_changes_trig_output() {
    {
        // The bar's Degrees and Radians → model.deg, and the change must
        // flow through model.ctx() so trig actually evaluates
        // differently.
        use crate::engine;
        let mut app = CalculatorApp::new();
        let mut st = ui::State::new();
        app.area = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        assert!(!app.model.deg, "default is radians");
        let rad_val = engine::evaluate("sin(90)", &app.model.ctx()).unwrap();
        assert!(
            (rad_val - 1.0).abs() > 0.01,
            "RAD sin(90) must not be 1 (got {rad_val})"
        );
        let mut press = |app: &mut CalculatorApp, id: &str| {
            ui::window_toolbar::deliver(ui::window_toolbar::ToolbarEvent::Action { id: id.into() });
            ui::widget::Widget::settle(app, &mut ui::widget::Edits::lend(&mut st.doc), &st.root);
        };
        press(&mut app, "calculator.angle:degrees");
        assert!(app.model.deg, "choosing Degrees must switch to degrees");
        let deg_val = engine::evaluate("sin(90)", &app.model.ctx()).unwrap();
        assert!(
            (deg_val - 1.0).abs() < 1e-9,
            "DEG sin(90) must be 1 (got {deg_val})"
        );
        press(&mut app, "calculator.angle:radians");
        assert!(
            !app.model.deg,
            "choosing Radians must switch back to radians"
        );
    }
}

/// Graph-mode pointer path: drag pans / orbits and the wheel zooms the 2D and
/// 3D views, the narrow Functions drawer opens from its toggle, and the row
/// visibility toggle and delete button act on the tapped row. Fresh app + state
/// per phase.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn graph_pointer_path_drives_views_drawer_and_row_controls() {
    // phase graph2d_drag_pans_the_view
    {
        // handle_event drag → on_drag → view2d center shifts.
        let mut app = CalculatorApp::new();
        app.set_mode(Mode::Graph2d);
        let mut st = ui::State::new();
        let mut app = wide(app); // docked panel → live canvas
        assert_eq!(app.model.view2d.cx, 0.0, "pan 2D: starts at cx 0");
        assert_eq!(app.model.view2d.cy, 0.0, "pan 2D: starts at cy 0");
        let c = app.g2.canvas.center();
        drag(&mut app, &mut st, c, Point::new(c.x + 40.0, c.y + 30.0));
        assert!(app.model.view2d.cx != 0.0, "horizontal drag must pan cx");
        assert!(app.model.view2d.cy != 0.0, "vertical drag must pan cy");
    }

    // phase graph2d_wheel_zooms_the_view
    {
        // handle_event wheel over the canvas → on_wheel → view2d.scale grows.
        let mut app = CalculatorApp::new();
        app.set_mode(Mode::Graph2d);
        let mut st = ui::State::new();
        let mut app = wide(app);
        let before = app.model.view2d.scale;
        let c = app.g2.canvas.center();
        send(&mut app, &mut st, &cursor(c));
        send(&mut app, &mut st, &wheel(1.0));
        assert!(
            app.model.view2d.scale > before,
            "scrolling up over the canvas must zoom in (scale {before} → {})",
            app.model.view2d.scale
        );
    }

    // phase graph3d_drag_orbits_the_view
    {
        // handle_event drag → on_drag → view3d yaw/pitch change.
        let mut app = CalculatorApp::new();
        app.set_mode(Mode::Graph3d);
        let mut st = ui::State::new();
        let mut app = wide(app);
        let (yaw0, pitch0) = (app.model.view3d.yaw, app.model.view3d.pitch);
        let c = app.g3.canvas.center();
        drag(&mut app, &mut st, c, Point::new(c.x + 50.0, c.y + 20.0));
        assert!(
            (app.model.view3d.yaw - yaw0).abs() >= f64::EPSILON,
            "horizontal drag must change yaw"
        );
        assert!(
            (app.model.view3d.pitch - pitch0).abs() >= f64::EPSILON,
            "vertical drag must change pitch"
        );
    }

    // phase graph3d_wheel_zooms_the_view
    {
        // handle_event wheel over the surface canvas → on_wheel → view3d.zoom.
        let mut app = CalculatorApp::new();
        app.set_mode(Mode::Graph3d);
        let mut st = ui::State::new();
        let mut app = wide(app);
        let before = app.model.view3d.zoom;
        let c = app.g3.canvas.center();
        send(&mut app, &mut st, &cursor(c));
        send(&mut app, &mut st, &wheel(1.0));
        assert!(
            app.model.view3d.zoom > before,
            "scrolling up over the surface must zoom in"
        );
    }

    // phase narrow_graph2d_bar_toggle_opens_the_drawer
    {
        let mut app = CalculatorApp::new();
        app.set_mode(Mode::Graph2d);
        app.area = Rect::new(0.0, 0.0, 560.0, 720.0);
        app.relayout();
        assert!(
            !app.g2.panel.side().is_open(),
            "narrow default must start collapsed"
        );
        let mut st = ui::State::new();
        ui::window_toolbar::deliver(ui::window_toolbar::ToolbarEvent::ToggleSidebar);
        ui::widget::Widget::settle(&mut app, &mut Edits::lend(&mut st.doc), &st.root);
        assert!(
            app.g2.panel.side().is_open(),
            "the bar's toggle must open the Functions drawer"
        );
    }

    // phase vis_dot_click_toggles_function_visibility
    {
        // on_press_graph → vis_at → funcs[i].visible flips, via the real toggle rect.
        let mut app = CalculatorApp::seeded(); // Graph2d, 4 visible funcs
        app.set_mode(Mode::Graph2d);
        let mut st = ui::State::new();
        let mut app = wide(app); // docked panel
        assert!(app.model.funcs[0].visible, "vis toggle: starts visible");
        let dot = app
            .g2
            .panel
            .vis_center(0)
            .expect("row 0 visibility toggle laid out");
        tap(&mut app, &mut st, dot);
        assert!(
            !app.model.funcs[0].visible,
            "tapping the toggle must hide the function"
        );
        tap(&mut app, &mut st, dot);
        assert!(app.model.funcs[0].visible, "tapping again must show it");
    }

    // phase graph_row_delete_button_tombstones_the_stable_resource
    {
        // The visible delete button uses the same keyed action as the
        // palette, so it cannot rebind after a peer changes list order.
        let mut app = TestApp::<CalculatorShell>::capture(None).unwrap();
        let before = app.state().funcs;
        let delete = node(&mut app, "calculator.graph.delete:2d:0")
            .bounds
            .expect("first formula has a visible delete control");
        app.harness().click_at(
            f64::midpoint(delete.x0, delete.x1),
            f64::midpoint(delete.y0, delete.y1),
        );
        app.settle();
        // Undo takes a removed formula back, so nobody is asked.
        assert!(app.asking().is_none(), "nothing asks first");
        let after = app.state().funcs;
        assert_eq!(after.len(), before.len() - 1, "one row removed");
        assert!(
            after.ids().all(|id| Some(id) != before.ids().next()),
            "the clicked stable row is tombstoned rather than a neighbouring index"
        );
    }
}
