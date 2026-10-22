// The window's History is the calculator's list of past calculations:
// included into `app::tests`.

/// Every row History lists, as assistive technology reads it: its target
/// and its name, with its time groups opened.
fn history_rows(app: &mut TestApp<CalculatorShell>) -> Vec<(String, String)> {
    fn timeline(node: &ui::headless::InspectionNode) -> Option<&ui::headless::InspectionNode> {
        if node.test_id.as_deref() == Some("activity_timeline") {
            return Some(node);
        }
        node.children.iter().find_map(timeline)
    }
    fn rows(node: &ui::a11y::AccessNode, out: &mut Vec<(String, String)>) {
        if let Some(target) = &node.action_target {
            out.push((target.clone(), node.name.clone().unwrap_or_default()));
        }
        for child in &node.children {
            rows(child, out);
        }
    }
    let mut listed = Vec::new();
    for _ in 0..4 {
        let inspection = app.harness().inspection();
        listed.clear();
        if let Some(semantics) = timeline(&inspection.root).and_then(|node| node.semantics.as_ref())
        {
            rows(semantics, &mut listed);
        }
        let groups: Vec<String> = listed
            .iter()
            .filter(|(target, _)| target.starts_with("timeline-group:"))
            .map(|(target, _)| target.clone())
            .collect();
        for group in groups {
            // An open group refuses Expand; only closed ones open.
            let _ = app.harness().perform_semantic_named(
                "activity_timeline",
                Some(&group),
                &ui::a11y::Action::Expand,
            );
        }
        app.harness().relayout();
    }
    listed
        .into_iter()
        .filter(|(target, _)| target.starts_with("timeline-entry:"))
        .collect()
}

/// Type `expression` and press Enter in calculator mode.
fn calculate(app: &mut TestApp<CalculatorShell>, expression: &str) {
    app.harness().type_text(expression);
    app.harness().key("Enter");
    app.settle();
}

/// Past calculations are History's rows: each reads as the calculation and
/// its result, a graph change reads as the function it plotted, and choosing
/// a calculation closes History and puts it back into the calculator. The
/// calculator itself lists none of them.
#[test]
fn history_lists_calculations_and_choosing_one_recalls_it() {
    fn all(node: &ui::a11y::AccessNode, out: &mut Vec<String>) {
        out.extend(node.action_target.clone());
        for child in &node.children {
            all(child, out);
        }
    }
    let mut app = hosted();
    press_bar(&mut app, "calculator.mode:0");
    calculate(&mut app, "1+2");
    calculate(&mut app, "3*4");
    press_bar(&mut app, "calculator.mode:1");
    click(&mut app, "calculator.graph.field:2d:0");
    app.harness().type_text("x+1");
    app.settle();
    press_bar(&mut app, "calculator.mode:0");
    app.harness().type_text("9");
    app.settle();
    app.flush().unwrap();

    let mut targets = Vec::new();
    all(&calculator(&mut app).1, &mut targets);
    assert!(
        !targets
            .iter()
            .any(|target| target.starts_with("calculator.history")),
        "the calculator keeps no list beside History: {targets:?}"
    );

    app.harness()
        .open_history()
        .expect("the window's History opens");
    app.settle();
    app.harness().relayout();
    let rows = history_rows(&mut app);
    let mut addressable: Vec<String> = app.harness().targets().into_keys().collect();
    addressable.sort();
    let named = |text: &str| {
        rows.iter()
            .find(|(_, name)| name.ends_with(text))
            .map(|(target, _)| target.clone())
    };
    assert!(
        named("3*4 = 12").is_some(),
        "{rows:?} among {addressable:?}"
    );
    assert!(named("y = x+1").is_some(), "{rows:?}");
    let first = named("1+2 = 3").unwrap_or_else(|| panic!("{rows:?}"));

    app.harness()
        .perform_semantic_named(
            "activity_timeline",
            Some(&first),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    app.settle();
    app.harness().relayout();
    assert!(
        history_rows(&mut app).is_empty(),
        "choosing a calculation closes History"
    );
    assert_eq!(
        ui::window_toolbar::declared()
            .and_then(|toolbar| toolbar.views)
            .map(|views| views.chosen),
        Some("calculator.mode:0".to_owned()),
        "back in calculator mode"
    );
    assert_eq!(
        calculator(&mut app).1.value.as_deref(),
        Some("1+2"),
        "the chosen calculation is back in the calculator"
    );
}

/// The calculations are the calculator's library, one record per workspace:
/// a second Calculator window lists the first window's calculations in its
/// History. What a window is doing (the expression being typed, the mode,
/// a graph's view) stays that window's own.
#[test]
fn a_second_window_lists_the_first_windows_calculations() {
    let mut app = hosted();
    press_bar(&mut app, "calculator.mode:0");
    calculate(&mut app, "6*7");
    app.harness().type_text("5");
    app.settle();
    app.flush().unwrap();

    let mut beside = app.another_window().expect("a second window");
    beside.harness().resize((1280, 800)).unwrap();
    assert_ne!(
        calculator(&mut beside).1.value.as_deref(),
        Some("5"),
        "the expression being typed is the first window's own"
    );
    beside
        .harness()
        .open_history()
        .expect("the window's History opens");
    let rows = history_rows(&mut beside);
    assert!(
        rows.iter().any(|(_, name)| name.ends_with("6*7 = 42")),
        "the second window lists the first window's calculation: {rows:?}"
    );
}

/// The window bar's History button is the main way into the calculator's
/// past calculations: the window offers it, and a press on it opens the
/// window's History, listing what was done.
#[test]
fn the_history_button_opens_history() {
    let mut app = hosted();
    press_bar(&mut app, "calculator.mode:0");
    calculate(&mut app, "1+2");
    app.flush().unwrap();

    app.harness().present_in_window_bar();
    assert_eq!(
        app.harness().window_history_control(),
        Some(ui::widget::HistoryControl::of(false)),
        "the window bar offers History"
    );
    app.harness()
        .press_window_history()
        .expect("the window bar's History button opens History");
    app.settle();
    let rows = history_rows(&mut app);
    assert!(
        rows.iter().any(|(_, name)| name.ends_with("1+2 = 3")),
        "pressing History opens it: {rows:?}"
    );
    assert!(
        !app.harness().targets().contains_key("action-palette"),
        "the press is History's, never an app action the palette refuses"
    );
}

/// A formula is kept whole in its change, however long: History names the
/// whole function, and only its row elides it to fit.
#[test]
fn a_long_formula_reaches_history_whole() {
    let formula = ["x"; 60].join("+");
    assert!(formula.chars().count() > 100);
    let mut app = hosted();
    press_bar(&mut app, "calculator.mode:1");
    click(&mut app, "calculator.graph.field:2d:0");
    app.harness().type_text(&formula);
    app.settle();
    app.flush().unwrap();
    app.harness()
        .open_history()
        .expect("the window's History opens");
    let rows = history_rows(&mut app);
    assert!(
        rows.iter()
            .any(|(_, name)| name.ends_with(&format!("y = {formula}"))),
        "the whole formula: {rows:?}"
    );
}
