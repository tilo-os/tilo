//! A string literal inside an interpolation: `{f("x")}` written plainly and `{f(\"x\")}`
//! escaped both read the inner literal as its own string, and the outer string goes on after it.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, Runtime, Value};

    const SOURCE: &str = r#"app so.tilo.words "Words"

type Page {
    said: Text
}

root Page

default view greeting for Page

fn shout(s: Text) -> Text {
    s.upper()
}

view greeting(p: Page) {
    text("Hi {shout("there")} and {shout(\"again\")}")
}
"#;

    tilo_test_env::scenario! {
        fn a_string_literal_inside_an_interpolation_is_its_own_string;

        phase the_braces_hold_a_whole_expression_however_the_literal_is_written;
        {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
            app.load(&rt);
            let page = rt.create("Page").unwrap();
            let view = rt
                .open_view(&me(), &app.view("greeting"), vec![Value::resource(&page)])
                .unwrap();
            let said: Vec<String> = of_kind(view.tree(), "ui.Text")
                .iter()
                .map(|node| text_prop(node, "value"))
                .collect();
            assert_eq!(said, ["Hi THERE and AGAIN"]);
        }
    }
}
