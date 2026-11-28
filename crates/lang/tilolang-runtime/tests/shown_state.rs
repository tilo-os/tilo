//! A view reads the state another window keeps, by the resource it shows: the shell reading an
//! editor's find state, again once it moves, and a caller acting for the person refused it.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::common::*;
    use serde_json::{Value as Json, json};
    use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
    use tilolang_runtime::view_state::StateKey;
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, Value};

    const SOURCE: &str = r#"app so.tilo.bar "Bar"

type Board {
    name: Text
}

root Board

default view bar for Board

view bar(b: Board) {
    if let found = state_shown("so.tilo.bar/{b.id()}", "find") {
        text(json.encode(found))
    } else {
        text("nothing")
    }
}

action read(b: Board) -> Text {
    if let found = state_shown("so.tilo.bar/{b.id()}", "find") { json.encode(found) } else { "nothing" }
}
"#;

    #[derive(Clone, Default)]
    struct Elsewhere(Rc<RefCell<Option<Json>>>);

    impl Invoker for Elsewhere {
        fn invoke(&self, invocation: &Invocation) -> Invoked {
            panic!("a state read is no call: {invocation:?}");
        }

        fn state_shown(&self, _: &str, _: &str) -> Result<Option<Json>, String> {
            Ok(self.0.borrow().clone())
        }
    }

    fn world(found: Option<Json>) -> (Runtime, App, ResourceRef, Elsewhere) {
        let dispatch = Elsewhere(Rc::new(RefCell::new(found)));
        let app = App::compile(SOURCE, &[]);
        let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
        rt.set_foreign_invoker(dispatch.clone());
        app.load(&rt);
        let board = rt.create_root("Board").unwrap();
        (rt, app, board, dispatch)
    }

    fn says(rt: &Runtime, app: &App, board: &ResourceRef) -> String {
        let view = rt
            .open_view(&me(), &app.view("bar"), vec![Value::resource(board)])
            .expect("the bar opens");
        text_prop(view.tree().of_kind("ui.Text")[0], "value")
    }

    #[test]
    fn a_view_reads_the_state_another_window_keeps_and_again_when_it_moves() {
        let (rt, app, board, dispatch) = world(Some(json!({"query": "x"})));
        assert_eq!(says(&rt, &app, &board), r#"{"query":"x"}"#);
        // What the window keeps now is the slot this view reads, kept without the view asking.
        *dispatch.0.borrow_mut() = None;
        rt.set_view_state(
            StateKey::shown(&format!("so.tilo.bar/{}", board.id_string()), me(), "find"),
            Value::Json(Box::new(json!({"query": "y"}))),
        );
        assert_eq!(says(&rt, &app, &board), r#"{"query":"y"}"#);
    }

    #[test]
    fn a_caller_acting_for_the_person_reads_no_other_windows_state() {
        let (rt, app, board, _) = world(Some(json!({"query": "x"})));
        let scout: tilolang_runtime::Actor =
            tilolang_runtime::ids::MemberId::agent("scout", "ada").into();
        let action = app.action("read");
        let err = rt
            .invoke_action(&scout, (action.as_str(), vec![Value::resource(&board)]))
            .expect_err("an agent acting for the person is refused");
        assert!(err.to_string().contains("the person's own"), "{err}");
    }
}
