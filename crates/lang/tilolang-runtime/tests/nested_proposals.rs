//! A proposal another action calls. The caller reaches outside through it, so a person is asked
//! once: allowed, the proposal runs inside the caller and queues its effect, without asking
//! again; refused, nothing runs. A caller the workspace lets run on its own leaves the proposal to
//! ask for itself, and allowing that runs it inside the caller too.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, InvokeOutcome, Resolution, RunOutcome, Runtime, Value};

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
    }

    fn post() -> Post {
        let app = App::compile(SOURCE, &[]);
        let auth = TestAuthority::new();
        let rt = Runtime::new(AmStore::new(), auth.clone()).unwrap();
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
        }
    }

    /// The reply, which asks before anything leaves: what it asks about.
    fn replied(p: &Post, asks: &str) -> tilolang_runtime::Suggestion {
        let asked = match p
            .rt
            .invoke_action(
                &me(),
                (p.app.action("reply").as_str(), vec![p.letter.clone()]),
            )
            .unwrap()
        {
            InvokeOutcome::Suggested(asked) => asked,
            other => panic!("the reply asks before the letter leaves: {other:?}"),
        };
        assert_eq!(asked.action, p.app.action(asks), "the ask is the {asks}'s");
        assert!(p.rt.pending_outbox().is_empty(), "asking sends nothing");
        asked
    }

    fn field(rt: &Runtime, letter: &Value, name: &str) -> Value {
        let Value::Ref(r) = letter else {
            panic!("a letter is a resource")
        };
        rt.read(&r.target, name).unwrap()
    }

    tilo_test_env::scenario! {
        fn a_proposal_another_action_calls_runs_once_the_person_allows_it;

        phase allowed_once_it_sends_from_inside_the_reply;
        {
            let p = post();
            let asked = replied(&p, "reply");
            let out = p.rt.resolve(&asked.run, &me(), Resolution::Approve).unwrap();
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

        phase refused_nothing_runs;
        {
            let p = post();
            let asked = replied(&p, "reply");
            p.rt.resolve(&asked.run, &me(), Resolution::Decline).unwrap();
            assert!(p.rt.pending_outbox().is_empty(), "nothing leaves");
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(false));
            assert_eq!(field(&p.rt, &p.letter, "answered"), Value::Bool(false));
        }

        phase allowed_through_the_record_the_person_sees;
        {
            let p = post();
            let asked = replied(&p, "reply");
            // The host records what its person is asked as the invocation their window shows.
            p.rt.record_suggestion(&me(), &asked.action, asked.args.clone(), &asked)
                .unwrap();
            let Value::Ref(r) = &p.letter else {
                panic!("a letter is a resource")
            };
            let waiting = p.rt.proposals_of(&r.target);
            assert_eq!(waiting.len(), 1, "the ask waits on the letter it is about");
            let state = p
                .rt
                .resolve_invocation(&waiting[0].0, &me(), Resolution::Approve)
                .unwrap();
            assert_eq!(state, "done", "one Allow is the whole answer");
            assert_eq!(p.rt.pending_outbox().len(), 1, "the letter waits in the outbox");
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(true));
            assert!(
                p.rt.proposals_of(&r.target).is_empty(),
                "nothing is left to answer"
            );
        }

        phase a_reply_let_run_leaves_the_send_to_ask_and_allowing_it_sends;
        {
            let p = post();
            p.auth.grant_outside(&p.app.action("reply"));
            let asked = replied(&p, "send");
            assert_eq!(
                field(&p.rt, &p.letter, "answered"),
                Value::Bool(true),
                "the reply ran up to the send"
            );
            let out = p.rt.resolve(&asked.run, &me(), Resolution::Approve).unwrap();
            assert!(matches!(out, RunOutcome::Done(_)), "the reply finishes: {out:?}");
            assert_eq!(p.rt.pending_outbox().len(), 1, "the letter waits in the outbox");
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(true));
        }

        phase a_reply_let_run_whose_send_is_refused_sends_nothing;
        {
            let p = post();
            p.auth.grant_outside(&p.app.action("reply"));
            let asked = replied(&p, "send");
            p.rt.resolve(&asked.run, &me(), Resolution::Decline).unwrap();
            assert!(p.rt.pending_outbox().is_empty(), "nothing leaves");
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(false));
        }
    }
}
