//! Activity end to end: the history paged toward its oldest change and searched past the first
//! page, a chrome verb's ask opening the window at its place, a range compared before and
//! after, the agent session a range is with each record's own undo, and a brief opening its own
//! changes.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value as Json, json};
use tilolang::IdsLock;
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

const SOURCE: &str = include_str!("../../../../apps/tl/activity/app.tl");

fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// The activity's own reading: the feed and summary the window opens on.
fn activity() -> ResourceRef {
    ResourceRef::parse_id("workspace.activity", "so.tilo.history#Activity")
        .expect("the activity's address")
}

/// A page of the feed, keyed by its filters and how many of the newest changes it holds.
fn feed(limit: usize) -> ResourceRef {
    ResourceRef::parse_id(
        &format!("workspace.activity/feed~this~all-time~-~{limit}"),
        "so.tilo.history#FeedReading",
    )
    .expect("a page's address")
}

/// One range's detail.
fn range(from: &str, to: &str) -> ResourceRef {
    ResourceRef::parse_id(
        &format!("workspace.activity/range~{from}~{to}"),
        "so.tilo.history#RangeDetail",
    )
    .expect("a range's address")
}

/// One range's comparison.
fn compare(from: &str, to: &str) -> ResourceRef {
    ResourceRef::parse_id(
        &format!("workspace.activity/compare~{from}~{to}"),
        "so.tilo.history#RangeDetail",
    )
    .expect("a comparison's address")
}

/// A page of the agent sessions, keyed by how many of each session's changes it holds: what a
/// window showing one session's changes reads.
fn sessions(pages: usize) -> ResourceRef {
    ResourceRef::parse_id(
        &format!("workspace.activity/sessions~this~{pages}"),
        "so.tilo.history#Sessions",
    )
    .expect("a sessions page's address")
}

/// A change a session still leaves in place, as its card reads it.
fn still_in_effect(summary: &str, at: &str, step: &str) -> Json {
    json!({
        "range": {"from": at, "to": at},
        "step": step,
        "at_ms": 1_700_000_010_000_i64,
        "summary": summary,
        "app_id": "so.tilo.lang.notes",
        "record": "note-1",
        "target": {"id": "note-1", "kind": "note", "label": "Plan",
                   "app_id": "so.tilo.lang.notes", "app_label": "Notes"},
        "undo_label": "Undo this step",
        "undo_verb": "undo-entry",
    })
}

/// The one agent session the tests show, its changes still in effect `in_effect` and of `total`
/// it holds.
fn still_standing(in_effect: Json, total: usize) -> Json {
    json!({
        "id": "run-1",
        "agent": {"name": "Ada's agent", "rgb": [90, 120, 200], "identity": "agent",
                  "relation": "software", "on_behalf_of": "Ada", "for_reader": false,
                  "task": null},
        "workspace": "Home",
        "instruction": "Tidy the notes",
        "decision": "The notes were out of order.",
        "started_ms": 1_700_000_000_000_i64,
        "last_ms": 1_700_000_060_000_i64,
        "range": {"from": "e010", "to": "e020"},
        "actions": [],
        "in_effect": in_effect,
        "changes_total": total,
        "undo_blocked": null,
        "now": "",
        "now_kind": "",
    })
}

/// The sessions a page reads, of the history that holds them.
fn sessions_page(sessions: Json) -> String {
    json!({
        "sessions": sessions,
        "total": 1,
        "pending": false,
        "mutations": true,
    })
    .to_string()
}

fn change(id: &str, summary: &str) -> Json {
    json!({
        "id": id,
        "author": {"name": "Ada", "rgb": [200, 120, 90], "identity": "ada", "relation": "reader",
                   "on_behalf_of": null, "for_reader": true, "task": null},
        "kind": "text",
        "at_ms": 1_700_000_000_000_i64,
        "summary": summary,
        "made_in": "Notes",
        "resource": null,
        "group": null,
        "redacted": false,
        "refused": [],
    })
}

/// `count` changes, newest first: `Change 0` newest, `Change <count - 1>` oldest.
fn changes(count: usize) -> Vec<Json> {
    (0..count)
        .map(|at| change(&format!("e{at:03}"), &format!("Change {at}")))
        .collect()
}

