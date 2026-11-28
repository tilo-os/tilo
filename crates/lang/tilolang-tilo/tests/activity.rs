//! Activity end to end: the history paged toward its oldest change and searched past the first
//! page, a chrome verb's ask opening the window at its place, a range compared before and
//! after, the agent session a range is with each record's own undo, and a brief opening its own
//! changes.

use crate::fixtures::Pane;
use serde_json::{Value as Json, json};
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewInstance};

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
        "undo": {"action": "undo-entry", "label": "Undo this step"},
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
        "app": "Notes",
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

fn installed(activity_reading: &str) -> Pane {
    Pane::install(&me(), SOURCE)
        .viewing("activity")
        .reading(&activity(), activity_reading)
        .rooted("so.tilo.activity.Window")
}

/// Pick `value` for the window's `state`, as the person does, and let the view run again.
fn pick(window: &mut ViewInstance, state: &str, value: Value) {
    window.set_view_state(state, value);
    window.poll_patches().expect("the window runs again");
}

fn of_kind<'a>(tree: &'a UiNode, kind: &str) -> Vec<&'a UiNode> {
    fn walk<'a>(node: &'a UiNode, kind: &str, found: &mut Vec<&'a UiNode>) {
        if node.kind == kind {
            found.push(node);
        }
        for child in &node.children {
            walk(child, kind, found);
        }
    }
    let mut found = Vec::new();
    walk(tree, kind, &mut found);
    found
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
fn open_history(app: &Pane) -> ViewInstance {
    let mut window = app.window();
    pick(&mut window, "place", Value::text("history"));
    window
}

/// Every word the page shows.
fn shown(view: &ViewInstance) -> Vec<String> {
    crate::fixtures::texts_props_held(
        view,
        &["value", "title", "label", "text", "message", "action"],
        true,
    )
}

#[test]
fn a_person_pages_toward_the_oldest_change_and_searches_past_the_first_page() {
    let app = installed("{}");
    app.device_says(&feed(0), &page(changes(200), 500));
    let mut window = open_history(&app);
    assert!(contains(&window, "Change 199"), "{:?}", shown(&window));
    assert!(!contains(&window, "Change 250"), "the first page holds 200");
    assert!(
        has(&window, "Show earlier changes"),
        "older changes are reachable"
    );

    // Showing more reads a larger page, and the changes only it holds appear.
    app.device_says(&feed(400), &page(changes(400), 500));
    press(&mut window, "Show earlier changes");
    assert!(contains(&window, "Change 250"), "{:?}", shown(&window));

    // The search reads further back than the page shown, and shows what it kept of what it
    // read.
    app.device_says(&feed(1600), &page(changes(500), 500));
    pick(&mut window, "found", Value::text("Change 250"));
    assert!(contains(&window, "Change 250"), "{:?}", shown(&window));
    assert!(
        has(&window, "1 of 500 changes"),
        "the search says what it kept: {:?}",
        shown(&window)
    );
    assert!(
        !has(&window, "Show earlier changes"),
        "the page holds every change the history matched"
    );
}

