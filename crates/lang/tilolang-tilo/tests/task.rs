//! An agent task's programs (`tilolang_tilo::task`): run in a session of the task's own that
//! holds no documents, reading Notes and Todo through their apps and changing them only by
//! invoking each app's actions, which the apps (here two hosts behind an API) run.

use std::rc::Rc;

use serde_json::{Value as Json, json};
use tilolang_tilo::task::TaskSession;

use crate::fixtures::TASKS_SET;
use crate::task_api::Apps;

const NOTES_APP: &str = "so.tilo.notes";
const TODO_APP: &str = "so.tilo.tasks";

struct Task {
    session: TaskSession,
    api: Rc<Apps>,
}

/// The agent's task, as the author of the programs these tests run.
fn agent() -> tilolang_tilo::runtime::Actor {
    tilolang_tilo::runtime::Actor::from(tilolang_tilo::runtime::ids::MemberId::agent("task-1"))
}

/// Notes and Todo, each in a host of its own, and a task of them; the policy refuses the
/// actions `refused`.
fn task(refused: Vec<&'static str>) -> Task {
    let notes: &[(&str, &str)] = &[crate::fixtures::DISCUSSION, crate::fixtures::NOTES];
    let mut apps = Apps::new(&[notes, TASKS_SET]);
    apps.refused = refused;
    let api = Rc::new(apps);
    Task {
        session: api.task(),
        api,
    }
}

impl Task {
    /// What `source` answers as one step of the task: run with its go-ahead (`approved`), or
    /// previewed first and run when its person need not be asked, as an agent's program is.
    fn step(&self, source: &str, approved: bool) -> Json {
        if approved {
            return self.session.run(source, &agent());
        }
        let preview = self.session.preview(source, &agent());
        if preview["state"] == "preview" && preview["asks"] == false {
            return self.session.run(source, &agent());
        }
        preview
    }

    /// The one line `source` printed.
    fn said(&self, source: &str) -> String {
        let answer = self.step(source, false);
        assert_eq!(answer["state"], "done", "{answer:#}");
        answer["output"][0].as_str().unwrap().to_owned()
    }
}

fn seeded() -> Task {
    let task = task(Vec::new());
    for source in [
        "notes.retitle(notes.new_note(notes.notebook), \"Q3 plan\")",
        "notes.pin(notes.new_note(notes.notebook), true)",
        "tasks.new_task(tasks.tasks, title: \"milk\")\ntasks.new_task(tasks.tasks, title: \"bins\")",
    ] {
        let answer = task.step(source, true);
        assert_eq!(answer["state"], "done", "{source}: {answer:#}");
    }
    task.api.invoked.borrow_mut().clear();
    task
}

/// A program reads a resource the session publishes rather than one of an app's records — the
/// device's styles here, as `styles.installed` — in the same call it runs: the API answers the
/// reading the session's services publish, and the program goes on with it (the style ids a
/// person may switch to).
#[test]
fn a_program_reads_the_reading_the_session_publishes() {
    let task = task(Vec::new());
    let published = "tilo.styles";
    task.api.readings.borrow_mut().insert(
        published.to_owned(),
        json!({
            "offered": [
                {"style": "so.tilo.shell:strips", "package": "so.tilo.shell",
                 "name": "strips", "line": "The strips", "loads": true, "reason": ""},
                {"style": "so.tilo.shell:cards", "package": "so.tilo.shell",
                 "name": "cards", "line": "Cards", "loads": true, "reason": ""},
            ],
        }),
    );
    let said = task.said("for s in styles.installed.offered { print(s.style) }");
    assert_eq!(said, "so.tilo.shell:strips", "{said}");
    // One call: the reading was asked for, and it was asked for once.
    let asked = task.api.read_for.borrow().clone();
    assert_eq!(
        asked,
        [("tilo.styles".to_owned(), published.to_owned())],
        "the reading asked of the session once, within the program"
    );
    assert!(
        task.api.invoked.borrow().is_empty(),
        "nothing an app does: a reading is read"
    );
    // A second program asks again: what the session publishes may have moved.
    let again = task.said("print(styles.installed.offered.len())");
    assert_eq!(again, "2", "{again}");
    assert_eq!(
        task.api.read_for.borrow().len(),
        2,
        "the second program reads it again"
    );
}

