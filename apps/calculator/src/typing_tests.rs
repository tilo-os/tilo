// Typing reaches the calculator through the framework's live input path:
// included into `app::tests`.

/// A person who switches to Graph 2D and types (no Tab first) writes the
/// function they type, on the framework's live path: the mode press in the
/// window's bar, the press on the formula and the keystrokes all go
/// through the host.
#[test]
fn typing_in_the_2d_graph_mode_enters_the_function() {
    // phase typing_after_switching_to_graph_2d
    {
        let mut app = hosted();
        press_bar(&mut app, "calculator.mode:1");
        assert_eq!(
            ui::window_toolbar::declared()
                .and_then(|toolbar| toolbar.views)
                .map(|views| views.chosen),
            Some("calculator.mode:1".to_owned())
        );
        app.harness().type_text("x+1");
        app.settle();
        assert_eq!(
            app.state()
                .funcs
                .values()
                .next()
                .map(|row| row.expr.clone()),
            Some("x+1".to_owned()),
            "typing in the 2D graph mode enters the function"
        );
    }
    // phase typing_after_pressing_the_formula
    {
        let mut app = hosted();
        press_bar(&mut app, "calculator.mode:1");
        click(&mut app, "calculator.graph.field:2d:0");
        app.harness().type_text("2*x");
        app.settle();
        assert_eq!(
            app.state()
                .funcs
                .values()
                .next()
                .map(|row| row.expr.clone()),
            Some("2*x".to_owned()),
            "typing in the 2D graph mode enters the function"
        );
        assert_eq!(
            node(&mut app, "calculator.graph.field:2d:0")
                .value
                .as_deref(),
            Some("2*x"),
            "the formula shows what was typed"
        );
    }
    // phase typing_after_switching_to_graph_3d
    {
        let mut app = hosted();
        press_bar(&mut app, "calculator.mode:2");
        let surface = |app: &mut TestApp<CalculatorShell>| {
            app.state()
                .surfaces
                .values()
                .next()
                .map(|row| row.expr.clone())
        };
        let starter = surface(&mut app).expect("Graph 3D starts with a surface");
        app.harness().type_text("+1");
        app.settle();
        let typed = surface(&mut app).expect("the surface stays");
        // The field writes its formula in its own spelling (`x^(2)` for
        // `x^2`): the same function, one higher everywhere.
        let (before, after) = (
            engine::compile(&starter, false, true).expect("the starter plots"),
            engine::compile(&typed, false, true).expect("what was typed plots"),
        );
        assert!(
            typed.ends_with("+1")
                && [(0.0, 0.0), (1.0, -2.0), (0.5, 3.0)]
                    .into_iter()
                    .all(|(x, y)| (after.eval2(x, y) - before.eval2(x, y) - 1.0).abs() < 1e-9),
            "typing in the 3D graph mode enters the surface: {starter:?} became {typed:?}"
        );
    }
    // phase typing_as_soon_as_the_window_opens
    {
        let mut app = hosted();
        app.harness().send(&WindowEvent::Focused(true));
        app.harness().type_text("6*7");
        app.settle();
        assert_eq!(calculator(&mut app).1.value.as_deref(), Some("6*7"));
    }
    // phase switching_to_calculator_mode_takes_typing_too
    {
        let mut app = hosted();
        press_bar(&mut app, "calculator.mode:0");
        app.harness().type_text("1+2");
        app.settle();
        assert_eq!(calculator(&mut app).1.value.as_deref(), Some("1+2"));
    }
}
