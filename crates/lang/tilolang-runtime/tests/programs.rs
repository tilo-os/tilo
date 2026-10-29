//! Agents' programs: a session over Notes and Tasks that finds, reads, acts, keeps its `let`s
//! between programs, asks where the agent's code is not trusted, and teaches on errors.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::program::{ProgramOutcome, ProgramReply, Session};
    use tilolang_runtime::{
        AmStore, InvokeOutcome, Principal, ResourceRef, Runtime, SimpleAuthority, Value,
    };

    const TODO: &str = include_str!("../../../../apps/tl/tasks/app.tl");
    const NOTES: &str = include_str!("../../../../apps/tl/notes/app.tl");

    struct World {
        rt: Runtime,
        auth: SimpleAuthority,
        todo: App,
        list: ResourceRef,
        notes: Vec<ResourceRef>,
    }

    fn agent() -> Principal {
        Principal::new("agent:claude")
    }

    fn world() -> World {
        let todo = App::compile(TODO, &[]);
        let notes_app = App::compile(NOTES, &[]);
        let auth = SimpleAuthority::new();
        let rt = Runtime::new(AmStore::new(), auth.clone()).unwrap();
        todo.load(&rt);
        notes_app.load(&rt);
        let list = rt.create("Tasks").unwrap();
        let nb = rt.create("Notes").unwrap();
        let mut notes = Vec::new();
        let mut titled = Vec::new();
        for title in ["Launch video script", "Groceries", "Launch checklist"] {
            let made = rt
                .invoke_action(
                    &me(),
                    (
                        notes_app.action("new_note").as_str(),
                        vec![Value::resource(&nb)],
                    ),
                )
                .unwrap();
            let InvokeOutcome::Done(Value::Ref(note)) = made else {
                panic!("new_note made no note: {made:?}");
            };
            let note = (*note.target).clone();
            rt.invoke_action(
                &me(),
                (
                    notes_app.action("retitle").as_str(),
                    vec![Value::resource(&note), Value::text(title)],
                ),
            )
            .unwrap();
            titled.push((title.to_lowercase(), note.clone()));
            notes.push(note);
        }
        // The host's text search, as the platform's index answers it: every word held.
        rt.set_text_search(move |words, limit| {
            let words: Vec<String> = words.split_whitespace().map(str::to_lowercase).collect();
            titled
                .iter()
                .filter(|(title, _)| words.iter().all(|w| title.contains(w.as_str())))
                .map(|(_, note)| note.clone())
                .take(limit)
                .collect()
        });
        World {
            rt,
            auth,
            todo,
            list,
            notes,
        }
    }

    impl World {
        fn list_literal(&self) -> String {
            format!("@tasks.Tasks:{}", self.list.id_string())
        }

        fn todo_titles(&self) -> Vec<String> {
            let Ok(Value::List(items)) = self.rt.read(&self.list, "items") else {
                panic!("the list has no items field");
            };
            items
                .iter()
                .map(|item| {
                    let Value::Ref(r) = item else {
                        panic!("not a todo")
                    };
                    match self.rt.read(&r.target, "title") {
                        Ok(Value::Text(t)) => t,
                        other => panic!("a todo without a title: {other:?}"),
                    }
                })
                .collect()
        }
    }

    fn done(reply: &ProgramReply) -> String {
        assert!(
            matches!(reply.outcome, ProgramOutcome::Done),
            "the program did not finish: {:?}\n{}",
            reply.outcome,
            reply.output.join("\n")
        );
        reply.output.join("\n")
    }

    fn rejected(reply: &ProgramReply) -> String {
        let ProgramOutcome::Rejected(lines) = &reply.outcome else {
            panic!("the program compiled: {:?}", reply.outcome);
        };
        lines.join("\n")
    }

    #[test]
    fn lets_survive_between_programs_of_a_session() {
        let w = world();
        w.auth.trust_principal(&agent().0);
        let mut session = Session::new();
        let first = session.run(
            &w.rt,
            &agent(),
            &format!(
                "let hits = find(\"Note launch\")\nfor n in hits {{\n    tasks.add({}, \"Follow up: {{n.title}}\")\n}}\nprint(hits.len(), \"notes\")\nhits",
                w.list_literal()
            ),
        );
        let out = done(&first);
        assert!(out.starts_with("2 notes"), "{out}");
        assert!(out.contains("\"Launch video script\""), "{out}");
        assert!(out.contains("@notes.Note:"), "{out}");
        assert_eq!(
            w.todo_titles(),
            [
                "Follow up: Launch video script",
                "Follow up: Launch checklist"
            ]
        );
        assert_eq!(session.bindings().len(), 1);
        assert_eq!(session.bindings()[0].name, "hits");

        let second = session.run(&w.rt, &agent(), "print(hits.len())\nlet n = hits.len()");
        assert_eq!(done(&second), "2");
        let third = session.run(&w.rt, &agent(), "n + 1");
        assert_eq!(done(&third), "3");
    }

    #[test]
    fn a_printed_reference_is_input_to_the_next_program() {
        let w = world();
        let mut session = Session::new();
        let found = session.run(&w.rt, &agent(), "find(\"Note groceries\")");
        let out = done(&found);
        let literal = out.split_whitespace().next().unwrap().to_owned();
        assert!(literal.starts_with("@notes.Note:"), "{out}");
        let read = session.run(&w.rt, &agent(), &format!("print(read({literal}))"));
        let text = done(&read);
        assert!(
            text.starts_with(&format!("{literal} \"Groceries\"  Note: 1 paragraph")),
            "{text}"
        );
        assert!(
            text.contains(&format!("{literal} title  Groceries")),
            "{text}"
        );
        assert!(
            text.contains(&format!("{literal} .pinned  false")),
            "{text}"
        );
    }

    #[test]
    fn find_without_a_type_finds_every_app_and_of_narrows() {
        let w = world();
        let mut session = Session::new();
        let reply = session.run(
            &w.rt,
            &agent(),
            "let all = find(\"launch\")\nprint(all.of(Note).len(), all.of(Task).len())",
        );
        assert_eq!(done(&reply), "2 0");
        assert_eq!(w.notes.len(), 3);
    }

    #[test]
    fn an_action_the_agent_may_not_take_alone_asks_and_the_program_waits() {
        let w = world();
        let mut session = Session::new();
        let reply = session.run(
            &w.rt,
            &agent(),
            &format!(
                "print(\"before\")\ntasks.add({}, \"Ask first\")\nprint(\"after\")",
                w.list_literal()
            ),
        );
        let ProgramOutcome::Suspended(asked) = &reply.outcome else {
            panic!("the action did not ask: {:?}", reply.outcome);
        };
        assert_eq!(asked.action, w.todo.action("add"));
        assert_eq!(reply.output, ["before"]);
        assert!(w.todo_titles().is_empty());
    }

    #[test]
    fn errors_name_what_was_meant() {
        let w = world();
        let mut session = Session::new();
        let typo = rejected(&session.run(&w.rt, &agent(), "tasks.ad(1)"));
        assert!(typo.contains("did you mean `add`?"), "{typo}");
        let word = rejected(&session.run(&w.rt, &agent(), "prnt(1)"));
        assert!(word.contains("did you mean `print`?"), "{word}");
        let ty = rejected(&session.run(&w.rt, &agent(), "find(\"Notee launch\")"));
        assert!(ty.contains("did you mean `Note`?"), "{ty}");
        done(&session.run(&w.rt, &agent(), "let hits = find(\"Note launch\")"));
        let name = rejected(&session.run(&w.rt, &agent(), "print(hitz)"));
        assert!(name.contains("did you mean `hits`?"), "{name}");
        let field = rejected(&session.run(&w.rt, &agent(), "for n in hits { print(n.titel) }"));
        assert!(field.contains("did you mean `title`?"), "{field}");
    }

    #[test]
    fn a_program_that_fails_to_compile_keeps_the_session() {
        let w = world();
        let mut session = Session::new();
        done(&session.run(&w.rt, &agent(), "let x = 41"));
        rejected(&session.run(&w.rt, &agent(), "let x = nope"));
        assert_eq!(done(&session.run(&w.rt, &agent(), "x + 1")), "42");
    }

    #[test]
    fn a_programs_assignment_is_its_apps_field_edit() {
        use std::cell::RefCell;
        use std::rc::Rc;
        use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};

        struct Sent(Rc<RefCell<Vec<Invocation>>>);
        impl Invoker for Sent {
            fn invoke(&self, invocation: &Invocation) -> Invoked {
                self.0.borrow_mut().push(invocation.clone());
                Invoked::Done(Value::Unit)
            }
        }

        let w = world();
        let note = &w.notes[0];
        let assign = format!(
            "let n = @notes.Note:{}\nn.title = \"Mine\"",
            note.id_string()
        );
        // A runtime that runs its apps' code itself has nowhere to send it: the program never
        // writes the record.
        let reply = Session::new().run(&w.rt, &agent(), &assign);
        let ProgramOutcome::Failed(why) = &reply.outcome else {
            panic!("an edit with nowhere to go ran: {:?}", reply.outcome);
        };
        assert!(why.contains("through its app"), "{why}");
        let sent = Rc::new(RefCell::new(Vec::new()));
        w.rt.set_invoker(Sent(Rc::clone(&sent)));
        let reply = Session::new().run(&w.rt, &agent(), &assign);
        done(&reply);
        let sent = sent.borrow();
        let [
            Invocation::Edit {
                field,
                value,
                target,
                ..
            },
        ] = sent.as_slice()
        else {
            panic!("one field edit: {sent:?}");
        };
        assert_eq!((field.as_str(), value), ("title", &Value::text("Mine")));
        assert_eq!(target.id_string(), note.id_string());
        assert_ne!(
            w.rt.read(note, "title").unwrap(),
            Value::text("Mine"),
            "the program wrote nothing itself"
        );
    }

    #[test]
    fn a_collection_changes_through_its_apps_actions() {
        let w = world();
        let reply = Session::new().run(
            &w.rt,
            &agent(),
            &format!("let l = {}\nl.items = l.items", w.list_literal()),
        );
        let lines = rejected(&reply);
        assert!(lines.contains("'s actions"), "{lines}");
    }
}