/// A page of the feed holding `entries`, of `total` the history matched.
fn page(entries: Vec<Json>, total: i64) -> String {
    json!({
        "feed": {
            "workspace": "Home",
            "scope": "this-workspace",
            "total": total,
            "entries": entries,
            "groups": [],
            "people": ["Ada"],
            "lenses": [],
            "pending": false,
            "mutations": true,
        }
    })
    .to_string()
}

/// A range's detail with `group` as the work group the history named it.
fn detail(group: Option<&str>, offers: Json) -> String {
    json!({
        "detail": {
            "title": "Renamed a note",
            "provenance": "Ada renamed it",
            "authors": ["Ada"],
            "resources": [],
            "group": group,
            "group_label": null,
            "offers": offers,
            "changes": [],
            "comparison": null,
            "gone": false,
        }
    })
    .to_string()
}

#[derive(Clone, Default)]
struct Sent(Rc<RefCell<Vec<Invocation>>>);

impl Sent {
    /// The arguments of the first `action` asked, by parameter name.
    fn arg(&self, action: &str, name: &str) -> Option<Value> {
        self.0
            .borrow()
            .iter()
            .find_map(|invocation| match invocation {
                Invocation::Action {
                    name: asked, args, ..
                } if asked == action => args
                    .iter()
                    .find(|(arg, _)| arg == name)
                    .map(|(_, value)| value.clone()),
                _ => None,
            })
    }
}

impl Invoker for Sent {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Done(Value::Unit)
    }
}

struct Installed {
    host: Host,
    root: ResourceRef,
    view: String,
    sent: Sent,
}

impl Installed {
    fn new(activity_reading: &str) -> Self {
        let host = Host::new(me(), || None).expect("a host");
        // Only the packages the app `use`s: the whole platform carries the actions package the
        // host already runs, and a host loads one package once.
        let metas = tilolang_tilo::platform_apps::named_platform_packages(SOURCE)
            .expect("the platform's packages read");
        for meta in &metas {
            host.runtime()
                .load_foreign(meta.clone())
                .expect("the device is loaded");
        }
        let lock =
            IdsLock::parse(include_str!("../../../../apps/tl/activity/app.ids")).expect("the ids");
        let compiled = tilolang::compile_package(SOURCE, &lock, &metas)
            .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
        let package = host.load_compiled(&compiled).expect("the app loads");
        let view = package
            .meta
            .views
            .iter()
            .find(|view| view.name == "activity")
            .expect("the window view")
            .id
            .clone();
        let sent = Sent::default();
        host.runtime().set_foreign_invoker(sent.clone());
        host.runtime()
            .set_reading(&activity(), activity_reading)
            .expect("the activity reads");
        let root = host
            .runtime()
            .create_root("so.tilo.activity.Window")
            .expect("the app's root");
        Self {
            host,
            root,
            view,
            sent,
        }
    }

    fn read(&self, target: &ResourceRef, reading: &str) {
        self.host
            .runtime()
            .set_reading(target, reading)
            .expect("the reading is set");
    }

    fn window(&self) -> ViewInstance {
        self.host
            .runtime()
            .open_view(&me(), &self.view, vec![Value::resource(&self.root)])
            .expect("the window opens")
    }
}

/// Pick `value` for the window's `state`, as the person does, and let the view run again.
fn pick(window: &mut ViewInstance, state: &str, value: Value) {
    window.set_view_state(state, value);
    window.poll_patches().expect("the window runs again");
}

fn of_kind<'a>(tree: &'a UiNode, kind: &str) -> Vec<&'a UiNode> {
    let mut found = Vec::new();
    fn walk<'a>(node: &'a UiNode, kind: &str, found: &mut Vec<&'a UiNode>) {
        if node.kind == kind {
            found.push(node);
        }
        for child in &node.children {
            walk(child, kind, found);
        }
    }
    walk(tree, kind, &mut found);
    found
}

/// Every word the page shows: each node's text, its title and its label.
fn words(node: &UiNode, into: &mut Vec<String>) {
    for prop in ["value", "title", "label", "text", "message", "action"] {
        match node.props.get(prop) {
            Some(Value::Text(text)) => into.push(text.clone()),
            Some(Value::Opt(Some(held))) => {
                if let Value::Text(text) = &**held {
                    into.push(text.clone());
                }
            }
            _ => {}
        }
    }
    for child in &node.children {
        words(child, into);
    }
}

