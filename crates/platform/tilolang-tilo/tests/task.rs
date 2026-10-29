//! An agent task's programs (`tilolang_tilo::task`): run in a session of the task's own that
//! holds no records, reading Notes and Todo through their apps and changing them only by
//! invoking each app's actions, which the apps (here two hosts behind a door) run.

use std::cell::RefCell;
use std::rc::Rc;

use automerge::AutoCommit;
use serde_json::{Value as Json, json};
use tilolang_runtime::Principal;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::host::Host;
use tilolang_tilo::publish::{PlanRequest, edit_action, resource_id};
use tilolang_tilo::task::{Answered, TaskDoor, TaskSession};

use crate::fixtures::Replica;

const NOTES: (&str, &str) = (
    include_str!("../../../../apps/tl/notes/app.tl"),
    include_str!("../../../../apps/tl/notes/app.ids"),
);
const TODO: (&str, &str) = (
    include_str!("../../../../apps/tl/todo/app.tl"),
    include_str!("../../../../apps/tl/todo/app.ids"),
);
const NOTES_APP: &str = "so.tilo.lang.notes";
const TODO_APP: &str = "so.tilo.lang.todo";

fn compiled((source, ids): (&str, &str)) -> tilolang::Compiled {
    let lock = tilolang::IdsLock::parse(ids).unwrap();
    tilolang::compile_package(source, &lock, &[]).unwrap()
}

/// One app as the workspace runs it: its own room record and host.
fn app(compiled: &tilolang::Compiled) -> Replica {
    let doc = AutoCommit::new();
    let host = Host::new(&doc, Principal::new("ada"), || None).unwrap();
    host.load_compiled(compiled).unwrap();
    host.root_of(&compiled.meta.app).unwrap();
    let mut replica = Replica {
        name: "ada".to_owned(),
        doc,
        host,
        log: Vec::new(),
    };
    replica.commit();
    replica
}

/// The task's door: each app answers its reads and runs what the task invokes, as the broker
/// delivers it; every call is recorded.
struct Door {
    notes: RefCell<Replica>,
    todo: RefCell<Replica>,
    invoked: RefCell<Vec<Json>>,
    /// Actions whose invocations the policy refuses.
    refused: Vec<&'static str>,
}

impl Door {
    fn app(&self, app: &str) -> &RefCell<Replica> {
        match app {
            NOTES_APP => &self.notes,
            TODO_APP => &self.todo,
            other => panic!("no app {other}"),
        }
    }
}

impl TaskDoor for Door {
    fn read(&self, app: &str, item: &str) -> Result<Json, String> {
        self.app(app).borrow().host.read_values(item)
    }

    fn invoke(&self, call: &Json) -> Answered {
        self.invoked.borrow_mut().push(call.clone());
        let app = call["app"].as_str().unwrap();
        let action = call["action"].as_str().unwrap();
        if self.refused.contains(&action) {
            return Answered::Refused(format!("{action}: denied by the workspace's policy"));
        }
        let mut replica = self.app(app).borrow_mut();
        let prefix = resource_id(app, "");
        let resource = call["resource"].as_str().unwrap().strip_prefix(&prefix);
        let arguments = call["arguments"].as_object().unwrap();
        let planned = if action == "edit-text" || action == "edit-flag" {
            let package = replica.host.package(app).unwrap();
            let id = resource.unwrap();
            let ty = replica.host.read_values(id).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_owned();
            let declared = package.meta.types.iter().find(|t| t.id == ty).unwrap();
            let field = package
                .meta
                .types
                .iter()
                .flat_map(|t| &t.fields)
                .find(|f| f.id == arguments["field"])
                .unwrap();
            edit_action(
                &package.meta,
                &declared.name,
                id,
                &field.id,
                &arguments["value"],
            )
        } else {
            replica.host.plan(&PlanRequest {
                app,
                kind: "",
                action_id: action,
                resource,
                args: arguments,
                caller: None,
            })
        };
        match planned {
            Ok(planned) => {
                replica.apply(&planned);
                let returned = replica
                    .host
                    .take_returned(action)
                    .map(|made| resource_id(app, &made.id_string()));
                Answered::Done(
                    json!({"value": null, "resources": returned.into_iter().collect::<Vec<_>>()}),
                )
            }
            Err(why) => Answered::Failed(why),
        }
    }
}

struct Task {
    session: TaskSession,
    door: Rc<Door>,
}

fn task(refused: Vec<&'static str>) -> Task {
    let (notes, todo) = (compiled(NOTES), compiled(TODO));
    let door = Rc::new(Door {
        notes: RefCell::new(app(&notes)),
        todo: RefCell::new(app(&todo)),
        invoked: RefCell::default(),
        refused,
    });
    let bundles: Vec<Bundle> = [notes, todo]
        .iter()
        .map(|compiled| Bundle {
            wasm: compiled.wasm.clone(),
            meta: compiled.meta.clone(),
            ids: String::new(),
            imports: Vec::new(),
        })
        .collect();
    let session =
        TaskSession::new("task-1", &(Rc::clone(&door) as Rc<dyn TaskDoor>), &bundles).unwrap();
    Task { session, door }
}

impl Task {
    fn run(&self, source: &str, confirm: bool) -> Json {
        self.session.run(source, confirm)
    }