/// A resource the session publishes nothing of reads as its type starts, never as a fault of the
/// program: a device without Wi-Fi publishes no networks, and a view draws it so.
#[test]
fn a_reading_the_session_has_none_of_reads_as_it_starts() {
    let task = task(Vec::new());
    let said = task.said("print(device.time.clock)");
    // No reading: the clock's text starts empty, and the program carries on.
    assert_eq!(said, "", "{said}");
    let answer = task.step("print(device.time.timer_paused)", false);
    assert_eq!(answer["state"], "done", "{answer:#}");
}

#[test]
fn one_program_reads_notes_and_todo() {
    let task = seeded();
    assert_eq!(
        task.said("print(notes.notebook.notes.len(), notes.notebook.notes.filter(n -> n.pinned).len(), tasks.tasks.items.len())"),
        "2 1 2"
    );
    let titles = task.step("tasks.tasks.items", false);
    let shown = titles["output"][0].as_str().unwrap();
    assert!(
        shown.contains("@tasks.Task:") && shown.contains("\"milk\""),
        "{shown}"
    );
    assert!(
        task.api.invoked.borrow().is_empty(),
        "reading invokes nothing"
    );
}

#[test]
fn a_programs_lets_stay_for_the_next() {
    let task = seeded();
    let first = task.step("let list = tasks.tasks", false);
    assert_eq!(first["state"], "done", "{first:#}");
    assert_eq!(first["lets"][0]["name"], "list", "{first:#}");
    assert_eq!(task.said("list.items.len()"), "2");
}

