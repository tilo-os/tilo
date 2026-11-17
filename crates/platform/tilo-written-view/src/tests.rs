//! A written view over a workspace: what it shows, that it follows the records it reads, and that
//! what it does is decided before anything is sent.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::{Map, Value as Json, json};
use tilo_actions::{Actor, MemberId};
use ui::written_view::{ViewState as _, WrittenView};

use crate::state::State;
use crate::surface::Surface;
use crate::workspace::{Read, Workspace};

const TASKS: &str = "so.tilo.tasks";

/// A workspace as a test holds it: each app's record as the app stores it, what the person may
/// not read, and everything a view sent.
#[derive(Clone, Default)]
struct Desk(Rc<RefCell<Held>>);

#[derive(Default)]
struct Held {
    records: BTreeMap<String, (u64, Json)>,
    refused: Vec<String>,
    /// Reads waiting for their record to change.
    waiting: Vec<String>,
    answers: Vec<(String, Read)>,
    invoked: Vec<(String, String, String, Map<String, Json>)>,
}

impl Desk {
    /// The app `app` wrote `record`: every read waiting on it answers.
    fn write(&self, app: &str, record: Json) {
        let mut held = self.0.borrow_mut();
        let revision = held
            .records
            .get(app)
            .map_or(1, |(revision, _)| revision.saturating_add(1));
        held.records
            .insert(app.to_owned(), (revision, record.clone()));
        if let Some(at) = held.waiting.iter().position(|waiting| waiting == app) {
            held.waiting.remove(at);
            held.answers.push((
                app.to_owned(),
                Read::Record {
                    revision: revision.to_string(),
                    value: record,
                },
            ));
        }
    }

    fn refuse(&self, app: &str) {
        self.0.borrow_mut().refused.push(app.to_owned());
    }

    fn invoked(&self) -> Vec<(String, String, String, Map<String, Json>)> {
        self.0.borrow().invoked.clone()
    }
}

impl Workspace for Desk {
    fn read(&mut self, app: &str, after: Option<String>) {
        let mut held = self.0.borrow_mut();
        if held.refused.iter().any(|refused| refused == app) {
            held.answers.push((app.to_owned(), Read::Refused));
            return;
        }
        let answer = match held.records.get(app) {
            Some((revision, value)) if after.as_deref() != Some(&revision.to_string()) => {
                Some(Read::Record {
                    revision: revision.to_string(),
                    value: value.clone(),
                })
            }
            None if after.is_none() => Some(Read::Absent {
                revision: "0".to_owned(),
            }),
            // The record stands as the reader last saw it, or there is none yet: the read waits.
            _ => None,
        };
        match answer {
            Some(answer) => held.answers.push((app.to_owned(), answer)),
            None => held.waiting.push(app.to_owned()),
        }
    }

    fn take_read(&mut self) -> Vec<(String, Read)> {
        std::mem::take(&mut self.0.borrow_mut().answers)
    }

    fn invoke(&mut self, app: &str, resource: &str, verb: &str, arguments: Map<String, Json>) {
        self.0.borrow_mut().invoked.push((
            app.to_owned(),
            resource.to_owned(),
            verb.to_owned(),
            arguments,
        ));
    }

    fn take_failed(&mut self) -> Vec<String> {
        Vec::new()
    }
}

/// Tasks' record of the workspace with one area named `name`, as Tasks stores it: a collection
/// is its items by id, each under its place.
fn tracker(name: &str) -> Json {
    json!({
        "areas": {
            "a1": { "k": "a0", "v": {
                "id": "a1",
                "name": name,
                "kind": { "$variant": "project" },
                "people": {},
            } },
        },
    })
}

const LIST: &str =
    "view areas() {\n    for a in tasks.tracker.areas {\n        text(a.name)\n    }\n}";
const ACTS: &str = "view areas() {\n    for a in tasks.tracker.areas {\n        button(\"Delete\", on: tasks.delete_area(tasks.tracker, a.id))\n    }\n}";

fn ada() -> MemberId {
    MemberId::person("ada")
}

fn bo() -> MemberId {
    MemberId::person("bo")
}

/// `source`, written by `author`, shown to Bo over `desk`.
fn shown(source: &str, author: MemberId, desk: &Desk) -> (Surface, Rc<State>) {
    let state = Rc::new(State::default());
    let view = WrittenView {
        source: source.to_owned(),
        author: Actor::Member(author),
        viewer: "bo".into(),
    };
    let surface = Surface::open(&view, Box::new(desk.clone()), Rc::clone(&state))
        .unwrap_or_else(|why| panic!("the view opens: {why}"));
    (surface, state)
}

