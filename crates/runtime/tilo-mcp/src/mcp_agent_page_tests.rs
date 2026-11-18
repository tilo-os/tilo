//! What the page of one action says of a structured argument: how its value is written.
use super::*;

#[test]
fn an_actions_page_writes_out_what_a_structured_argument_takes() {
    // A star after a parameter says its value is an object or a list, and no
    // more. The page of the one action says how that value is written, so
    // that the first call can be the right one.
    let notes = Notes::new(true);
    let asked = notes.row(
        TODO,
        TODO,
        "notes.page",
        "Todo",
        Some((
            "ask",
            "Ask",
            vec![
                ActionParam::required("detail", ParamType::Object, "What is asked for")
                    .with_schema(json!({
                        "type": "object",
                        "required": ["kind", "places"],
                        "properties": {
                            "kind": {"enum": ["login", "key"]},
                            "places": {"type": "array", "items": {"type": "string"}},
                        },
                    })),
                ActionParam::optional("note", ParamType::Object, "Anything else"),
            ],
            tilo_actions::OperationKind::Document,
        )),
        false,
    );
    let page =
        super::super::render::action_page("notes/Todo", asked.action.as_ref().unwrap(), None);
    assert!(
        page.contains(
            "\n  detail:object* — What is asked for\n    written as \
             {\"kind\": \"login\" | \"key\", \"places\": [\"<text>\"]}"
        ),
        "{page}"
    );
    // A parameter whose declaration says no more than its type has no such line.
    assert!(page.ends_with("\n  note?:object — Anything else"), "{page}");
}
