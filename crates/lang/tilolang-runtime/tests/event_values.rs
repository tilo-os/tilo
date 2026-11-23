//! What an event carries, read by the view that handles it: pressing a past calculation loads
//! its expression into the editor, which is the view's own state, computed from the calculation
//! when the press lands.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, Runtime, Value};

    const SOURCE: &str = r#"app so.tilo.sums "Sums"

type Calculation {
    expr: Text
    result: Text
}

type Calculator {
    past: Collection<Calculation>
}

view calculation(x: Calculation) {
    ui.Text("{x.expr} = {x.result}")
}

default view calculation for Calculation

action calculated(c: Calculator, expr: Text, result: Text) -> Calculation {
    c.past.insert(Calculation(expr: expr, result: result))
}

/// What the editor starts from: the calculation's expression, its spaces kept out.
fn expression_of(x: Calculation) -> Text {
    x.expr.trim()
}

view home(c: Calculator) {
    state expr: Text = ""
    state fresh: Bool = true
    ui.Text(if fresh { "New" } else { "Editing {expr}" })
    ui.TextInput(bind: expr, placeholder: "Expression")
    ui.VirtualList {
        on activate(x: Calculation): set(expr, expression_of(x)).then(set(fresh, false))
        for x in c.past {
            ui.ListItem(item: x) {
                ui.Text(x.expr)
            }
        }
    }
}
"#;

    tilo_test_env::scenario! {
        fn a_pressed_calculation_loads_into_the_editor;

        phase the_editor_shows_the_expression_of_the_calculation_pressed;
        {
            let app = App::compile(SOURCE, &[]);
            let auth = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), auth.clone()).unwrap();
            auth.trust(&app.load(&rt));
            let root = rt.create_root("Calculator").unwrap();
            let calculated = app.action("calculated");
            let made: Vec<_> = [(" 2 * 3 ", "6"), ("7 - 4", "3")]
                .into_iter()
                .map(|(expr, result)| {
                    returned_ref(
                        rt.invoke_action(
                            &me(),
                            (
                                calculated.as_str(),
                                vec![Value::resource(&root), Value::text(expr), Value::text(result)],
                            ),
                        )
                        .unwrap(),
                    )
                })
                .collect();
            let mut view = rt
                .open_view(&me(), &app.view("home"), vec![Value::resource(&root)])
                .unwrap();
            view.set_lazy_rows(false).unwrap();
            let said = |view: &tilolang_runtime::ViewInstance| text_prop(of_kind(view.tree(), "ui.Text")[0], "value");
            assert_eq!(said(&view), "New");
            let list = of_kind(view.tree(), "ui.VirtualList")[0].key.clone();

            view.fire_with(&list, "activate", Some(&Value::resource(&made[0])))
                .unwrap();
            view.poll_patches().unwrap();
            assert_eq!(said(&view), "Editing 2 * 3", "{}", dump(view.tree()));

            // Another calculation pressed replaces what the editor holds.
            view.fire_with(&list, "activate", Some(&Value::resource(&made[1])))
                .unwrap();
            view.poll_patches().unwrap();
            assert_eq!(said(&view), "Editing 7 - 4", "{}", dump(view.tree()));
            // The calculations are as they were: only the view's state changed.
            assert_eq!(rt.read(&made[0], "expr").unwrap(), Value::text(" 2 * 3 "));
        }
    }
}
