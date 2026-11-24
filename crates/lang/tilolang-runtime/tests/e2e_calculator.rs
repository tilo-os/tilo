//! The calculator app (`apps/tl/calculator/app.tl`) end to end: its arithmetic and scientific
//! functions through its actions, and its calculator, conversion and graph windows through its
//! views.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, UiNode, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/calculator/app.tl");

    struct Calc {
        rt: Runtime,
        app: App,
        root: ResourceRef,
    }

    impl Calc {
        fn new() -> Self {
            let app = App::compile(SOURCE, &[]);
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            authority.trust(&app.load(&rt));
            // A press of the window's copy button reaches the person's selection, which the
            // policy allows for the app it is written in.
            authority.grant_outside(&app.action("copy_text"));
            let root = rt.create_root("Calculator").unwrap();
            Self { rt, app, root }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        /// Works `expr` out and answers what it came to, as the record keeps it, or why the
        /// calculator refused it.
        fn calculate(&self, expr: &str) -> Result<String, String> {
            self.work_out(expr, false)
        }

        /// The same, in degrees where `degrees`.
        fn work_out(&self, expr: &str, degrees: bool) -> Result<String, String> {
            let done = self.rt.invoke_action(
                &me(),
                (
                    self.app.action("calculate").as_str(),
                    vec![
                        Value::resource(&self.root),
                        Value::text(expr),
                        Value::Bool(degrees),
                    ],
                ),
            );
            let done = match done {
                Ok(done) => done,
                Err(refused) => return Err(refused.to_string()),
            };
            match &done {
                InvokeOutcome::Done(Value::Ref(_)) => {}
                other => return Err(format!("{other:?}")),
            }
            let kept = returned_ref(done);
            match self.rt.read(&kept, "result").unwrap() {
                Value::Text(said) => Ok(said),
                other => Err(format!("result reads {other:?}")),
            }
        }

        /// Works `expr` out and answers the calculation it kept.
        fn work_out_ref(&self, expr: &str) -> ResourceRef {
            let done = self.call(
                "calculate",
                vec![Value::resource(&self.root), Value::text(expr)],
            );
            match &done {
                InvokeOutcome::Done(Value::Ref(_)) => returned_ref(done),
                other => panic!("{expr}: {other:?}"),
            }
        }

        fn worked_out(&self, expr: &str) -> String {
            self.calculate(expr)
                .unwrap_or_else(|refused| panic!("{expr}: refused: {refused}"))
        }

        fn add_curve(&self, formula: &str) -> ResourceRef {
            returned_ref(self.call(
                "add_curve",
                vec![Value::resource(&self.root), Value::text(formula)],
            ))
        }

        fn add_surface(&self, formula: &str) -> ResourceRef {
            returned_ref(self.call(
                "add_surface",
                vec![Value::resource(&self.root), Value::text(formula)],
            ))
        }

        /// The app's mode, as a person picks it from the window's modes.
        fn choose_mode(&self, mode: i64) {
            let mut view = self.window();
            let tabs = of_kind(view.tree(), "ui.ViewTabs")[0].key.clone();
            view.fire_with(&tabs, "change", Some(&Value::Int(mode)))
                .unwrap();
            view.poll_patches().unwrap();
        }

        /// The quantity a person picks from the conversion window's segments.
        fn press_segment(&self, label: &str) {
            let mut view = self.window();
            let segment = of_kind(view.tree(), "ui.Segment")
                .into_iter()
                .find(|s| s.props.get("label").and_then(Value::as_text) == Some(label))
                .unwrap_or_else(|| panic!("no `{label}` segment:\n{}", dump(view.tree())))
                .key
                .clone();
            view.fire(&segment, "press").unwrap();
            view.poll_patches().unwrap();
        }

        /// The calculator's window, every row of its lists built.
        fn window(&self) -> ViewInstance {
            self.views("calculator", Value::resource(&self.root))
        }

        /// A past calculation opened.
        fn opened(&self, calculation: &ResourceRef) -> ViewInstance {
            self.views("calculation", Value::resource(calculation))
        }

        fn views(&self, view: &str, on: Value) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(&me(), &self.app.view(view), vec![on])
                .unwrap_or_else(|e| panic!("{view}: {e}"));
            window.set_lazy_rows(false).unwrap();
            window
        }
    }

    /// Every button of the window, by the words it shows.
    fn buttons(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.Button")
            .iter()
            .filter_map(|b| b.props.get("label").and_then(Value::as_text))
            .map(str::to_owned)
            .collect()
    }

    /// Every text of the window.
    fn texts(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.Text")
            .iter()
            .filter_map(|t| t.props.get("value").and_then(Value::as_text))
            .map(str::to_owned)
            .collect()
    }

    /// The shapes of the window's drawing.
    fn drawn(tree: &UiNode) -> Vec<&UiNode> {
        of_kind(tree, "ui.Drawing")
    }

    /// The cells of the window's surfaces: one shape gathers them all, nine numbers each, so
    /// the floor's lines beside it are no cells.
    fn mesh_cells(drawing: &UiNode) -> usize {
        match drawing.props.get("shapes") {
            Some(Value::List(shapes)) => shapes
                .iter()
                .filter_map(|shape| match shape {
                    Value::Enum(case) if case.variant == "mesh" => match case.fields.get("cells") {
                        Some(Value::List(cells)) => Some(cells.len() / 9),
                        _ => None,
                    },
                    _ => None,
                })
                .sum(),
            other => panic!("a drawing holds shapes, not {other:?}"),
        }
    }

    /// The texts a drawing sets in its own shapes: the graph's corner numbers.
    fn shape_texts(drawing: &UiNode) -> Vec<String> {
        match drawing.props.get("shapes") {
            Some(Value::List(shapes)) => shapes
                .iter()
                .filter_map(|shape| match shape {
                    Value::Enum(case) if case.variant == "text" => case
                        .fields
                        .get("text")
                        .and_then(Value::as_text)
                        .map(str::to_owned),
                    _ => None,
                })
                .collect(),
            other => panic!("a drawing holds shapes, not {other:?}"),
        }
    }

    fn shape_count(drawing: &UiNode) -> usize {
        match drawing.props.get("shapes") {
            Some(Value::List(shapes)) => shapes.len(),
            other => panic!("a drawing holds shapes, not {other:?}"),
        }
    }

    tilo_test_env::scenario! {
        fn the_calculator_works_out_values_and_draws_its_windows;

        phase arithmetic_keeps_its_order;
        {
            let c = Calc::new();
            assert_eq!(c.worked_out("12*(4+3)"), "84");
            assert_eq!(c.worked_out("10-3-2"), "5", "left to right");
            assert_eq!(c.worked_out("2+3*4"), "14");
            assert_eq!(c.worked_out("(2+3)*4"), "20");
            assert_eq!(c.worked_out("-2^2"), "-4", "a power before a leading minus");
            assert_eq!(c.worked_out("2^3^2"), "512", "a power from the right");
            assert_eq!(c.worked_out("2pi"), "6.2831853072", "implicit multiplication");
            assert_eq!(c.worked_out("50%"), "0.5");
            assert_eq!(c.worked_out("5!"), "120");
            assert_eq!(c.worked_out("1/4"), "0.25");
            assert_eq!(c.worked_out("0.1+0.2"), "0.3", "ten places, no float noise");
        }

        phase scientific_functions_answer_in_radians;
        {
            let c = Calc::new();
            assert_eq!(c.worked_out("sqrt(2)"), "1.4142135624");
            assert_eq!(c.worked_out("cbrt(27)"), "3");
            assert_eq!(c.worked_out("abs(-3)"), "3");
            assert_eq!(c.worked_out("floor(2.9)"), "2");
            assert_eq!(c.worked_out("ceil(2.1)"), "3");
            assert_eq!(c.worked_out("round(2.5)"), "3");
            assert_eq!(c.worked_out("trunc(-2.7)"), "-2");
            assert_eq!(c.worked_out("sign(-9)"), "-1");
            assert_eq!(c.worked_out("ln(e)"), "1");
            assert_eq!(c.worked_out("log10(1000)"), "3");
            assert_eq!(c.worked_out("log2(8)"), "3");
            assert_eq!(c.worked_out("log(2,1024)"), "10");
            assert_eq!(c.worked_out("exp(0)"), "1");
            assert_eq!(c.worked_out("sin(pi/6)"), "0.5");
            assert_eq!(c.worked_out("cos(0)"), "1");
            assert_eq!(c.worked_out("tan(0)"), "0");
            assert_eq!(c.worked_out("asin(1)"), "1.5707963268");
            assert_eq!(c.worked_out("sinh(0)"), "0");
            assert_eq!(c.worked_out("cosh(0)"), "1");
            assert_eq!(c.worked_out("tanh(0)"), "0");
            assert_eq!(c.worked_out("gamma(5)"), "24");
            assert_eq!(c.worked_out("fact(5)"), "120");
            assert_eq!(c.worked_out("pow(2,10)"), "1024");
            assert_eq!(c.worked_out("root(3,27)"), "3");
            assert_eq!(c.worked_out("hypot(3,4)"), "5");
            assert_eq!(c.worked_out("mod(7,3)"), "1");
            assert_eq!(c.worked_out("mod(-1,3)"), "2", "a remainder is never negative");
            assert_eq!(c.worked_out("min(3,8)"), "3");
            assert_eq!(c.worked_out("max(3,8)"), "8");
            assert_eq!(c.worked_out("min(3,8,5)"), "3", "a minimum takes any count");
            assert_eq!(c.worked_out("max(3,8,5)"), "8");
            assert_eq!(c.worked_out("asinh(0)"), "0");
            assert_eq!(c.worked_out("acosh(1)"), "0");
            assert_eq!(c.worked_out("atanh(0)"), "0");
            assert_eq!(c.worked_out("rad(180)"), "3.1415926536");
            assert_eq!(c.worked_out("deg(pi)"), "180");
            assert_eq!(c.worked_out("atan2(1,1)"), "0.7853981634");
        }

        phase degrees_work_the_trigonometric_functions_in_degrees;
        {
            let c = Calc::new();
            assert_eq!(c.work_out("sin(30)", true).unwrap(), "0.5");
            assert_eq!(c.work_out("cos(60)", true).unwrap(), "0.5");
            assert_eq!(c.work_out("asin(0.5)", true).unwrap(), "30");
            assert_eq!(c.work_out("sin(pi/6)", false).unwrap(), "0.5");
        }

        phase a_definite_integral_binds_the_expression_it_integrates;
        {
            let c = Calc::new();
            assert_eq!(c.worked_out("int(x^2,0,3)"), "9");
            assert_eq!(c.worked_out("int(sin(x),0,pi)"), "2");
            assert_eq!(c.worked_out("int(x^2,x,0,3)"), "9", "the variable may be named");
            assert_eq!(c.worked_out("int(y^2,y,0,3)"), "9", "either of the graphs' variables");
        }

        phase an_expression_with_no_value_is_refused_and_not_recorded;
        {
            let c = Calc::new();
            assert!(c.calculate("sqrt(").is_err());
            assert!(c.calculate("2+").is_err());
            assert!(c.calculate("").is_err());
            let view = c.window();
            assert!(
                of_kind(view.tree(), "ui.ListRow").is_empty(),
                "nothing is recorded:\n{}",
                dump(view.tree())
            );
        }

        phase the_window_draws_the_display_and_the_scientific_keypad;
        {
            let c = Calc::new();
            let view = c.window();
            let keys = buttons(view.tree());
            for key in ["7", "=", "sin", "cos", "tan", "ln", "log", "√", "^", "x²", "1/x", "!", "%", "π", "e", "AC", "M+", "ans", "00", "∫", "Γ", "ⁿ√", "eˣ", "sinh", "cosh", "tanh", "|x|", "mod"] {
                assert!(keys.iter().any(|k| k.as_str() == key), "{key} is offered: {keys:?}");
            }
            let fields = of_kind(view.tree(), "ui.TextInput");
            assert!(fields.iter().any(|f| f.props.get("placeholder").and_then(Value::as_text) == Some("Type a calculation")));
            assert!(fields.iter().any(|f| f.props.get("placeholder").and_then(Value::as_text) == Some("Result")));
        }

        phase the_window_offers_every_mode;
        {
            let c = Calc::new();
            let view = c.window();
            let segments = of_kind(view.tree(), "ui.Segment")
                .iter()
                .filter_map(|s| s.props.get("label").and_then(Value::as_text))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            for mode in ["Calculator", "Convert", "2D graph", "3D graph"] {
                assert!(segments.iter().any(|m| m.as_str() == mode), "{mode} is offered: {segments:?}");
            }
        }

        phase a_past_calculation_opens_with_its_expression;
        {
            let c = Calc::new();
            let kept = c.work_out_ref("12*(4+3)");
            let opened = c.opened(&kept);
            // The display works the expression out again on the opened window.
            let fields = of_kind(opened.tree(), "ui.TextInput");
            assert!(
                fields.iter().any(|f| f.props.get("text").and_then(Value::as_text) == Some("84")),
                "the calculation stands on the display:\n{}",
                dump(opened.tree())
            );
        }

        phase copying_a_value_reaches_the_clipboard;
        {
            use std::cell::RefCell;
            use std::rc::Rc;

            /// The media type and bytes of what the person copied.
            type Copied = Option<(String, Vec<u8>)>;

            /// The person's selection, as the device answers it: what a copy leaves there.
            #[derive(Clone, Default)]
            struct Selection(Rc<RefCell<Copied>>);

            impl tilolang_runtime::clipboard::ClipboardHost for Selection {
                fn copy(&self, mime: &str, bytes: &[u8]) -> bool {
                    *self.0.borrow_mut() = Some((mime.to_owned(), bytes.to_vec()));
                    true
                }

                fn paste(&self, _types: &[&str]) -> Option<(String, Vec<u8>)> {
                    self.0.borrow().clone()
                }
            }

            let c = Calc::new();
            let selection = Selection::default();
            c.rt.set_clipboard(selection.clone());
            c.worked_out("12*(4+3)");
            let copied = c.call("copy_text", vec![Value::text("84")]);
            assert!(matches!(copied, InvokeOutcome::Done(_)), "{copied:?}");
            let held = selection.0.borrow().clone();
            assert_eq!(
                held,
                Some(("text/plain".to_owned(), b"84".to_vec())),
                "the result is on the selection"
            );
        }

        phase a_past_calculation_presses_back_into_the_editor;
        {
            let c = Calc::new();
            let kept = c.work_out_ref("12*(4+3)");
            c.worked_out("2+2");
            let mut view = c.window();
            let list = of_kind(view.tree(), "ui.VirtualList")
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("no history list:\n{}", dump(view.tree())))
                .key
                .clone();
            // Pressing the row puts its expression back on the display, in the same window.
            assert!(matches!(
                view.fire_with(&list, "activate", Some(&Value::resource(&kept))).unwrap(),
                InvokeOutcome::Done(_)
            ));
            // The window draws again, as the host's next turn draws it.
            view.poll_patches().unwrap();
            let fields = of_kind(view.tree(), "ui.TextInput");
            assert!(
                fields.iter().any(|f| f.props.get("text").and_then(Value::as_text) == Some("84")),
                "the recalled expression is worked out on the display:\n{}",
                dump(view.tree())
            );
        }

        phase the_calculations_are_listed_newest_first;
        {
            let c = Calc::new();
            c.worked_out("1+1");
            c.worked_out("2+2");
            c.worked_out("3+3");
            let view = c.window();
            let rows = of_kind(view.tree(), "ui.ListRow")
                .iter()
                .filter_map(|r| r.props.get("title").and_then(Value::as_text))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            assert_eq!(rows, ["3+3", "2+2", "1+1"], "newest first");
        }

        phase conversion_turns_one_unit_into_another;
        {
            let c = Calc::new();
            // A person chooses Convert from the window's modes.
            let mut view = c.window();
            let tabs = of_kind(view.tree(), "ui.ViewTabs")[0].key.clone();
            view.fire_with(&tabs, "change", Some(&Value::Int(1))).unwrap();
            view.poll_patches().unwrap();
            let view = c.window();
            // Each quantity starts at a pair of its own: a metre to feet, and then mass's own.
            assert!(
                texts(view.tree()).iter().any(|t| t.as_str() == "1 m = 3.280839895 ft"),
                "the conversion reads:\n{}",
                dump(view.tree())
            );
            assert_eq!(of_kind(view.tree(), "ui.Dropdown").len(), 2, "from and to");
            // And chooses the quantity from the window's own segments.
            c.press_segment("Mass");
            let mass = c.window();
            assert!(
                texts(mass.tree()).iter().any(|t| t.as_str() == "1 kg = 2.2046226218 lb"),
                "mass converts kilograms to pounds:\n{}",
                dump(mass.tree())
            );
            c.press_segment("Temperature");
            let warm = c.window();
            assert!(
                texts(warm.tree()).iter().any(|t| t.as_str() == "1 °C = 33.8 °F"),
                "temperature's units do not share a zero:\n{}",
                dump(warm.tree())
            );
            // Money is refused in words, not converted from a rate nobody asked.
            c.press_segment("Currency");
            let money = c.window();
            assert!(
                texts(money.tree())
                    .iter()
                    .any(|t| t.starts_with("A rate has to come from somewhere")),
                "money says why it is not converted:\n{}",
                dump(money.tree())
            );
        }

        phase curves_are_added_and_deleted_and_the_graph_is_drawn;
        {
            let c = Calc::new();
            let curve = c.add_curve("sin(x)");
            assert_eq!(c.rt.read(&curve, "expr").unwrap(), Value::text("sin(x)"));
            assert_eq!(c.rt.read(&curve, "visible").unwrap(), Value::Bool(true));
            c.choose_mode(2);
            let view = c.window();
            let drawings = drawn(view.tree());
            assert_eq!(drawings.len(), 1, "one plot");
            // The grid's lines and labels, the curve, and the samples the pointer reads.
            assert!(shape_count(drawings[0]) > 14 + 64, "{}", shape_count(drawings[0]));
            c.call("delete_curve", vec![Value::resource(&curve)]);
            let after = c.window();
            assert!(
                of_kind(after.tree(), "ui.Checkbox").is_empty(),
                "no curve is left to show:\n{}",
                dump(after.tree())
            );
        }

        phase surfaces_are_added_and_deleted_and_the_graph_opens_with_a_starter;
        {
            let c = Calc::new();
            c.choose_mode(3);
            let starter = c.window();
            assert_eq!(drawn(starter.tree()).len(), 1, "the starter surface is drawn");
            let surface = c.add_surface("sin(x)*cos(y)");
            assert_eq!(c.rt.read(&surface, "expr").unwrap(), Value::text("sin(x)*cos(y)"));
            let own = c.window();
            let graph = drawn(own.tree())[0];
            assert_eq!(
                mesh_cells(graph),
                12 * 12,
                "the surface is drawn as cells, not as bands"
            );
            assert!(
                shape_count(graph) > 7,
                "and the floor grid stands under them"
            );
            c.call("delete_surface", vec![Value::resource(&surface)]);
            let back = c.window();
            assert_eq!(drawn(back.tree()).len(), 1, "the starter stands again");
        }

        phase a_calculation_is_removed_and_the_record_cleared;
        {
            let c = Calc::new();
            let first = c.work_out_ref("1+1");
            c.worked_out("2+2");
            c.call("forget", vec![Value::resource(&first)]);
            let one = c.window();
            assert_eq!(of_kind(one.tree(), "ui.ListRow").len(), 1, "one is left");
            c.call("clear", vec![Value::resource(&c.root)]);
            let none = c.window();
            assert!(of_kind(none.tree(), "ui.ListRow").is_empty(), "the record is cleared");
        }

        phase a_text_that_is_no_calculation_says_what_is_wrong;
        {
            let c = Calc::new();
            let mut view = c.window();
            let field = of_kind(view.tree(), "ui.TextInput")[0].key.clone();
            for (typed, why) in [
                ("(2+3", "A bracket is not closed"),
                ("2 +", "The expression ends where a value belongs"),
                ("nope(2)", "There is no function by that name"),
                ("$2", "There is a character a calculation does not use"),
                ("2)", "There is more after the calculation"),
            ] {
                view.edit(&field, &Value::text(typed)).unwrap();
                view.poll_patches().unwrap();
                let result = of_kind(view.tree(), "ui.TextInput")
                    .into_iter()
                    .filter(|input| {
                        input.props.get("placeholder").and_then(Value::as_text) == Some("Result")
                    })
                    .next()
                    .expect("the result field");
                assert_eq!(
                    result.props.get("text").and_then(Value::as_text),
                    Some(why),
                    "`{typed}` says why it is no calculation:\n{}",
                    dump(view.tree())
                );
            }
        }

        phase the_graph_pans_up_and_zooms_each_axis_on_its_own;
        {
            let c = Calc::new();
            let mut view = c.window();
            let tabs = of_kind(view.tree(), "ui.ViewTabs")[0].key.clone();
            view.fire_with(&tabs, "change", Some(&Value::Int(2))).unwrap();
            view.poll_patches().unwrap();
            let drawing = of_kind(view.tree(), "ui.Drawing")[0].key.clone();
            // The grid is read against ten either way, each axis on its own.
            assert_eq!(
                shape_texts(of_kind(view.tree(), "ui.Drawing")[0]),
                ["-10", "10", "-10", "10"],
                "both axes start at ten"
            );
            // Taller takes the y range alone: the x numbers stay where they were.
            let taller = of_kind(view.tree(), "ui.Button")
                .into_iter()
                .find(|button| button.props.get("label").and_then(Value::as_text) == Some("Taller"))
                .expect("the Taller button")
                .key
                .clone();
            view.press(&taller).unwrap();
            view.poll_patches().unwrap();
            let taller_numbers = shape_texts(of_kind(view.tree(), "ui.Drawing")[0]);
            assert_eq!(
                taller_numbers,
                ["-10", "10", "-15", "15"],
                "the y range grew and the x range did not"
            );
            // A drag down the drawing pans the graph up: the height it moved stays under the hand.
            let drag = |view: &tilolang_runtime::ViewInstance, event: &str, at: f64| {
                view.fire_with(
                    &drawing,
                    event,
                    Some(&Value::List(vec![Value::Float(0.5), Value::Float(at)])),
                )
                .unwrap();
            };
            drag(&view, "drag_start", 0.5);
            drag(&view, "drag", 0.25);
            view.poll_patches().unwrap();
            assert_eq!(
                shape_texts(of_kind(view.tree(), "ui.Drawing")[0]),
                ["-10", "10", "-22.5", "7.5"],
                "the y range followed the hand up, and x did not move"
            );
        }
    }
}
