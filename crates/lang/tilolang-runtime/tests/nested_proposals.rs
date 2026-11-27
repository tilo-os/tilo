//! An action another action calls that reaches outside. Called by someone other than the person
//! (an agent), or delivered as the person's call by the dispatch with no yes of theirs behind
//! it, the person is asked once, and the call they allow runs inside the caller; a call
//! they decline ends the caller and takes back what it wrote. A person's own press acts at once,
//! unless the action leaves the system, which always confirms first, the owner's press included
//! and nothing leaving before the yes, or its app marks it destructive, which confirms unless
//! Shift is held. A rule of theirs that asks them first still asks too, unless Shift is held.
//! The yes they give for one app's action answers the calls that action makes of its own app,
//! never one it makes into another app: that app's grants decide that call.

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

/// Copies whom the letter goes to: the device's clipboard is outside the workspace, and nothing
/// leaves the system.
action copy_address(l: Letter) uses clipboard {
    clipboard.copy(l.to)
}

/// Throws the letter away: undo brings it back.
destructive action discard(b: Box, l: Letter) {
    b.letters.remove(l)
}

/// Throws every letter away, each with the destructive action that does.
action tidy(b: Box) {
    for l in b.letters {
        discard(b, l)
    }
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

/// A letter sent with one press.
view sending(l: Letter) {
    ui.Button("Send") {
        on press: send(l)
    }
}

/// Whom a letter goes to, copied with one press.
view copying(l: Letter) {
    ui.Button("Copy") {
        on press: copy_address(l)
    }
}

/// The letters, each thrown away with one press.
view discarding(b: Box) {
    for l in b.letters {
        ui.Button("Discard") {
            on press: discard(b, l)
        }
    }
}

/// The box tidied with one press.
view tidying(b: Box) {
    ui.Button("Tidy") {
        on press: tidy(b)
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

    /// Another app, whose own action calls Post's: what the person pressed there is their act in
    /// Other, and what it reaches in Post is Post's to grant.
    const OTHER: &str = r#"app so.tilo.other "Other"

use so.tilo.post as post

/// Stamps the letter through Post's own send, and stamps it outside.
action stamp(l: post.Letter) {
    post.send(l)
    outside("post", l.to)
}

/// The letter, stamped with one press.
view stamping(l: post.Letter) {
    ui.Button("Stamp") {
        on press: stamp(l)
    }
}
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

    /// The person's view `name` of the letter.
    fn view_of(p: &Post, name: &str) -> ViewInstance {
        p.rt.open_view(&me(), &p.app.view(name), vec![p.letter.clone()])
            .unwrap()
    }

    /// The dispatch delivering the person's call of `action` on the letter again, on their yes on
    /// the system's confirm.
    fn approved(p: &Post, action: &str) -> InvokeOutcome {
        p.rt.invoke_approved_through(
            &me(),
            None,
            (p.app.action(action).as_str(), vec![p.letter.clone()]),
            None,
        )
        .unwrap()
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

        phase an_approved_press_does_not_answer_another_apps_call;
        {
            let p = post();
            let other = App::compile(OTHER, &[p.app.meta().clone()]);
            p.auth.trust(&other.load(&p.rt));
            let view = p
                .rt
                .open_view(&me(), &other.view("stamping"), vec![p.letter.clone()])
                .unwrap();
            let InvokeOutcome::Asked(_) = press(&view, false) else {
                panic!("a press of what leaves asks first");
            };
            assert_eq!(
                p.dispatch.asked().0,
                other.action("stamp"),
                "the press asks about Other's own action"
            );
            // The person's yes runs Other's action; the send it makes into Post is Post's to
            // grant, so it asks although the press was allowed.
            let out = p
                .rt
                .invoke_approved_through(
                    &me(),
                    None,
                    (other.action("stamp").as_str(), vec![p.letter.clone()]),
                    None,
                )
                .unwrap();
            let InvokeOutcome::Waits { run, .. } = out else {
                panic!("Post's own send is judged by Post's grants: {out:?}");
            };
            assert_eq!(
                p.dispatch.asked().0,
                p.app.action("send"),
                "the call into Post is asked: the yes was for Other"
            );
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(false));
            p.auth.grant_outside(&p.app.action("send"));
            let out = p.dispatch.allow(&p.rt, &run);
            assert!(matches!(out, RunOutcome::Done(_)), "{out:?}");
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(true));
            assert_eq!(
                p.rt.pending_effects().len(),
                2,
                "Post's send and Other's own outside effect"
            );
        }
    }

    tilo_test_env::scenario! {
        fn a_press_runs_what_follows_it_once_it_is_done;

        phase send_on_a_letter_confirms_and_nothing_leaves_before_the_yes;
        {
            let p = post();
            let view = view_of(&p, "sending");
            let InvokeOutcome::Asked(_) = press(&view, false) else {
                panic!("the owner's own Send confirms first");
            };
            assert_eq!(p.dispatch.asked().0, p.app.action("send"), "the send is asked");
            assert!(p.rt.pending_effects().is_empty(), "nothing leaves before the yes");
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(false));
            // The yes is the dispatch delivering the call again as the person approved it.
            let out = approved(&p, "send");
            assert!(matches!(out, InvokeOutcome::Done(_)), "{out:?}");
            assert_eq!(p.rt.pending_effects().len(), 1, "the letter goes once, on the yes");
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(true));
        }

        phase send_asks_where_the_workspace_grants_outside_reach;
        {
            let p = post();
            p.auth.grant_outside(&p.app.action("send"));
            let view = view_of(&p, "sending");
            let InvokeOutcome::Asked(_) = press(&view, false) else {
                panic!("the policy lets them send; the press still confirms it");
            };
            assert_eq!(p.dispatch.asked().0, p.app.action("send"), "the send is asked");
            assert!(p.rt.pending_effects().is_empty(), "nothing leaves before the yes");
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(false));
            let out = approved(&p, "send");
            assert!(matches!(out, InvokeOutcome::Done(_)), "{out:?}");
            assert_eq!(p.rt.pending_effects().len(), 1, "the letter goes once, on the yes");
        }

        phase an_agents_granted_send_runs_by_its_grant;
        {
            let p = post();
            p.auth.grant_outside(&p.app.action("send"));
            let out = p
                .rt
                .invoke_action(
                    &helper(),
                    (p.app.action("send").as_str(), vec![p.letter.clone()]),
                )
                .unwrap();
            assert!(matches!(out, InvokeOutcome::Done(_)), "{out:?}");
            assert_eq!(p.dispatch.len(), 0, "an agent's grant decides its call alone");
            assert_eq!(p.rt.pending_effects().len(), 1);
        }

        phase shift_on_send_still_asks;
        {
            let p = post();
            let view = view_of(&p, "sending");
            let InvokeOutcome::Asked(_) = press(&view, true) else {
                panic!("Shift never skips the confirm of what leaves");
            };
            assert_eq!(p.dispatch.asked().0, p.app.action("send"));
            assert!(p.rt.pending_effects().is_empty());
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(false));
        }

        phase a_press_of_what_sends_on_the_way_confirms_it_whole_and_the_yes_sends_once;
        {
            let p = post();
            let mut view = letter_view(&p, &me());
            let InvokeOutcome::Asked(_) = press_reply(&view) else {
                panic!("a reply that sends confirms before it runs");
            };
            assert_eq!(p.dispatch.asked().0, p.app.action("reply"), "the reply is asked whole");
            assert!(p.rt.pending_effects().is_empty(), "nothing left before the yes");
            assert_eq!(field(&p.rt, &p.letter, "answered"), Value::Bool(false));
            assert_eq!(note(&mut view), "unanswered");
            let out = approved(&p, "reply");
            assert!(matches!(out, InvokeOutcome::Done(_)), "{out:?}");
            assert_eq!(p.dispatch.len(), 1, "the yes covers the send the reply makes");
            assert_eq!(p.rt.pending_effects().len(), 1, "the letter goes once");
            assert_eq!(field(&p.rt, &p.letter, "answered"), Value::Bool(true));
            assert_eq!(field(&p.rt, &p.letter, "sent"), Value::Bool(true));
        }

        phase what_follows_a_press_the_person_answered_runs_on_the_yes;
        {
            let p = post();
            let mut view = letter_view(&p, &me());
            let InvokeOutcome::Asked(call) = press_reply(&view) else {
                panic!("a reply that sends asks for its yes");
            };
            assert_eq!(note(&mut view), "unanswered");
            let out = approved(&p, "reply");
            assert!(matches!(out, InvokeOutcome::Done(_)), "{out:?}");
            assert_eq!(p.dispatch.len(), 1, "the yes covers the send the reply makes");
            // The host answers the call the press waited on once it ended: what follows the
            // reply runs then, as it would had the press waited on another app's answer.
            p.rt.call_ended(&call, true);
            assert_eq!(
                note(&mut view),
                "answered",
                "what followed the reply ran on the yes"
            );
        }

        phase shift_never_skips_the_confirm_of_what_leaves;
        {
            let p = post();
            p.auth.ask_first(&p.app.action("send"));
            let view = letter_view(&p, &me());
            let InvokeOutcome::Asked(_) = press(&view, true) else {
                panic!("a letter that leaves asks, Shift or not");
            };
            assert!(p.rt.pending_effects().is_empty());
            assert_eq!(
                field(&p.rt, &p.letter, "answered"),
                Value::Bool(false),
                "nothing of the reply ran before the yes"
            );
        }

        phase an_undoable_press_asks_nothing;
        {
            let p = post();
            let view = view_of(&p, "marking");
            let out = press(&view, false);
            assert!(matches!(out, InvokeOutcome::Done(_)), "{out:?}");
            assert_eq!(p.dispatch.len(), 0, "nobody is asked about what undo takes back");
            assert_eq!(field(&p.rt, &p.letter, "answered"), Value::Bool(true));
        }

        phase shift_on_an_undoable_press_asks_nothing;
        {
            let p = post();
            let view = view_of(&p, "marking");
            let out = press(&view, true);
            assert!(matches!(out, InvokeOutcome::Done(_)), "{out:?}");
            assert_eq!(p.dispatch.len(), 0);
            assert_eq!(field(&p.rt, &p.letter, "answered"), Value::Bool(true));
        }

        phase a_copy_acts_on_the_press_though_it_reaches_outside;
        {
            let p = post();
            let view = view_of(&p, "copying");
            let out = press(&view, false);
            assert!(matches!(out, InvokeOutcome::Done(_)), "{out:?}");
            assert_eq!(p.dispatch.len(), 0, "a copy leaves nothing to confirm");
        }

        phase a_destructive_press_confirms_and_shift_skips_it;
        {
            let p = post();
            let letters = || {
                let Value::Ref(root) = &p.root else {
                    panic!("the box is a resource")
                };
                let Value::List(held) = p.rt.read(&root.target, "letters").unwrap() else {
                    panic!("the box holds its letters")
                };
                held.len()
            };
            let view = p
                .rt
                .open_view(&me(), &p.app.view("discarding"), vec![p.root.clone()])
                .unwrap();
            let InvokeOutcome::Asked(_) = press(&view, false) else {
                panic!("a plain press of a delete confirms it, though the policy allows it");
            };
            assert_eq!(p.dispatch.asked().0, p.app.action("discard"));
            assert_eq!(letters(), 1, "nothing is thrown away before the yes");
            let out = press(&view, true);
            assert!(matches!(out, InvokeOutcome::Done(_)), "{out:?}");
            assert_eq!(letters(), 0, "Shift is the yes to a delete undo takes back");
        }

        phase a_press_of_what_deletes_twice_asks_once;
        {
            let p = post();
            let Value::Ref(root) = &p.root else {
                panic!("the box is a resource")
            };
            p.rt.invoke_action(
                &me(),
                (
                    p.app.action("write").as_str(),
                    vec![p.root.clone(), Value::text("cy@example.org")],
                ),
            )
            .unwrap();
            let letters = || {
                let Value::List(held) = p.rt.read(&root.target, "letters").unwrap() else {
                    panic!("the box holds its letters")
                };
                held.len()
            };
            assert_eq!(letters(), 2);
            let view = p
                .rt
                .open_view(&me(), &p.app.view("tidying"), vec![p.root.clone()])
                .unwrap();
            let InvokeOutcome::Waits { run, .. } = press(&view, false) else {
                panic!("the first delete the press reaches confirms");
            };
            assert_eq!(p.dispatch.asked().0, p.app.action("discard"));
            assert_eq!(letters(), 2, "nothing is thrown away before the yes");
            let out = p.dispatch.allow(&p.rt, &run);
            assert!(matches!(out, RunOutcome::Done(_)), "{out:?}");
            assert_eq!(letters(), 0, "both letters went on the one yes");
            assert_eq!(p.dispatch.len(), 1, "the press asked once");
        }

        phase shift_is_the_yes_to_what_can_be_undone;
        {
            let p = post();
            p.auth.ask_first(&p.app.action("mark"));
            let view = view_of(&p, "marking");
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