fn texts(surface: &Surface) -> Vec<String> {
    surface
        .nodes("ui.Text")
        .into_iter()
        .filter_map(|(_, text)| text)
        .collect()
}

#[test]
fn a_view_shows_what_an_app_keeps_and_follows_it_as_it_changes() {
    let desk = Desk::default();
    desk.write(TASKS, tracker("Launch"));
    let (mut surface, state) = shown(LIST, ada(), &desk);
    assert_eq!(texts(&surface), ["Launch"], "the view reads Tasks' record");

    // Someone renames the area in Tasks: the read the view left waiting answers, and the view
    // is drawn again over the new reading. Nothing reloads and no view is opened anew.
    desk.write(TASKS, tracker("Launch day"));
    surface.turn();
    assert_eq!(texts(&surface), ["Launch day"]);

    assert!(state.refused().is_empty());
    assert!(desk.invoked().is_empty(), "reading sends nothing");
}

#[test]
fn a_view_over_an_app_that_keeps_nothing_yet_shows_it_once_it_does() {
    let desk = Desk::default();
    let (mut surface, _) = shown(LIST, ada(), &desk);
    assert!(texts(&surface).is_empty(), "no record, nothing to list");

    desk.write(TASKS, tracker("Launch"));
    surface.turn();
    assert_eq!(texts(&surface), ["Launch"]);
}

#[test]
fn a_view_someone_else_wrote_only_suggests_until_the_person_allows_that_one_thing() {
    let desk = Desk::default();
    desk.write(TASKS, tracker("Launch"));
    // Ada wrote it; Bo looks at it and has not trusted what she writes.
    let (mut surface, state) = shown(ACTS, ada(), &desk);
    let button = surface.nodes("ui.Button").remove(0).0;

    surface.press(&button);
    assert!(
        desk.invoked().is_empty(),
        "nothing is sent before Bo allows it"
    );
    let waiting = state.suggestions();
    assert_eq!(waiting.len(), 1, "what the view asked waits for Bo");
    assert_eq!(waiting[0].app, "Tasks");
    assert_eq!(
        waiting[0].by,
        Actor::Member(ada()),
        "and names who wrote the view"
    );

    // Dismissed: gone, and still nothing sent.
    state.dismiss(waiting[0].id);
    surface.turn();
    assert!(state.suggestions().is_empty());
    assert!(desk.invoked().is_empty());

    // Asked again and allowed: that one thing is sent, as Bo's, to Tasks' own verb.
    surface.press(&button);
    let again = state.suggestions();
    assert_eq!(again.len(), 1);
    state.allow(again[0].id);
    surface.turn();
    assert!(state.suggestions().is_empty());
    let invoked = desk.invoked();
    assert_eq!(invoked.len(), 1);
    let (app, resource, verb, arguments) = &invoked[0];
    assert_eq!(
        (app.as_str(), resource.as_str(), verb.as_str()),
        (TASKS, "tasks.tracker", "delete-area")
    );
    assert_eq!(arguments.get("area"), Some(&json!("a1")));

    // Allowing one thing allowed nothing else: the next press waits again.
    surface.press(&button);
    assert_eq!(state.suggestions().len(), 1);
    assert_eq!(desk.invoked().len(), 1);
}

#[test]
fn a_persons_own_view_acts_without_asking_them() {
    let desk = Desk::default();
    desk.write(TASKS, tracker("Launch"));
    let (mut surface, state) = shown(ACTS, bo(), &desk);
    let button = surface.nodes("ui.Button").remove(0).0;

    surface.press(&button);
    assert!(state.suggestions().is_empty());
    assert_eq!(
        desk.invoked().len(),
        1,
        "their own code does what they pressed"
    );
}

#[test]
fn a_record_the_person_may_not_read_is_said_refused_and_shows_nothing() {
    let desk = Desk::default();
    desk.write(TASKS, tracker("Launch"));
    desk.refuse(TASKS);
    let (mut surface, state) = shown(LIST, ada(), &desk);
    surface.turn();
    assert_eq!(state.refused(), ["Tasks"]);
    assert!(texts(&surface).is_empty(), "nothing of the record is shown");
}

#[test]
fn a_view_that_does_not_compile_says_so_where_it_stands_and_reads_nothing() {
    let desk = Desk::default();
    desk.write(TASKS, tracker("Launch"));
    let (surface, state) = shown("view broken() { text(nothing_here) }", ada(), &desk);
    assert!(
        !surface.nodes("error").is_empty(),
        "the view's own fault is shown in its place"
    );
    assert!(state.suggestions().is_empty());
    assert!(desk.invoked().is_empty());
}