#[test]
fn a_chrome_verb_opens_the_window_at_its_focus_and_says_it_was_shown() {
    let app = installed(
        r#"{"focus":{"serial":7,"view":"history","author":"Ada","lens":null,"entry":null}}"#,
    );
    app.device_says(&feed(0), &page(changes(3), 3));
    let mut window = app.window();
    let timer = of_kind(window.tree(), "ui.Timer").first().map_or_else(
        || panic!("the window holds the ask's clock: {:?}", shown(&window)),
        |node| node.key.clone(),
    );
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
    let app = installed("{}");
    let standing = json!([
        still_in_effect("Renamed a note", "e020", "aa11"),
        still_in_effect("Moved a paragraph", "e010", "bb22"),
    ]);
    app.device_says(&feed(0), &page(changes(3), 3));
    app.device_says(&range("e010", "e020"), &detail(None, json!([])));
    app.device_says(
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
        app.sent.arg("open", "id"),
        Some(Value::text("note-1")),
        "the change's resource opens where it is"
    );

    // A session that ran long lists a page, and showing more reads a larger one.
    app.device_says(
        &sessions(20),
        &sessions_page(json!([still_standing(standing, 30)])),
    );
    window.poll_patches().expect("the window runs again");
    assert!(has(&window, "Show more"), "{:?}", shown(&window));
    app.device_says(
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
    let app = installed("{}");
    app.device_says(&feed(0), &page(changes(3), 3));
    app.device_says(
        &range("e010", "e020"),
        &detail(None, json!([{"action": "compare", "label": "Compare"}])),
    );
    app.device_says(
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
                "comparison": {"state": "changes", "lines": ["Added a page", "Removed a note"]},
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
    let app = installed(
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

/// A change with the group it was made in, when, by whom, and which devices refused it.
fn grouped(
    id: &str,
    summary: &str,
    group: Option<&str>,
    at_ms: i64,
    author: &str,
    refused: Json,
) -> Json {
    let mut one = change(id, summary);
    one["group"] = json!(group);
    one["at_ms"] = json!(at_ms);
    one["author"]["name"] = json!(author);
    one["refused"] = refused;
    one
}

/// A page whose changes and work groups the test names.
fn grouped_page(entries: Vec<Json>, groups: Json) -> String {
    json!({
        "feed": {
            "workspace": "Home",
            "scope": "this-workspace",
            "total": entries.len(),
            "entries": entries,
            "groups": groups,
            "people": ["Ada", "Bo"],
            "lenses": [],
            "pending": false,
            "mutations": true,
        }
    })
    .to_string()
}

#[test]
fn a_work_sessions_changes_stand_under_it_and_are_selected_together() {
    let app = installed("{}");
    app.device_says(
        &feed(0),
        &grouped_page(
            vec![
                grouped(
                    "e003",
                    "Renamed a note",
                    Some("g1"),
                    1_700_000_000_000,
                    "Ada",
                    json!([]),
                ),
                grouped(
                    "e002",
                    "Moved a paragraph",
                    Some("g1"),
                    1_700_000_000_000,
                    "Ada",
                    json!([]),
                ),
                grouped(
                    "e001",
                    "Tidied the plan",
                    None,
                    1_700_000_000_000,
                    "Bo",
                    json!([]),
                ),
            ],
            json!([{"id": "g1", "label": "Tidy the notes", "start": "e003", "end": "e002",
                    "named_by": null}]),
        ),
    );
    let mut window = open_history(&app);
    assert!(
        has(&window, "Tidy the notes"),
        "the session names its changes: {:?}",
        shown(&window)
    );
    press(&mut window, "Select");
    assert_eq!(window.view_state("first"), Some(Value::text("e003")));
    assert_eq!(window.view_state("last"), Some(Value::text("e002")));
}

#[test]
fn a_change_without_a_time_sits_under_undated() {
    let app = installed("{}");
    app.device_says(
        &feed(0),
        &grouped_page(
            vec![grouped(
                "e001",
                "Tidied the plan",
                None,
                0,
                "Ada",
                json!([]),
            )],
            json!([]),
        ),
    );
    let window = open_history(&app);
    assert!(has(&window, "Undated"), "{:?}", shown(&window));
}

#[test]
fn a_refused_change_says_which_device_refused_it_and_why() {
    let app = installed("{}");
    app.device_says(
        &feed(0),
        &grouped_page(
            vec![grouped(
                "e001",
                "Tidied the plan",
                None,
                1_700_000_000_000,
                "Ada",
                json!([{"change": "e001", "device": "home", "label": "Home laptop",
                      "reason": "the device is offline", "permanent": false}]),
            )],
            json!([]),
        ),
    );
    let window = open_history(&app);
    assert!(
        has(&window, "Not on Home laptop: the device is offline"),
        "{:?}",
        shown(&window)
    );
}

#[test]
fn the_person_filter_narrows_and_clearing_it_changes_nothing() {
    let app = installed("{}");
    app.device_says(
        &feed(0),
        &grouped_page(
            vec![
                grouped(
                    "e002",
                    "Renamed a note",
                    None,
                    1_700_000_000_000,
                    "Ada",
                    json!([]),
                ),
                grouped(
                    "e001",
                    "Tidied the plan",
                    None,
                    1_700_000_000_000,
                    "Bo",
                    json!([]),
                ),
            ],
            json!([]),
        ),
    );
    let mut window = open_history(&app);
    app.sent.take();
    pick(&mut window, "writer", Value::text("Bo"));
    assert!(contains(&window, "Tidied the plan"), "{:?}", shown(&window));
    assert!(!contains(&window, "Renamed a note"), "{:?}", shown(&window));
    assert!(has(&window, "1 of 2 changes"), "{:?}", shown(&window));
    assert!(
        app.sent.calls().is_empty(),
        "choosing a person writes nothing: {:?}",
        app.sent.calls()
    );

    pick(&mut window, "first", Value::text("e001"));
    press(&mut window, "Clear filters");
    assert_eq!(window.view_state("writer"), Some(Value::text("")));
    assert_eq!(window.view_state("first"), Some(Value::text("")));
    assert!(contains(&window, "Renamed a note"), "{:?}", shown(&window));
}

#[test]
fn a_work_session_is_named_and_the_name_goes_to_the_history() {
    let app = installed("{}");
    app.device_says(&feed(0), &page(changes(2), 2));
    app.device_says(&range("e000", "e000"), &detail(Some("run-1"), json!([])));
    let mut window = open_history(&app);
    pick(&mut window, "first", Value::text("e000"));
    pick(&mut window, "last", Value::text("e000"));
    assert!(
        of_kind(window.tree(), "ui.TextInput")
            .iter()
            .any(|node| node.props.get("placeholder")
                == Some(&Value::text("Name this work session"))),
        "the field says what it names: {:?}",
        shown(&window)
    );
    pick(&mut window, "name", Value::text("Morning tidy"));
    press(&mut window, "Save name");
    assert_eq!(
        app.sent.arg("name", "label"),
        Some(Value::text("Morning tidy")),
        "the typed words are the session's name"
    );
    let named = app.sent.arg("name", "range").expect("the range is named");
    let Value::Rec(range) = named else {
        panic!("the range is a record: {named:?}")
    };
    assert!(
        range
            .fields
            .values()
            .any(|value| value == &Value::text("e000")),
        "the range is the selected change: {range:?}"
    );
}

/// The range compared, with the changes the history says it made.
fn comparison(changes: Json, comparison: Json) -> String {
    json!({
        "detail": {
            "title": "Renamed a note",
            "provenance": "Ada renamed it",
            "authors": ["Ada"],
            "resources": [],
            "group": null,
            "group_label": null,
            "offers": [{"action": "compare", "label": "Compare"}],
            "changes": changes,
            "comparison": comparison,
            "gone": false,
        }
    })
    .to_string()
}

#[test]
fn the_fields_a_change_touched_read_before_and_after() {
    let app = installed("{}");
    app.device_says(&feed(0), &page(changes(2), 2));
    app.device_says(&range("e000", "e000"), &comparison(json!([]), json!(null)));
    app.device_says(
        &compare("e000", "e000"),
        &comparison(
            json!([
                {"field": "Title", "before": "Draft", "after": "Final"},
                {"field": "Pinned", "before": "yes", "after": ""},
            ]),
            json!(null),
        ),
    );
    let mut window = open_history(&app);
    pick(&mut window, "first", Value::text("e000"));
    pick(&mut window, "last", Value::text("e000"));
    press(&mut window, "Compare");
    let words = shown(&window);
    for wanted in ["Fields", "Title", "Pinned"] {
        assert!(
            words.iter().any(|text| text == wanted),
            "{wanted} shows: {words:?}"
        );
    }
    let secondary = |title: &str| {
        of_kind(window.tree(), "ui.ListRow")
            .into_iter()
            .find(|node| node.props.get("title") == Some(&Value::text(title)))
            .and_then(|node| node.props.get("secondary").cloned())
    };
    assert_eq!(
        secondary("Title"),
        Some(Value::text("Was Draft")),
        "what the field held before"
    );
    assert_eq!(
        secondary("Pinned"),
        Some(Value::text("Nothing now")),
        "a field that now holds nothing says so"
    );
}

#[test]
fn a_preview_that_could_not_be_read_says_so() {
    let app = installed("{}");
    app.device_says(&feed(0), &page(changes(2), 2));
    app.device_says(&range("e000", "e000"), &comparison(json!([]), json!(null)));
    app.device_says(
        &compare("e000", "e000"),
        &comparison(json!([]), json!({"state": "unavailable"})),
    );
    let mut window = open_history(&app);
    pick(&mut window, "first", Value::text("e000"));
    pick(&mut window, "last", Value::text("e000"));
    press(&mut window, "Compare");
    let words = shown(&window);
    assert!(
        words.iter().any(|text| text == "Preview unavailable"),
        "{words:?}"
    );
    assert!(
        words.iter().any(|text| text
            == "The workspace as it was before and after this change could not be read."),
        "{words:?}"
    );
}

/// A replay of the checkpoints `frames` holds.
fn replay_page(frames: Json, unavailable: i64, note: Json) -> String {
    json!({
        "replay": {
            "frames": frames,
            "unavailable": unavailable,
            "note": note,
            "pending": false,
        }
    })
    .to_string()
}

#[test]
fn replay_plays_its_checkpoints_and_names_each_window() {
    let app = installed(&replay_page(
        json!([
            {"entry": "e001", "summary": "Moved a note", "at_ms": 1_700_000_000_000_i64,
             "author": {"name": "Ada", "rgb": [200, 120, 90], "identity": "ada",
                        "relation": "reader", "on_behalf_of": null, "for_reader": true,
                        "task": null},
             "workspaces": [{"name": "Home", "groups": [
                 {"tabs": ["Notes"], "active": 0, "width_permille": 500},
                 {"tabs": [], "active": 0, "width_permille": 500},
             ]}],
             "pointers": [{"name": "Ada", "rgb": [200, 120, 90]}]},
            {"entry": "e002", "summary": "Renamed a note", "at_ms": 1_700_000_060_000_i64,
             "author": {"name": "Ada", "rgb": [200, 120, 90], "identity": "ada",
                        "relation": "reader", "on_behalf_of": null, "for_reader": true,
                        "task": null},
             "workspaces": [], "pointers": []},
        ]),
        0,
        json!(null),
    ));
    let mut window = app.window();
    pick(&mut window, "place", Value::text("replay"));
    let words = shown(&window);
    for wanted in ["One tab, 50% of the width", "Empty window", "Present: Ada"] {
        assert!(
            words.iter().any(|text| text == wanted),
            "{wanted} shows: {words:?}"
        );
    }
    assert!(has(&window, "Checkpoint 1 of 2"), "{:?}", shown(&window));
    press(&mut window, "Play");
    let clock = of_kind(window.tree(), "ui.Timer").first().map_or_else(
        || panic!("playing carries a clock: {:?}", shown(&window)),
        |node| node.key.clone(),
    );
    window.fire(&clock, "tick").expect("the clock strikes");
    window.poll_patches().expect("the window runs again");
    assert!(
        has(&window, "Checkpoint 2 of 2"),
        "the tick moved the place: {:?}",
        shown(&window)
    );
    assert!(has(&window, "Checkpoint 2 of 2"), "{:?}", shown(&window));
    assert!(
        has(&window, "The workspace did not exist at this checkpoint"),
        "a frame with no workspace says so: {:?}",
        shown(&window)
    );
}

#[test]
fn an_unavailable_replay_says_so() {
    let app = installed(&replay_page(
        json!([]),
        2,
        json!("The checkpoints were compacted"),
    ));
    let mut window = app.window();
    pick(&mut window, "place", Value::text("replay"));
    let words = shown(&window);
    assert!(
        words
            .iter()
            .any(|text| text == "Workspace replay is unavailable for these checkpoints"),
        "{words:?}"
    );
    assert!(
        words
            .iter()
            .any(|text| text == "The checkpoints were compacted"),
        "{words:?}"
    );
}

#[test]
fn marking_the_new_changes_viewed_is_offered_while_there_are_any() {
    let app = installed(
        &json!({
            "summary": {
                "workspace": "Home",
                "briefs": [{
                    "title": "Tidy the notes",
                    "detail": "Four notes put in order",
                    "authors": [],
                    "apps": [],
                    "resources": [],
                    "changes": 4,
                    "actions": [],
                    "first_ms": 1_700_000_000_000_i64,
                    "last_ms": 1_700_000_060_000_i64,
                    "group": null,
                    "work_session": null,
                    "entries": ["e010", "e011"],
                }],
                "narrative": null,
                "resume": null,
                "away": true,
                "sources_available": false,
                "sources": [],
                "pending": false,
            }
        })
        .to_string(),
    );
    let mut window = app.window();
    assert!(has(&window, "While you were away"), "{:?}", shown(&window));
    press(&mut window, "Mark as viewed");
    let named = app
        .sent
        .arg("mark-viewed", "entries")
        .expect("the entries are named");
    let Value::List(entries) = named else {
        panic!("the changes are a list: {named:?}")
    };
    assert_eq!(entries.len(), 2, "every change the brief showed");
}

#[test]
fn a_morning_source_opens_refreshes_and_is_removed() {
    let app = installed(
        &json!({
            "summary": {
                "workspace": "Home",
                "briefs": [],
                "narrative": null,
                "resume": null,
                "away": false,
                "sources_available": true,
                "sources": [{
                    "key": "mail",
                    "label": "Morning mail",
                    "text": "3 new messages",
                    "refreshable": true,
                    "status": null,
                }],
                "pending": false,
            }
        })
        .to_string(),
    );
    let mut window = app.window();
    let row = of_kind(window.tree(), "ui.ListRow")
        .into_iter()
        .find_map(|node| {
            (node.props.get("title") == Some(&Value::text("Morning mail")))
                .then(|| node.key.clone())
        });
    let Some(row) = row else {
        panic!("the source is a row: {:?}", shown(&window))
    };
    window.fire(&row, "activate").expect("the source opens");
    assert_eq!(
        last_command(&app),
        "open",
        "the row opens what it heard about"
    );
    press(&mut window, "Refresh source");
    assert_eq!(last_command(&app), "refresh");
    press(&mut window, "Remove morning source");
    assert_eq!(last_command(&app), "remove");
}

/// The command of the last `source` call the app sent.
fn last_command(app: &Pane) -> String {
    let (_, args) = app
        .sent
        .calls()
        .into_iter()
        .rev()
        .find(|(name, _)| name == "source")
        .expect("the app asks the source's verb");
    let command = args
        .into_iter()
        .find(|(named, _)| named == "command")
        .map(|(_, value)| value);
    command_of(command.expect("the call names its command"))
}

/// The name of an enum value an action was given.
fn command_of(value: Value) -> String {
    match value {
        Value::Enum(case) => case
            .variant
            .rsplit('.')
            .next()
            .unwrap_or(&case.variant)
            .to_owned(),
        Value::Text(name) => name,
        other => panic!("a command is an enum value, not {other:?}"),
    }
}
