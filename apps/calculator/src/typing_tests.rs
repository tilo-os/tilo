// Typing reaches the calculator through the framework's live input path:
// included into `app::tests`.

// USER REQUIREMENT (2026-10-22): "typing in the 2D graph mode enters the function"
/// A person who switches to Graph 2D and types (no Tab first) writes the
/// function they type, on the framework's live path: the mode press, the
/// press on the formula and the keystrokes all go through the host.
#[test]
fn typing_in_the_2d_graph_mode_enters_the_function() {
    // phase typing_after_switching_to_graph_2d
    {
        let mut app = hosted();
        click(&mut app, "calculator.mode:1");
        assert!(node(&mut app, "calculator.mode:1").state.selected);
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
        click(&mut app, "calculator.mode:1");
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
    // phase a_press_in_calculator_mode_takes_typing_too
    {
        let mut app = hosted();
        click(&mut app, "calculator.mode:0");
        app.harness().type_text("1+2");
        app.settle();
        assert_eq!(calculator(&mut app).1.value.as_deref(), Some("1+2"));
    }
}
