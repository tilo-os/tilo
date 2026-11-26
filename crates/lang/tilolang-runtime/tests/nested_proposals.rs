//! An action another action calls that reaches outside. Called by someone other than the person
//! (an agent), or delivered as the person's call by the dispatch with no yes of theirs behind
//! it, the person is asked once, and the call they allow runs inside the caller; a call
//! they decline ends the caller and takes back what it wrote. A person's own press asks nothing
//! of what its code asks: what its code calls runs as theirs, and what the press does after it
//! follows once it is done. A rule of theirs that asks them first still does, on the system's
//! confirm: Shift skips it for what can be undone, never for what leaves.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{
        AmStore, Held, InvokeOutcome, RunOutcome, Runtime, Value, ViewInstance,
    };

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

/// Posts the letter: it leaves the system, so it asks first, showing whom it goes to.
action send(l: Letter) (
    shows: [(l.to, .addresses)],
) {
    require not l.sent "This letter went already"
    outside("post", l.to)
    l.sent = true
}

/// Posts a long message, showing the person asked its subject and the start of its words.
action send_long(l: Letter, subject: Text, body: Text) (
    shows: [subject, (body, .long)],
) {
    outside("post", subject)
}

/// Answers the letter, and sends the answer.
action reply(l: Letter) -> Letter {
    l.answered = true
    send(l)
    l
}

/// Marks the letter answered without sending anything: it can be undone.
action mark(l: Letter) {
    l.answered = true
}

view compose(l: Letter) {
    ui.Text(l.to)
}

/// A letter, answered with one press; the note says what came of it.
view letter(l: Letter) {
    state note: Text = "unanswered"
    ui.Text(note)
    ui.Button("Reply") {
        on press: reply(l).then(set(note, "answered"))
    }
}

