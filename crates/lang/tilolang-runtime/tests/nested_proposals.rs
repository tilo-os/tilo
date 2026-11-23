//! An action another action calls that reaches outside: a person is asked once, and the call they
//! allow runs inside the caller. A call they decline ends the caller and takes back what it wrote.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, InvokeOutcome, RunOutcome, Runtime, Value};

    const SOURCE: &str = r#"app so.tilo.post "Post"

type Letter {
    to: Text
    answered: Bool = false
    sent: Bool = false
}

type Box {
    letters: Collection<Letter>
}

root Box

action write(b: Box, to: Text) -> Letter {
    b.letters.insert(Letter(to: to))
}

/// Posts the letter: it leaves the system, so it asks first, showing the letter.
action send(l: Letter) proposal compose(l) {
    require not l.sent "This letter went already"
    outside("post", l.to)
    l.sent = true
}

/// Answers the letter, and sends the answer.
action reply(l: Letter) -> Letter {
    l.answered = true
    send(l)
    l
}

view compose(l: Letter) {
    ui.Text(l.to)
}

view letters(b: Box) {
    for l in b.letters {
        ui.Text(l.to)
    }
}

default view letters for Box
"#;

    struct Post {
        app: App,
        auth: TestAuthority,
        rt: Runtime,
        letter: Value,
        dispatch: Dispatch,
    }

    fn post() -> Post {
        let app = App::compile(SOURCE, &[]);
        let auth = TestAuthority::new();
        let rt = Runtime::new(AmStore::new(), auth.clone()).unwrap();
        let dispatch = Dispatch::new();
        rt.set_foreign_invoker(dispatch.clone());
        auth.trust(&app.load(&rt));
        let root = rt.create_root("Box").unwrap();
        let letter = returned_ref(
            rt.invoke_action(
                &me(),
                (
                    app.action("write").as_str(),
                    vec![Value::resource(&root), Value::text("bo@example.org")],
                ),
            )
            .unwrap(),
        );
        Post {
            app,
            auth,
            rt,
            letter: Value::resource(&letter),
            dispatch,
        }
    }

    fn field(rt: &Runtime, letter: &Value, name: &str) -> Value {
        let Value::Ref(r) = letter else {
            panic!("a letter is a resource")
        };
        rt.read(&r.target, name).unwrap()
    }

    tilo_test_env::scenario! {
        fn a_call_another_action_makes_that_reaches_outside_asks_once;

        phase the_whole_reply_asks_through_the_dispatch_and_changes_nothing;
        {
            let p = post();
            let asked = p
                .rt
                .invoke_action(
                    &me(),
                    (p.app.action("reply").as_str(), vec![p.letter.clone()]),
                )
                .unwrap();
            let InvokeOutcome::Asked(_) = asked else {
                panic!("the reply asks before anything leaves: {asked:?}");
            };
            assert_eq!(
                p.dispatch.asked().0,
                p.app.action("reply"),
                "the dispatch is asked about the reply itself"
            );
            assert!(p.rt.pending_outbox().is_empty(), "asking sends nothing");
            assert_eq!(field(&p.rt, &p.letter, "answered"), Value::Bool(false));
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(false));
        }

        phase allowed_once_it_sends_from_inside_the_reply;
        {
            let p = post();
            p.auth.grant_outside(&p.app.action("reply"));
            let InvokeOutcome::Waits { run, .. } = p
                .rt
                .invoke_action(
                    &me(),
                    (p.app.action("reply").as_str(), vec![p.letter.clone()]),
                )
                .unwrap()
            else {
                panic!("the send inside the reply asks");
            };
            assert_eq!(p.dispatch.asked().0, p.app.action("send"));
            assert_eq!(
                field(&p.rt, &p.letter, "answered"),
                Value::Bool(true),
                "the reply ran up to the send"
            );
            // What the person's yes does: the dispatch runs the call as them.
            p.auth.grant_outside(&p.app.action("send"));
            let out = p.dispatch.allow(&p.rt, &run);
            assert!(
                matches!(out, RunOutcome::Done(_)),
                "the one answer covers the send the reply makes: {out:?}"
            );
            let pending = p.rt.pending_outbox();
            assert_eq!(pending.len(), 1, "the letter waits in the outbox");
            assert_eq!(pending[0].capability, "post");
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(true));
            assert_eq!(field(&p.rt, &p.letter, "answered"), Value::Bool(true));
        }

        phase refused_nothing_runs_and_the_replys_writes_are_taken_back;
        {
            let p = post();
            p.auth.grant_outside(&p.app.action("reply"));
            let InvokeOutcome::Waits { run, .. } = p
                .rt
                .invoke_action(
                    &me(),
                    (p.app.action("reply").as_str(), vec![p.letter.clone()]),
                )
                .unwrap()
            else {
                panic!("the send inside the reply asks");
            };
            let error = decline(&p.rt, &run);
            assert!(
                matches!(error, tilolang_runtime::RuntimeError::Denied(_)),
                "{error}"
            );
            assert!(p.rt.pending_outbox().is_empty(), "nothing leaves");
            assert_eq!(
                field(&p.rt, &p.letter, "answered"),
                Value::Bool(false),
                "what the reply wrote before it asked is taken back"
            );
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(false));
        }

        phase a_trusted_send_runs_without_asking;
        {
            let p = post();
            p.auth.grant_outside(&p.app.action("reply"));
            p.auth.grant_outside(&p.app.action("send"));
            let out = p
                .rt
                .invoke_action(
                    &me(),
                    (p.app.action("reply").as_str(), vec![p.letter.clone()]),
                )
                .unwrap();
            assert!(matches!(out, InvokeOutcome::Done(_)), "{out:?}");
            assert_eq!(p.dispatch.len(), 0, "no call needed a person");
            assert_eq!(p.rt.pending_outbox().len(), 1);
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(true));
        }
    }
}