#[test]
fn changes_in_two_apps_are_invocations_of_each() {
    let task = seeded();
    let answer = task.step(
        "for n in notes.notebook.notes.filter(n -> n.pinned) {\n    notes.pin(n, false)\n}\ntasks.new_task(tasks.tasks, title: \"call Ada\")",
        false,
    );
    assert_eq!(answer["state"], "done", "{answer:#}");
    assert_eq!(answer["changed"], 2, "{answer:#}");
    assert_eq!(answer["apps"], json!([NOTES_APP, TODO_APP]), "{answer:#}");
    let invoked = task.api.invoked.borrow().clone();
    let actions: Vec<(&str, &str)> = invoked
        .iter()
        .map(|call| {
            (
                call["app"].as_str().unwrap(),
                call["action"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(actions.len(), 2, "{invoked:#?}");
    assert_eq!(actions[0].0, NOTES_APP);
    assert_eq!(actions[1].0, TODO_APP);
    assert!(
        invoked[0]["resource"]
            .as_str()
            .unwrap()
            .starts_with("tl:so.tilo.notes:"),
        "the action is invoked on its target: {invoked:#?}"
    );
    assert_eq!(
        task.said("notes.notebook.notes.filter(n -> n.pinned).len()"),
        "0"
    );
    assert_eq!(task.said("tasks.tasks.items.len()"), "3");
}

#[test]
fn an_assignment_is_the_resources_field_edit() {
    let task = seeded();
    let answer = task.step("for t in tasks.tasks.items {\n    t.done = true\n}", false);
    assert_eq!(answer["state"], "done", "{answer:#}");
    let invoked = task.api.invoked.borrow().clone();
    assert_eq!(invoked.len(), 2, "{invoked:#?}");
    assert!(
        invoked
            .iter()
            .all(|call| call["action"] == "edit-flag" && call["arguments"]["value"] == true),
        "{invoked:#?}"
    );
    assert_eq!(
        task.said("tasks.tasks.items.filter(t -> t.done).len()"),
        "2"
    );
}

#[test]
fn many_changes_are_previewed_and_sent_once_approved() {
    let task = seeded();
    let bulk = "for t in [\"a\", \"b\", \"c\", \"d\"] {\n    tasks.new_task(tasks.tasks, title: t)\n}\nfor n in notes.notebook.notes {\n    n.title = \"x\"\n}";
    let previewed = task.step(bulk, false);
    assert_eq!(previewed["state"], "preview", "{previewed:#}");
    assert_eq!(
        previewed["steps"].as_array().unwrap().len(),
        6,
        "{previewed:#}"
    );
    assert!(
        task.api.invoked.borrow().is_empty(),
        "a preview invokes nothing"
    );
    assert_eq!(task.said("tasks.tasks.items.len()"), "2");
    let applied = task.step(bulk, true);
    assert_eq!(applied["state"], "done", "{applied:#}");
    assert_eq!(applied["changed"], 6, "{applied:#}");
    assert_eq!(task.said("tasks.tasks.items.len()"), "6");
}

#[test]
fn a_destructive_action_is_previewed_first() {
    let task = seeded();
    let answer = task.step(
        "for t in tasks.tasks.items {\n    tasks.remove(tasks.tasks, t)\n}",
        false,
    );
    assert_eq!(answer["state"], "preview", "{answer:#}");
    assert_eq!(answer["steps"][0]["destructive"], true, "{answer:#}");
    assert_eq!(task.said("tasks.tasks.items.len()"), "2");
}

tilo_test_env::scenario! {
    fn a_program_stops_at_what_is_refused_or_cannot_be_read;

    phase a_refused_invocation_is_never_done;
    {
        let task = task(vec!["edit-text"]);
        let answer = task.step(
            "let n = notes.new_note(notes.notebook)\nn.title = \"x\"",
            true,
        );
        assert_eq!(answer["state"], "failed", "{answer:#}");
        assert!(
            answer["detail"].as_str().unwrap().contains("denied"),
            "{answer:#}"
        );
        let tried = task.step(
            "match try tasks.new_task(tasks.tasks, title: \"Read the letter\") { .ok(t) => print(t.title), .err(why) => print(why) }",
            true,
        );
        assert_eq!(tried["state"], "done", "{tried:#}");
        assert_eq!(tried["output"], json!(["Read the letter"]), "{tried:#}");
    }

    phase what_cannot_be_read_is_an_error_never_nothing;
    {
        let notes: &[(&str, &str)] = &[crate::fixtures::DISCUSSION, crate::fixtures::NOTES];
        let mut apps = Apps::new(&[notes, TASKS_SET]);
        apps.unreachable = vec![(TODO_APP, "todo is not open")];
        let api = Rc::new(apps);
        let session = api.task();
        for source in [
            "tasks.tasks.items.len()",
            "print(tasks.tasks.items.len())",
            "tasks.tasks.items",
            "tasks.tasks.items.filter(t -> t.done)",
            "tasks.tasks.items.filter(t -> t.done).len()",
        ] {
            let answer = session.preview(source, &agent());
            assert_eq!(answer["state"], "failed", "{source}: {answer:#}");
            assert!(
                answer["detail"]
                    .as_str()
                    .unwrap()
                    .contains("todo is not open"),
                "{source}: {answer:#}"
            );
        }
    }

    phase a_refused_action_stops_the_program_unless_it_tries;
    {
        let task = task(vec!["new_task"]);
        let answer = task.step(
            "tasks.new_task(tasks.tasks, title: \"milk\")\nprint(\"went on\")",
            true,
        );
        assert_eq!(answer["state"], "failed", "{answer:#}");
        assert!(
            answer["detail"].as_str().unwrap().contains("denied"),
            "{answer:#}"
        );
        assert!(
            answer["output"].as_array().unwrap().is_empty(),
            "{answer:#}"
        );
    }
}

#[test]
fn a_flag_argument_is_sent_as_a_flag() {
    let task = seeded();
    let answer = task.step(
        "notes.pin(notes.notebook.notes.filter(n -> n.title == \"Q3 plan\").single(), false)",
        true,
    );
    assert_eq!(answer["state"], "done", "{answer:#}");
    let pin = task.api.invoked.borrow().last().cloned().unwrap();
    assert_eq!(pin["arguments"]["pinned"], json!(false), "{pin:#}");
}

#[test]
fn a_resource_argument_is_the_catalogues_reference() {
    let task = seeded();
    let answer = task.step(
        "for t in tasks.tasks.items.take(1) {\n    tasks.remove(tasks.tasks, t)\n}",
        true,
    );
    assert_eq!(answer["state"], "done", "{answer:#}");
    let remove = task.api.invoked.borrow().last().cloned().unwrap();
    assert_eq!(
        remove["arguments"]["t"]["kind"],
        json!("tl.so.tilo.tasks.Task"),
        "{remove:#}"
    );
    assert_eq!(task.said("tasks.tasks.items.len()"), "1");
}

/// Every argument travels in the shape the action's published parameter declares, which the
/// broker binds before it delivers, and arrives as the value the program gave: a resource of a
/// union (a channel or a thread) by a reference naming which it is, an optional resource, rich
/// text.
#[test]
fn each_argument_is_sent_as_its_published_parameter_takes_it() {
    let chat: &[(&str, &str)] = &[crate::fixtures::CHAT];
    let api = Rc::new(Apps::new(&[chat, TASKS_SET]));
    let task = Task {
        session: api.task(),
        api,
    };
    let answer = task.step(
        "let ch = chat.new_channel(chat.conversations, \"launch\")\n\
         let first = chat.send(ch, RichText(\"The launch video is ready for review\"))\n\
         chat.send(ch, RichText(\"Watching it now\"), first)",
        true,
    );
    assert_eq!(answer["state"], "done", "{answer:#}");
    let send = task.api.invoked.borrow().last().cloned().unwrap();
    assert_eq!(
        send["arguments"]["to"]["kind"],
        json!("tl.so.tilo.chat.TextChannel"),
        "the union's member it is: {send:#}"
    );
    assert_eq!(
        send["arguments"]["reply_to"]["kind"],
        json!("tl.so.tilo.chat.Message"),
        "{send:#}"
    );
    assert_eq!(
        send["arguments"]["body"],
        json!("Watching it now"),
        "{send:#}"
    );
    assert_eq!(
        task.said(
            "print(ch.messages.len(), ch.messages.filter(m -> m.reply_to == .some(first)).len())"
        ),
        "2 1"
    );
}

/// "Make a note with the groceries: eggs, milk, bread" is one program: the note is made with its
/// title and its words by the action that makes it, and more words go under them by the note's
/// own action, as any caller writes a note's body.
#[test]
fn a_program_makes_a_note_with_its_words() {
    let task = task(Vec::new());
    let made = task.step(
        "let n = notes.new_note(notes.notebook, title: \"Groceries\", body: \"- eggs\\n- milk\")\nnotes.add_text(n, \"- bread\")",
        true,
    );
    assert_eq!(made["state"], "done", "{made:#}");
    let read = task.step(
        "for n in notes.notebook.notes.filter(n -> n.title == \"Groceries\") { print(read(n)) }",
        false,
    );
    assert_eq!(read["state"], "done", "{read:#}");
    let said = read["output"].to_string();
    for item in ["eggs", "milk", "bread"] {
        assert!(said.contains(item), "{item}: {read:#}");
    }
    assert!(
        said.find("milk") < said.find("bread"),
        "added words go under what the note says: {read:#}"
    );
}

/// A task moved to another day keeps its moment and its reminder at the same time of day:
/// "move the reminder to Friday" of a task due Wednesday at six, reminded at six, moves both.
#[test]
fn a_task_moved_to_another_day_takes_its_moment_and_reminder() {
    let task = task(Vec::new());
    let made = task.step(
        "tasks.new_task(tasks.tasks, title: \"Call mum\", due: Date(\"2026-11-18\"), at: Time(\"2026-11-18T17:00:00Z\"), remind: Time(\"2026-11-18T17:00:00Z\"))",
        true,
    );
    assert_eq!(made["state"], "done", "{made:#}");
    let moved = task.step(
        "for t in tasks.tasks.items.filter(t -> t.title == \"Call mum\") { tasks.schedule(t, Date(\"2026-11-20\")) }",
        true,
    );
    assert_eq!(moved["state"], "done", "{moved:#}");
    let said = task.said(
        "for t in tasks.tasks.items.filter(t -> t.title == \"Call mum\") { print(t.due, t.at, t.remind) }",
    );
    assert_eq!(
        said.matches("2026-11-20").count(),
        3,
        "the day, the moment and the reminder: {said}"
    );
    assert!(!said.contains("2026-11-18"), "{said}");

    // A day named in text ("due {t.due}") reads as the person reads their dates; until the
    // platform names it, it stays its stored text.
    let named_text = "for t in tasks.tasks.items.filter(t -> t.title == \"Call mum\") { print(\"due {t.due}\") }";
    let stored = task.said(named_text);
    assert_eq!(stored, "due 2026-11-20", "{stored}");
    task.session.set_sentence_by(|kind, value| match kind {
        "date" if value == "2026-11-20" => "20th November 2026".to_owned(),
        _ => value.to_owned(),
    });
    let named = task.said(named_text);
    assert_eq!(named, "due 20th November 2026", "{named}");
}

/// An action that sets what a document already holds changes nothing and is done, never a failure
/// of the program: "move the reminder to Friday" written as the day's move and then the reminder
/// set to the moment the move already gave it.
#[test]
fn an_action_that_changes_nothing_is_done() {
    let task = task(Vec::new());
    let made = task.step(
        "tasks.new_task(tasks.tasks, title: \"Call mum\", due: Date(\"2026-11-18\"), at: Time(\"2026-11-18T17:00:00Z\"), remind: Time(\"2026-11-18T17:00:00Z\"))",
        true,
    );
    assert_eq!(made["state"], "done", "{made:#}");
    let moved = task.step(
        "for t in tasks.tasks.items.filter(t -> t.title == \"Call mum\") {\n    tasks.schedule(t, Date(\"2026-11-20\"))\n    tasks.remind_at(t, Time(\"2026-11-20T17:00:00Z\"))\n}",
        true,
    );
    assert_eq!(moved["state"], "done", "{moved:#}");
}

/// "Move the reminder to Friday" of a task due Wednesday: a reminder on a day after the task is
/// due moves the task to that day, so the reminder and the day it is due move together.
#[test]
fn a_reminder_past_the_tasks_day_moves_the_task() {
    let task = task(Vec::new());
    let made = task.step(
        "tasks.new_task(tasks.tasks, title: \"Call mum\", due: Date(\"2026-11-18\"), remind: Time(\"2026-11-18T17:00:00Z\"))",
        true,
    );
    assert_eq!(made["state"], "done", "{made:#}");
    let moved = task.step(
        "for t in tasks.tasks.items.filter(t -> t.title == \"Call mum\") { tasks.remind_at(t, Time(\"2026-11-20T17:00:00Z\")) }",
        true,
    );
    assert_eq!(moved["state"], "done", "{moved:#}");
    let said = task.said(
        "for t in tasks.tasks.items.filter(t -> t.title == \"Call mum\") { print(t.due, t.remind) }",
    );
    assert_eq!(said.matches("2026-11-20").count(), 2, "{said}");
}

/// "What's on my plate today?" answered by `print(xs, fields: [.title, .due, .priority])`: the
/// answer a person reads (`person_output`) has no reference literals, leaves out a field that is
/// `none`, and writes a day in the person's words; the model keeps `output`, refs and all, for
/// its next program.
#[test]
fn a_programs_answer_reads_for_a_person_without_its_references() {
    let task = task(Vec::new());
    task.session.set_shown_by(|kind, value| {
        if kind == "date" && value == "2026-11-18" {
            return "18th November 2026".to_owned();
        }
        value.to_owned()
    });
    let answer = task.step(
        "tasks.new_task(tasks.tasks, title: \"Call mum\", due: Date(\"2026-11-18\"))\nprint(find(tasks.Task), fields: [.title, .due, .priority])",
        true,
    );
    assert_eq!(answer["state"], "done", "{answer:#}");
    let model = answer["output"][0].as_str().expect("a printed line");
    let person = answer["person_output"][0]
        .as_str()
        .expect("a printed line for a person");
    assert!(
        model.contains("@tasks.Task:"),
        "the model names the resource by its reference: {model}"
    );
    assert!(
        !person.contains("@tasks.Task:") && !person.contains("title:"),
        "a person reads the title and fields, not the reference or the field name: {person}"
    );
    assert_eq!(
        person, "Call mum  18th November 2026",
        "the day reads in the person's words, a field that is none left out"
    );
}