    /// The one line `source` printed.
    fn said(&self, source: &str) -> String {
        let answer = self.run(source, false);
        assert_eq!(answer["state"], "done", "{answer:#}");
        answer["output"][0].as_str().unwrap().to_owned()
    }
}

fn seeded() -> Task {
    let task = task(Vec::new());
    for source in [
        "notes.retitle(notes.new_note(notes.notes), \"Q3 plan\")",
        "notes.pin(notes.new_note(notes.notes), true)",
        "todo.add(todo.todos, \"milk\")\ntodo.add(todo.todos, \"bins\")",
    ] {
        let answer = task.run(source, true);
        assert_eq!(answer["state"], "done", "{source}: {answer:#}");
    }
    task.door.invoked.borrow_mut().clear();
    task
}

#[test]
fn one_program_reads_notes_and_todo() {
    let task = seeded();
    assert_eq!(
        task.said("print(notes.notes.len(), notes.notes.filter(n -> n.pinned).len(), todo.todos.items.len())"),
        "2 1 2"
    );
    let titles = task.run("todo.todos.items", false);
    let shown = titles["output"][0].as_str().unwrap();
    assert!(
        shown.contains("@todo.Todo:") && shown.contains("\"milk\""),
        "{shown}"
    );
    assert!(
        task.door.invoked.borrow().is_empty(),
        "reading invokes nothing"
    );
}

#[test]
fn a_programs_lets_stay_for_the_next() {
    let task = seeded();
    let first = task.run("let list = todo.todos", false);
    assert_eq!(first["state"], "done", "{first:#}");
    assert_eq!(first["lets"][0]["name"], "list", "{first:#}");
    assert_eq!(task.said("list.items.len()"), "2");
}

#[test]
fn changes_in_two_apps_are_invocations_of_each() {
    let task = seeded();
    let answer = task.run(
        "for n in notes.notes.filter(n -> n.pinned) {\n    notes.pin(n, false)\n}\ntodo.add(todo.todos, \"call Ada\")",
        false,
    );
    assert_eq!(answer["state"], "done", "{answer:#}");
    assert_eq!(answer["changed"], 2, "{answer:#}");
    assert_eq!(answer["apps"], json!([NOTES_APP, TODO_APP]), "{answer:#}");
    let invoked = task.door.invoked.borrow().clone();
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
            .starts_with("tl:so.tilo.lang.notes:"),
        "the action is invoked on its subject: {invoked:#?}"
    );
    assert_eq!(task.said("notes.notes.filter(n -> n.pinned).len()"), "0");
    assert_eq!(task.said("todo.todos.items.len()"), "3");
}

#[test]
fn an_assignment_is_the_resources_field_edit() {
    let task = seeded();
    let answer = task.run("for t in todo.todos.items {\n    t.done = true\n}", false);
    assert_eq!(answer["state"], "done", "{answer:#}");
    let invoked = task.door.invoked.borrow().clone();
    assert_eq!(invoked.len(), 2, "{invoked:#?}");
    assert!(
        invoked
            .iter()
            .all(|call| call["action"] == "edit-flag" && call["arguments"]["value"] == true),
        "{invoked:#?}"
    );
    assert_eq!(task.said("todo.todos.items.filter(t -> t.done).len()"), "2");
}

#[test]
fn many_changes_are_previewed_and_sent_on_confirm() {
    let task = seeded();
    let bulk = "for t in [\"a\", \"b\", \"c\", \"d\"] {\n    todo.add(todo.todos, t)\n}\nfor n in notes.notes {\n    n.title = \"x\"\n}";
    let previewed = task.run(bulk, false);
    assert_eq!(previewed["state"], "preview", "{previewed:#}");
    assert_eq!(
        previewed["steps"].as_array().unwrap().len(),
        6,
        "{previewed:#}"
    );
    assert!(
        task.door.invoked.borrow().is_empty(),
        "a preview invokes nothing"
    );
    assert_eq!(task.said("todo.todos.items.len()"), "2");
    let applied = task.run(bulk, true);
    assert_eq!(applied["state"], "done", "{applied:#}");
    assert_eq!(applied["changed"], 6, "{applied:#}");
    assert_eq!(task.said("todo.todos.items.len()"), "6");
}

#[test]
fn a_destructive_action_is_previewed_first() {
    let task = seeded();
    let answer = task.run(
        "for t in todo.todos.items {\n    todo.remove(todo.todos, t)\n}",
        false,
    );
    assert_eq!(answer["state"], "preview", "{answer:#}");
    assert_eq!(answer["steps"][0]["destructive"], true, "{answer:#}");
    assert_eq!(task.said("todo.todos.items.len()"), "2");
}

#[test]
fn a_refused_invocation_is_never_done() {
    let task = task(vec!["edit-text"]);
    let answer = task.run("let n = notes.new_note(notes.notes)\nn.title = \"x\"", true);
    assert_eq!(answer["state"], "failed", "{answer:#}");
    assert!(
        answer["detail"].as_str().unwrap().contains("denied"),
        "{answer:#}"
    );
    let tried = task.run(
        "match try todo.rename(todo.todos, \"Saturday\") { .ok => print(\"renamed\"), .err(why) => print(why) }",
        true,
    );
    assert_eq!(tried["state"], "done", "{tried:#}");
    assert_eq!(tried["output"], json!(["renamed"]), "{tried:#}");
}