fn shown(view: &ViewInstance) -> Vec<String> {
    let mut out = Vec::new();
    words(view.tree(), &mut out);
    out
}

fn has(view: &ViewInstance, wanted: &str) -> bool {
    shown(view).iter().any(|text| text == wanted)
}

fn contains(view: &ViewInstance, wanted: &str) -> bool {
    shown(view).iter().any(|text| text.contains(wanted))
}

/// Press the button labelled `label`.
fn press(view: &mut ViewInstance, label: &str) {
    let button = of_kind(view.tree(), "ui.Button")
        .into_iter()
        .find(|node| {
            node.props
                .get("label")
                .is_some_and(|held| matches!(held, Value::Text(text) if text == label))
        })
        .unwrap_or_else(|| panic!("no `{label}` button in: {:?}", shown(view)))
        .key
        .clone();
    view.fire(&button, "press").expect("the press runs");
    view.poll_patches().expect("the window runs again");
}

/// A feed page the history reads, and the place that shows it.
fn open_history(app: &Installed) -> ViewInstance {
    let mut window = app.window();
    pick(&mut window, "place", Value::text("history"));
    window
}

#[test]
fn a_person_pages_toward_the_oldest_change_and_searches_past_the_first_page() {
    let app = Installed::new("{}");
    app.read(&feed(0), &page(changes(200), 500));
    let mut window = open_history(&app);
    assert!(contains(&window, "Change 199"), "{:?}", shown(&window));
    assert!(!contains(&window, "Change 250"), "the first page holds 200");
    assert!(has(&window, "Show more"), "older changes are reachable");

    // Showing more reads a larger page, and the changes only it holds appear.
    app.read(&feed(400), &page(changes(400), 500));
    press(&mut window, "Show more");
    assert!(contains(&window, "Change 250"), "{:?}", shown(&window));

    // The search reads further back than the page shown, and shows what it kept of what it
    // read.
    app.read(&feed(1600), &page(changes(500), 500));
    pick(&mut window, "found", Value::text("Change 250"));
    assert!(contains(&window, "Change 250"), "{:?}", shown(&window));
    assert!(
        has(&window, "1 of 500 changes"),
        "the search says what it kept: {:?}",
        shown(&window)
    );
    assert!(
        !has(&window, "Show more"),
        "the page holds every change the history matched"
    );
}

#[test]
fn a_chrome_verb_opens_the_window_at_its_focus_and_says_it_was_shown() {
    let app = Installed::new(
        r#"{"focus":{"serial":7,"view":"history","author":"Ada","lens":null,"entry":null}}"#,
    );
    app.read(&feed(0), &page(changes(3), 3));
    let mut window = app.window();
    let timer = of_kind(window.tree(), "ui.Timer")
        .first()
        .map(|node| node.key.clone())
        .unwrap_or_else(|| panic!("the window holds the ask's clock: {:?}", shown(&window)));
    window.fire(&timer, "tick").expect("the clock strikes");
    window.poll_patches().expect("the window takes the ask");
    assert_eq!(
        window.view_state("place"),
        Some(Value::text("history")),
        "the ask opens its place"
    );
    assert_eq!(window.view_state("writer"), Some(Value::text("Ada")));
    assert_eq!(
        app.sent.arg("focused", "serial"),
        Some(Value::Int(7)),
        "the session is told the ask was shown"
    );
    assert!(
        of_kind(window.tree(), "ui.Timer").is_empty(),
        "the ask is taken once"
    );
}

