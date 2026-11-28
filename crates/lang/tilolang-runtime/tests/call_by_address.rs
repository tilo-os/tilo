//! A call by address: the dispatch answers, or refuses in the words the code reads.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::common::*;
    use serde_json::{Value as Json, json};
    use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
    use tilolang_runtime::{AmStore, InvokeOutcome, Runtime, Value};

    const SOURCE: &str = r#"app so.tilo.probe "Probe"

action find(arguments: json.Value) -> json.Value {
    call("tl:so.tilo.notes:n1", "find", arguments)
}

action try_find(arguments: json.Value) -> Text {
    match try_call("tl:so.tilo.notes:n1", "find", arguments) {
        .ok(found) => json.encode(found),
        .err(why) => why,
    }
}
"#;

    #[derive(Clone, Default)]
    struct Answers {
        refusal: Rc<RefCell<Option<String>>>,
    }

    impl Invoker for Answers {
        fn invoke(&self, invocation: &Invocation) -> Invoked {
            panic!("a call by address is the dispatch's call: {invocation:?}");
        }

        fn call(&self, resource: &str, action: &str, arguments: &Json) -> Result<Json, String> {
            let wanted = ("tl:so.tilo.notes:n1", "find", &json!({"query": "x"}));
            if (resource, action, arguments) != wanted {
                return Err(format!("unexpected call {resource}#{action} {arguments}"));
            }
            match self.refusal.borrow().clone() {
                Some(why) => Err(why),
                None => Ok(json!({"matches": 5})),
            }
        }
    }

    #[test]
    fn a_call_reads_the_dispatchs_answer_and_its_refusal() {
        let dispatch = Answers::default();
        let app = App::compile(SOURCE, &[]);
        let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
        rt.set_foreign_invoker(dispatch.clone());
        app.load(&rt);
        let run = |name: &str| {
            let action = app.action(name);
            let arguments = Value::Json(Box::new(json!({"query": "x"})));
            rt.invoke_action(&me(), (action.as_str(), vec![arguments]))
        };
        let answer = || InvokeOutcome::Done(Value::Json(Box::new(json!({"matches": 5}))));
        assert_eq!(run("find").expect("the call ran"), answer());
        *dispatch.refusal.borrow_mut() = Some("You may not read this note".to_owned());
        assert_eq!(
            run("try_find").expect("the try ran"),
            InvokeOutcome::Done(Value::Text("You may not read this note".to_owned())),
            "the refusal reaches the code in the dispatch's own words"
        );
        let err = run("find").expect_err("a call fails the action with the refusal");
        assert!(
            err.to_string().contains("You may not read this note"),
            "{err}"
        );
    }
}
