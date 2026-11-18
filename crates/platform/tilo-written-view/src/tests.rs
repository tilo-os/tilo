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
struct TestWorkspace(Rc<RefCell<Held>>);

#[derive(Default)]
struct Held {
    records: BTreeMap<String, (u64, Json)>,
    refused: Vec<String>,
    /// Reads waiting for their record to change.
    waiting: Vec<String>,
    answers: Vec<(String, Read)>,
    invoked: Vec<(String, String, String, Map<String, Json>)>,
    /// Every app a read was asked of.
    asked: Vec<String>,
}

impl TestWorkspace {
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

impl Workspace for TestWorkspace {
    fn read(&mut self, app: &str, after: Option<String>) {
        let mut held = self.0.borrow_mut();
        held.asked.push(app.to_owned());
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

/// `source`, written by `author`, shown to Bo over `workspace`.
fn shown(source: &str, author: MemberId, workspace: &TestWorkspace) -> (Surface, Rc<State>) {
    shown_reading(source, author, &["tasks"], workspace)
}

/// [`shown`], with the apps its writer listed.
fn shown_reading(
    source: &str,
    author: MemberId,
    reads: &[&str],
    workspace: &TestWorkspace,
) -> (Surface, Rc<State>) {
    let state = Rc::new(State::default());
    let view = WrittenView {
        source: source.to_owned(),
        author: Actor::Member(author),
        viewer: "bo".into(),
        reads: reads.iter().map(|app| (*app).to_owned()).collect(),
    };
    let surface = Surface::open(&view, Box::new(workspace.clone()), Rc::clone(&state))
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
    let workspace = TestWorkspace::default();
    workspace.write(TASKS, tracker("Launch"));
    let (mut surface, state) = shown(LIST, ada(), &workspace);
    assert_eq!(texts(&surface), ["Launch"], "the view reads Tasks' record");

    // Someone renames the area in Tasks: the read the view left waiting answers, and the view
    // is drawn again over the new reading. Nothing reloads and no view is opened anew.
    workspace.write(TASKS, tracker("Launch day"));
    surface.turn();
    assert_eq!(texts(&surface), ["Launch day"]);

    assert!(state.refused().is_empty());
    assert!(workspace.invoked().is_empty(), "reading sends nothing");
}

#[test]
fn a_view_over_an_app_that_keeps_nothing_yet_shows_it_once_it_does() {
    let workspace = TestWorkspace::default();
    let (mut surface, _) = shown(LIST, ada(), &workspace);
    assert!(texts(&surface).is_empty(), "no record, nothing to list");

    workspace.write(TASKS, tracker("Launch"));
    surface.turn();
    assert_eq!(texts(&surface), ["Launch"]);
}

#[test]
fn a_view_someone_else_wrote_only_suggests_until_the_person_allows_that_one_thing() {
    let workspace = TestWorkspace::default();
    workspace.write(TASKS, tracker("Launch"));
    // Ada wrote it; Bo looks at it and has not trusted what she writes.
    let (mut surface, state) = shown(ACTS, ada(), &workspace);
    let button = surface.nodes("ui.Button").remove(0).0;

    surface.press(&button);
    assert!(
        workspace.invoked().is_empty(),
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
    assert!(workspace.invoked().is_empty());

    // Asked again and allowed: that one thing is sent, as Bo's, to Tasks' own verb.
    surface.press(&button);
    let again = state.suggestions();
    assert_eq!(again.len(), 1);
    state.allow(again[0].id);
    surface.turn();
    assert!(state.suggestions().is_empty());
    let invoked = workspace.invoked();
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
    assert_eq!(workspace.invoked().len(), 1);
}

#[test]
fn a_persons_own_view_acts_without_asking_them() {
    let workspace = TestWorkspace::default();
    workspace.write(TASKS, tracker("Launch"));
    let (mut surface, state) = shown(ACTS, bo(), &workspace);
    let button = surface.nodes("ui.Button").remove(0).0;

    surface.press(&button);
    assert!(state.suggestions().is_empty());
    assert_eq!(
        workspace.invoked().len(),
        1,
        "their own code does what they pressed"
    );
}

#[test]
fn a_record_the_person_may_not_read_is_said_refused_and_shows_nothing() {
    let workspace = TestWorkspace::default();
    workspace.write(TASKS, tracker("Launch"));
    workspace.refuse(TASKS);
    let (mut surface, state) = shown(LIST, ada(), &workspace);
    surface.turn();
    assert_eq!(state.refused(), ["Tasks"]);
    assert!(texts(&surface).is_empty(), "nothing of the record is shown");
}

#[test]
fn a_view_that_does_not_compile_says_so_where_it_stands_and_reads_nothing() {
    let workspace = TestWorkspace::default();
    workspace.write(TASKS, tracker("Launch"));
    let (surface, state) = shown("view broken() { text(nothing_here) }", ada(), &workspace);
    assert!(
        !surface.nodes("error").is_empty(),
        "the view's own fault is shown in its place"
    );
    assert!(state.suggestions().is_empty());
    assert!(workspace.invoked().is_empty());
}

/// A control whose label says one thing and whose action does another.
const MISLABELLED: &str = "view areas() {\n    for a in tasks.tracker.areas {\n        button(\"Keep\", on: tasks.delete_area(tasks.tracker, a.id))\n    }\n}";

#[test]
fn a_suggestion_says_the_action_and_what_it_is_given_never_what_the_control_is_called() {
    let workspace = TestWorkspace::default();
    workspace.write(TASKS, tracker("Launch"));
    let (mut surface, state) = shown(MISLABELLED, ada(), &workspace);
    let button = surface.nodes("ui.Button").remove(0).0;
    surface.press(&button);

    // What Bo is asked about is Tasks' own action by its own name, with everything it is given:
    // the label the view put on its control is nowhere in it.
    let waiting = state.suggestions();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].app, "Tasks");
    assert!(
        waiting[0].action.to_lowercase().contains("delete"),
        "the action is named as Tasks names it: {:?}",
        waiting[0].action
    );
    assert_ne!(waiting[0].action, "Keep");
    assert_eq!(
        waiting[0].arguments,
        [("area".to_owned(), "a1".to_owned())],
        "every argument, by the name the app's action gives it"
    );

    // What is sent once Bo allows it is exactly what was shown.
    state.allow(waiting[0].id);
    surface.turn();
    let invoked = workspace.invoked();
    assert_eq!(invoked.len(), 1);
    let sent: Vec<(String, String)> = invoked[0]
        .3
        .iter()
        .map(|(name, value)| {
            (
                name.clone(),
                value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned),
            )
        })
        .collect();
    assert_eq!(sent, waiting[0].arguments);
}

#[test]
fn a_view_is_given_the_apps_its_writer_listed_and_no_other() {
    let workspace = TestWorkspace::default();
    workspace.write(TASKS, tracker("Launch"));
    // The definition names Tasks, and its writer listed nothing: Tasks is unknown to it. It says
    // so where it stands, and nothing of Tasks is asked for.
    let (mut surface, state) = shown_reading(LIST, ada(), &[], &workspace);
    surface.turn();
    assert!(
        !surface.nodes("error").is_empty(),
        "the definition names what it was not given"
    );
    assert!(
        !texts(&surface).iter().any(|text| text.contains("Launch")),
        "nothing of Tasks is shown"
    );
    assert!(
        workspace.0.borrow().asked.is_empty(),
        "nothing was asked of an app the view was not given"
    );
    assert!(state.refused().is_empty());

    // Listed, it is read.
    let (surface, _) = shown_reading(LIST, ada(), &["tasks"], &workspace);
    assert_eq!(texts(&surface), ["Launch"]);
    let asked = workspace.0.borrow().asked.clone();
    assert!(
        !asked.is_empty() && asked.iter().all(|app| app == TASKS),
        "only Tasks was asked for: {asked:?}"
    );
}

/// Tasks' document of the workspace with the areas `areas`, each its name and how many people
/// it has, as Tasks stores it.
fn staffed(areas: &[(&str, usize)]) -> Json {
    let mut stored = Map::new();
    for (at, (name, people)) in areas.iter().enumerate() {
        let mut members = Map::new();
        for person in 0..*people {
            members.insert(
                format!("p{person}"),
                json!({ "k": format!("a{person}"), "v": { "name": format!("Person {person}"), "role": "member" } }),
            );
        }
        stored.insert(
            format!("a{at}"),
            json!({ "k": format!("a{at}"), "v": {
                "id": format!("a{at}"),
                "name": name,
                "kind": { "$variant": "project" },
                "people": members,
            } }),
        );
    }
    json!({ "areas": stored })
}

/// A bar for each of Tasks' areas, as tall as the area has people.
const CHART: &str = "view staffing() {\n    ui.BarChart(\n        [.bars(values: tasks.tracker.areas.map(a -> a.people.len().to_float()), label: \"People\")],\n        categories: tasks.tracker.areas.map(a -> a.name),\n    )\n}";

/// The parts of the whole: each area's share of the people.
const PIE: &str = "view shares() {\n    ui.PieChart(tasks.tracker.areas.map(a -> ui.Slice.slice(label: a.name, value: a.people.len().to_float())))\n}";

#[test]
fn a_chart_follows_the_resource_it_reads() {
    let workspace = TestWorkspace::default();
    workspace.write(TASKS, staffed(&[("Launch", 2), ("Research", 1)]));
    let (mut surface, state) = shown(CHART, ada(), &workspace);
    assert_eq!(
        surface.numbers("ui.BarChart", "bars"),
        [2.0, 1.0],
        "a bar for each area, as tall as it has people"
    );
    assert_eq!(
        surface.words("ui.BarChart", "categories"),
        ["Launch", "Research"]
    );
    assert_eq!(
        surface.drawn("CategoryChart"),
        1,
        "drawn by the toolkit's own chart"
    );
    let chart = surface.nodes("ui.BarChart").remove(0).0;

    // Two people join Research and an area is added in Tasks, by whoever: the read the view left
    // waiting answers and the bars follow. It is the chart that stood there, with new readings:
    // nothing reloads and no view is opened anew.
    workspace.write(
        TASKS,
        staffed(&[("Launch", 2), ("Research", 3), ("Sales", 4)]),
    );
    surface.turn();
    assert_eq!(surface.numbers("ui.BarChart", "bars"), [2.0, 3.0, 4.0]);
    assert_eq!(
        surface.words("ui.BarChart", "categories"),
        ["Launch", "Research", "Sales"]
    );
    assert_eq!(surface.nodes("ui.BarChart").remove(0).0, chart);
    assert_eq!(surface.drawn("CategoryChart"), 1);

    assert!(state.refused().is_empty());
    assert!(workspace.invoked().is_empty(), "a chart sends nothing");
}

#[test]
fn a_pie_follows_the_resource_it_reads() {
    let workspace = TestWorkspace::default();
    workspace.write(TASKS, staffed(&[("Launch", 2), ("Research", 1)]));
    let (mut surface, _) = shown(PIE, ada(), &workspace);
    assert_eq!(surface.numbers("ui.PieChart", "slices"), [2.0, 1.0]);
    assert_eq!(surface.drawn("PieChart"), 1);

    workspace.write(TASKS, staffed(&[("Launch", 2), ("Research", 5)]));
    surface.turn();
    assert_eq!(surface.numbers("ui.PieChart", "slices"), [2.0, 5.0]);
    assert_eq!(surface.drawn("PieChart"), 1);
}

#[test]
fn a_chart_of_a_resource_the_person_may_not_read_draws_none_of_it() {
    let workspace = TestWorkspace::default();
    workspace.write(TASKS, staffed(&[("Launch", 2)]));
    workspace.refuse(TASKS);
    let (mut surface, state) = shown(CHART, ada(), &workspace);
    surface.turn();
    assert_eq!(state.refused(), ["Tasks"]);
    assert!(
        surface.numbers("ui.BarChart", "bars").is_empty(),
        "no reading of what the person may not read reaches the chart"
    );
    assert!(surface.words("ui.BarChart", "categories").is_empty());
}

const WRITER: &str = "so.tilo.word-processor";

/// A view over the word processor, the last part of whose id is two words: a view writes it
/// with `_`, since no name in a view holds `-`.
const POEM: &str = "view poem() {\n    text(word_processor.document.body.text)\n}";
const ADDS: &str = "view adds() {\n    button(\"Add a line\", on: word_processor.append_paragraph(word_processor.document, \"More\"))\n}";

/// The word processor's document of the workspace with the body `lines`, in the form the
/// view's runtime reads a rich text from: its paragraphs in their order, under `rich`.
fn written(lines: &[&str]) -> Json {
    let blocks: Vec<Json> = lines
        .iter()
        .enumerate()
        .map(|(at, line)| {
            json!({
                "id": format!("b{at}"),
                "kind": "paragraph",
                "text": line,
                "marks": [],
                "attrs": {},
                "children": [],
            })
        })
        .collect();
    json!({ "body": { "rich": blocks } })
}

#[test]
fn a_view_reads_an_app_whose_id_holds_a_hyphen_by_the_name_a_view_can_write() {
    let workspace = TestWorkspace::default();
    workspace.write(WRITER, written(&["The tide comes in on quiet feet"]));
    let (mut surface, state) = shown_reading(POEM, ada(), &["word_processor"], &workspace);
    assert!(
        surface.nodes("error").is_empty(),
        "the view names the word processor: {:?}",
        texts(&surface)
    );
    assert_eq!(
        workspace.0.borrow().asked.first().map(String::as_str),
        Some(WRITER),
        "what the app keeps is asked of it under the app's own id"
    );
    assert_eq!(texts(&surface), ["The tide comes in on quiet feet"]);

    // A line is written in the word processor: the view shows both, in their order.
    workspace.write(
        WRITER,
        written(&[
            "The tide comes in on quiet feet",
            "and leaves again before the light",
        ]),
    );
    surface.turn();
    assert_eq!(
        texts(&surface),
        ["The tide comes in on quiet feet\nand leaves again before the light"]
    );
    assert!(state.refused().is_empty());
}

#[test]
fn what_such_a_view_asks_is_sent_to_the_app_under_its_own_id() {
    let workspace = TestWorkspace::default();
    let (mut surface, state) = shown_reading(ADDS, bo(), &["word_processor"], &workspace);
    let buttons = surface.nodes("ui.Button");
    assert_eq!(buttons.len(), 1, "the view shows its control");
    surface.press(&buttons[0].0);
    assert!(state.suggestions().is_empty(), "Bo's own view acts");
    let invoked = workspace.invoked();
    assert_eq!(invoked.len(), 1);
    let (app, resource, verb, arguments) = &invoked[0];
    assert_eq!(
        (app.as_str(), resource.as_str(), verb.as_str()),
        (WRITER, "word-processor.document", "append-paragraph")
    );
    assert_eq!(arguments.get("text"), Some(&json!("More")));
}