#[test]
fn a_ranges_agent_work_lists_the_changes_it_still_leaves_in_place_and_undoes_one() {
    let app = Installed::new("{}");
    let standing = json!([
        still_in_effect("Renamed a note", "e020", "aa11"),
        still_in_effect("Moved a paragraph", "e010", "bb22"),
    ]);
    app.read(&feed(0), &page(changes(3), 3));
    app.read(&range("e010", "e020"), &detail(None, json!([])));
    app.read(
        &sessions(20),
        &sessions_page(json!([still_standing(standing.clone(), 2)])),
    );
    let mut window = open_history(&app);
    window.set_view_state("first", Value::text("e010"));
    pick(&mut window, "last", Value::text("e020"));
    for wanted in [
        "Agent task",
        "Why this work",
        "Tidy the notes",
        "Started by Ada",
        "Decision",
        "The notes were out of order.",
        "Changes still in effect",
        "Renamed a note",
        "Moved a paragraph",
        "Undo this step",
    ] {
        assert!(has(&window, wanted), "{wanted} shows: {:?}", shown(&window));
    }
    // Each row says where the change landed: the app, and the thing it changed.
    let rows = of_kind(window.tree(), "ui.ListRow");
    assert!(
        rows.iter().any(|row| {
            matches!(row.props.get("title"), Some(Value::Text(text)) if text == "Renamed a note")
                && matches!(row.props.get("secondary"), Some(Value::Text(text))
                    if text == "Notes · Plan")
        }),
        "the row says where it landed: {:?}",
        rows.iter()
            .map(|row| (row.props.get("title"), row.props.get("secondary")))
            .collect::<Vec<_>>()
    );

    // Each change's undo names that one change, through the system's undo, and its link opens
    // what it changed.
    press(&mut window, "Undo this step");
    assert_eq!(
        app.sent.arg("undo-record", "app_id"),
        Some(Value::text("so.tilo.lang.notes")),
        "the record's own app is named"
    );
    assert_eq!(
        app.sent.arg("undo-record", "step"),
        Some(Value::Opt(Some(Box::new(Value::text("aa11"))))),
        "and the one change it takes back"
    );
    press(&mut window, "Plan");
    assert_eq!(
        app.sent.arg("open-change", "id"),
        Some(Value::text("note-1")),
        "the change's resource opens where it is"
    );

    // A session that ran long lists a page, and showing more reads a larger one.
    app.read(
        &sessions(20),
        &sessions_page(json!([still_standing(standing, 30)])),
    );
    window.poll_patches().expect("the window runs again");
    assert!(has(&window, "Show more"), "{:?}", shown(&window));
    app.read(
        &sessions(40),
        &sessions_page(json!([still_standing(
            json!([still_in_effect("Moved a paragraph", "e010", "bb22")]),
            1,
        )])),
    );
    press(&mut window, "Show more");
    assert!(
        !has(&window, "Show more"),
        "the page holds every change still in effect: {:?}",
        shown(&window)
    );
}

#[test]
fn a_range_compares_the_workspace_either_side_of_it() {
    let app = Installed::new("{}");
    app.read(&feed(0), &page(changes(3), 3));
    app.read(
        &range("e010", "e020"),
        &detail(None, json!([{"verb": "detail", "label": "Compare"}])),
    );
    app.read(
        &compare("e010", "e020"),
        &json!({
            "detail": {
                "title": "Renamed a note",
                "provenance": "Ada renamed it",
                "authors": ["Ada"],
                "resources": [],
                "group": null,
                "group_label": null,
                "offers": [],
                "changes": [],
                "comparison": "Added a page\nRemoved a note",
                "gone": false,
            }
        })
        .to_string(),
    );
    let mut window = open_history(&app);
    window.set_view_state("first", Value::text("e010"));
    pick(&mut window, "last", Value::text("e020"));
    assert!(has(&window, "Compare"), "{:?}", shown(&window));
    assert!(
        !has(&window, "Added a page"),
        "nothing compares until asked"
    );
    press(&mut window, "Compare");
    assert!(contains(&window, "Added a page"), "{:?}", shown(&window));
    assert!(contains(&window, "Removed a note"), "{:?}", shown(&window));
}

#[test]
fn a_brief_opens_its_own_changes_in_history() {
    let app = Installed::new(
        &json!({
            "summary": {
                "workspace": "Home",
                "away": true,
                "pending": false,
                "sources_available": false,
                "briefs": [{
                    "title": "Tidied the notes",
                    "detail": "Renamed three notes.",
                    "authors": [],
                    "apps": ["Notes"],
                    "resources": [],
                    "changes": 3,
                    "actions": [],
                    "first_ms": 1_700_000_000_000_i64,
                    "last_ms": 1_700_000_060_000_i64,
                    "entries": ["e010", "e011", "e012"],
                }],
            }
        })
        .to_string(),
    );
    let mut window = app.window();
    press(&mut window, "Open in History");
    assert_eq!(window.view_state("place"), Some(Value::text("history")));
    assert_eq!(
        window.view_state("first"),
        Some(Value::text("e010")),
        "the brief's oldest change is selected"
    );
    assert_eq!(
        window.view_state("last"),
        Some(Value::text("e012")),
        "the brief's newest change is selected"
    );
}