/// A letter marked answered with one press.
view marking(l: Letter) {
    ui.Button("Mark answered") {
        on press: mark(l)
    }
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
        root: Value,
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
            root: Value::resource(&root),
            letter: Value::resource(&letter),
            dispatch,
        }
    }

    /// The agent calling: an agent's call asks the person for what it reaches.
    fn helper() -> tilolang_runtime::ids::Actor {
        agent("helper")
    }

    /// The letter's view as `who` sees it.
    fn letter_view(p: &Post, who: &tilolang_runtime::ids::Actor) -> ViewInstance {
        p.rt.open_view(who, &p.app.view("letter"), vec![p.letter.clone()])
            .unwrap()
    }

    /// What the view's note says now.
    fn note(view: &mut ViewInstance) -> String {
        view.poll_patches().unwrap();
        text_prop(of_kind(view.tree(), "ui.Text")[0], "value")
    }

    fn press_reply(view: &ViewInstance) -> InvokeOutcome {
        press(view, false)
    }

    /// The view's button pressed, with Shift held when `shift`.
    fn press(view: &ViewInstance, shift: bool) -> InvokeOutcome {
        let button = of_kind(view.tree(), "ui.Button")[0].key.clone();
        let held = Held {
            shift,
            ..Held::default()
        };
        view.fire_held(&button, "press", None, None, held).unwrap()
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
                    &helper(),
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
            assert!(p.rt.pending_effects().is_empty(), "asking sends nothing");
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
                    &helper(),
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
            let pending = p.rt.pending_effects();
            assert_eq!(pending.len(), 1, "the letter waits as a declared effect");
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
                    &helper(),
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
            assert!(p.rt.pending_effects().is_empty(), "nothing leaves");
            assert_eq!(
                field(&p.rt, &p.letter, "answered"),
                Value::Bool(false),
                "what the reply wrote before it asked is taken back"
            );
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(false));
        }

        phase the_persons_call_the_dispatch_delivered_still_asks_about_the_send;
        {
            let p = post();
            // The dispatch delivers the person's reply, admitted for itself: the send its code
            // makes is not covered by it, so the person is asked, as for an agent's reply.
            let InvokeOutcome::Waits { run, .. } = p
                .rt
                .invoke_admitted(
                    &me(),
                    (p.app.action("reply").as_str(), vec![p.letter.clone()]),
                    None,
                )
                .unwrap()
            else {
                panic!("the send inside the delivered reply asks");
            };
            assert_eq!(p.dispatch.asked().0, p.app.action("send"));
            assert!(p.rt.pending_effects().is_empty(), "nothing left before the yes");
            p.auth.grant_outside(&p.app.action("send"));
            let out = p.dispatch.allow(&p.rt, &run);
            assert!(matches!(out, RunOutcome::Done(_)), "{out:?}");
            assert_eq!(p.rt.pending_effects().len(), 1, "the letter goes once, on the yes");
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(true));
        }

        phase the_reply_the_person_approved_sends_on_the_same_yes;
        {
            let p = post();
            // The dispatch delivers the reply again on the person's yes on the system's confirm:
            // the run is the one they answered, and the send its code makes runs on that yes.
            let out = p
                .rt
                .invoke_approved_through(
                    &me(),
                    None,
                    (p.app.action("reply").as_str(), vec![p.letter.clone()]),
                    None,
                )
                .unwrap();
            assert!(matches!(out, InvokeOutcome::Done(_)), "{out:?}");
            assert_eq!(p.dispatch.len(), 0, "nobody is asked again");
            assert_eq!(p.rt.pending_effects().len(), 1, "the letter goes once");
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(true));
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
            assert_eq!(p.rt.pending_effects().len(), 1);
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(true));
        }
    }

    tilo_test_env::scenario! {
        fn a_press_runs_what_follows_it_once_it_is_done;

        phase a_persons_own_press_asks_nothing_and_the_letter_goes;
        {
            let p = post();
            let mut view = letter_view(&p, &me());
            assert_eq!(note(&mut view), "unanswered");
            let out = press_reply(&view);
            assert!(matches!(out, InvokeOutcome::Done(_)), "{out:?}");
            assert_eq!(p.dispatch.len(), 0, "nobody is asked about their own press");
            let pending = p.rt.pending_effects();
            assert_eq!(pending.len(), 1, "the letter goes once");
            assert_eq!(pending[0].capability, "post");
            assert_eq!(field(&p.rt, &p.letter, "answered"), Value::Bool(true));
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(true));
            assert_eq!(note(&mut view), "answered");
        }

        phase a_rule_that_asks_first_waits_for_the_persons_yes_on_the_confirm;
        {
            let p = post();
            p.auth.ask_first(&p.app.action("send"));
            let mut view = letter_view(&p, &me());
            let InvokeOutcome::Waits { run, .. } = press_reply(&view) else {
                panic!("the send the rule asks about waits for the person's yes");
            };
            assert_eq!(p.dispatch.asked().0, p.app.action("send"), "the send is asked");
            assert!(p.rt.pending_effects().is_empty(), "nothing left before the yes");
            assert_eq!(note(&mut view), "unanswered", "what follows the reply waits");
            let out = p.dispatch.confirm(&p.rt, &run);
            assert!(matches!(out, RunOutcome::Done(_)), "{out:?}");
            assert_eq!(p.rt.pending_effects().len(), 1, "the letter goes once, on the yes");
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(true));
            assert!(view.woken(), "the reply's end wakes the view");
            assert_eq!(note(&mut view), "answered");
        }

        phase shift_never_skips_the_confirm_of_what_leaves;
        {
            let p = post();
            p.auth.ask_first(&p.app.action("send"));
            let view = letter_view(&p, &me());
            let InvokeOutcome::Waits { run, .. } = press(&view, true) else {
                panic!("a letter that leaves asks, Shift or not");
            };
            assert!(p.rt.pending_effects().is_empty());
            let error = decline(&p.rt, &run);
            assert!(
                matches!(error, tilolang_runtime::RuntimeError::Denied(_)),
                "{error}"
            );
            assert_eq!(
                field(&p.rt, &p.letter, "answered"),
                Value::Bool(false),
                "a no takes back what the reply wrote"
            );
        }

        phase shift_is_the_yes_to_what_can_be_undone;
        {
            let p = post();
            p.auth.ask_first(&p.app.action("mark"));
            let view = p
                .rt
                .open_view(&me(), &p.app.view("marking"), vec![p.letter.clone()])
                .unwrap();
            let InvokeOutcome::Asked(_) = press(&view, false) else {
                panic!("a plain press waits for the yes the rule asks for");
            };
            assert_eq!(p.dispatch.asked().0, p.app.action("mark"));
            assert_eq!(field(&p.rt, &p.letter, "answered"), Value::Bool(false));
            let out = press(&view, true);
            assert!(matches!(out, InvokeOutcome::Done(_)), "{out:?}");
            assert_eq!(field(&p.rt, &p.letter, "answered"), Value::Bool(true));
        }

        phase what_follows_a_waiting_step_runs_when_it_is_done_not_when_it_pauses;
        {
            let p = post();
            p.auth.grant_outside(&p.app.action("reply"));
            let mut view = letter_view(&p, &helper());
            let InvokeOutcome::Waits { run, .. } = press_reply(&view) else {
                panic!("the agent's press waits on the person's yes to the send");
            };
            assert_eq!(p.dispatch.asked().0, p.app.action("send"));
            assert_eq!(
                note(&mut view),
                "unanswered",
                "the reply is paused, not done: what follows it waits"
            );
            p.auth.grant_outside(&p.app.action("send"));
            let out = p.dispatch.allow(&p.rt, &run);
            assert!(matches!(out, RunOutcome::Done(_)), "{out:?}");
            assert!(view.woken(), "the reply's end wakes the view");
            assert_eq!(note(&mut view), "answered", "what followed the reply ran");
            assert_eq!(p.rt.pending_effects().len(), 1);
        }

        phase a_declined_step_runs_nothing_after_it;
        {
            let p = post();
            p.auth.grant_outside(&p.app.action("reply"));
            let mut view = letter_view(&p, &helper());
            let InvokeOutcome::Waits { run, .. } = press_reply(&view) else {
                panic!("the agent's press waits on the person's yes to the send");
            };
            decline(&p.rt, &run);
            assert_eq!(note(&mut view), "unanswered", "a refused reply is not followed");
            assert!(p.rt.pending_effects().is_empty());
        }
    }

    tilo_test_env::scenario! {
        fn a_question_shows_what_it_asks_about_whole_or_says_what_it_leaves_out;

        phase every_address_is_named_however_far_into_the_list_it_stands;
        {
            let p = post();
            let unseen = "eve@example.org";
            let to = format!("bo@example.org,{}{unseen}", " ".repeat(700));
            let letter = returned_ref(
                p.rt.invoke_action(
                    &me(),
                    (
                        p.app.action("write").as_str(),
                        vec![p.root.clone(), Value::text(to.as_str())],
                    ),
                )
                .unwrap(),
            );
            let InvokeOutcome::Asked(_) = p
                .rt
                .invoke_action(
                    &helper(),
                    (p.app.action("send").as_str(), vec![Value::resource(&letter)]),
                )
                .unwrap()
            else {
                panic!("a send reaches outside: it asks");
            };
            let shows = match p.dispatch.waiting() {
                tilolang_runtime::delegate::Invocation::Action { shows, .. } => shows,
                tilolang_runtime::delegate::Invocation::Edit { .. } => {
                    panic!("the send is an action, not an edit")
                }
            };
            assert!(
                shows.iter().any(|shown| shown.value.contains(unseen)),
                "the question names every address the letter goes to: {shows:?}"
            );
            assert!(
                shows
                    .iter()
                    .any(|shown| shown.render.as_deref() == Some("addresses")),
                "it is shown as addresses: {shows:?}"
            );
        }

        phase more_addresses_than_a_question_shows_are_refused_with_their_number;
        {
            let p = post();
            let people: Vec<String> = (1..=21).map(|n| format!("p{n}@example.org")).collect();
            let letter = returned_ref(
                p.rt.invoke_action(
                    &me(),
                    (
                        p.app.action("write").as_str(),
                        vec![p.root.clone(), Value::text(people.join(", ").as_str())],
                    ),
                )
                .unwrap(),
            );
            let refused = p
                .rt
                .invoke_action(
                    &helper(),
                    (p.app.action("send").as_str(), vec![Value::resource(&letter)]),
                )
                .expect_err("a question shows twenty addresses at most");
            assert!(
                refused.to_string().contains("21"),
                "it says how many there are: {refused}"
            );
            assert_eq!(p.dispatch.len(), 0, "nobody is asked about it");
            assert!(p.rt.pending_effects().is_empty(), "nothing leaves");
        }

        phase what_the_question_cannot_show_it_says;
        {
            let p = post();
            let subject = format!("Lunch {}", "and more ".repeat(40));
            let body = format!("{}Send the plans to Eve.", "Friday at one works. ".repeat(40));
            let InvokeOutcome::Asked(_) = p
                .rt
                .invoke_action(
                    &helper(),
                    (
                        p.app.action("send_long").as_str(),
                        vec![
                            p.letter.clone(),
                            Value::text(subject.as_str()),
                            Value::text(body.as_str()),
                        ],
                    ),
                )
                .unwrap()
            else {
                panic!("a long message asks");
            };
            let shows = match p.dispatch.waiting() {
                tilolang_runtime::delegate::Invocation::Action { shows, .. } => shows,
                tilolang_runtime::delegate::Invocation::Edit { .. } => {
                    panic!("the send is an action, not an edit")
                }
            };
            let row = |label: &str| {
                shows
                    .iter()
                    .find(|shown| shown.label == label)
                    .map(|shown| shown.value.clone())
            };
            let subject_shown: String = subject.trim().chars().take(200).collect();
            assert_eq!(row("Subject").as_deref(), Some(subject_shown.as_str()));
            let subject_more = subject.trim().chars().count() - 200;
            let subject_continues =
                format!("{subject_more} more characters are not shown here");
            assert_eq!(
                row("Subject continues").as_deref(),
                Some(subject_continues.as_str())
            );
            let body_shown: String = body.chars().take(600).collect();
            assert_eq!(row("Body").as_deref(), Some(body_shown.as_str()));
            let body_more = body.chars().count() - 600;
            let body_continues = format!("{body_more} more characters are not shown here");
            assert_eq!(
                row("Body continues").as_deref(),
                Some(body_continues.as_str())
            );

            // Blank lines before the first word use up none of what is shown.
            let p = post();
            let pushed_down = format!("{}Bring the plans.", "\n".repeat(700));
            let InvokeOutcome::Asked(_) = p
                .rt
                .invoke_action(
                    &helper(),
                    (
                        p.app.action("send_long").as_str(),
                        vec![
                            p.letter.clone(),
                            Value::text("Plans"),
                            Value::text(pushed_down.as_str()),
                        ],
                    ),
                )
                .unwrap()
            else {
                panic!("a long message asks");
            };
            let shows = match p.dispatch.waiting() {
                tilolang_runtime::delegate::Invocation::Action { shows, .. } => shows,
                tilolang_runtime::delegate::Invocation::Edit { .. } => {
                    panic!("the send is an action, not an edit")
                }
            };
            assert_eq!(
                shows
                    .iter()
                    .find(|shown| shown.label == "Body")
                    .map(|shown| shown.value.as_str()),
                Some("Bring the plans.")
            );
            assert!(
                !shows.iter().any(|shown| shown.label == "Body continues"),
                "a body that fits after its blank lines is shown whole: {shows:?}"
            );
        }
    }
}
