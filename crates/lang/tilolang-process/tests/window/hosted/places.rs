//! A window's places are offered wherever its actions are: the Calendar's root publishes its
//! week, month, day, agenda and tasks, all its calendars and each list of its tasks, as actions
//! labelled in the reader's language. The launcher, the palette and an agent invoke the same one,
//! and the window goes to the place for the person at it. What a window opens has places too:
//! one comparison of Diff & Merge opens at its merge, and the Database Client at its structure.

use super::{Session, agent_session, root_id};
use crate::change_marks::{Driven as _, action_id, flattened, settle};
use serde_json::Map;
use tilolang_tilo::runtime::Value;

const CALENDAR: &str = "so.tilo.calendar";

/// The Calendar's root, as the window publishes it.
fn root(app: &Session) -> String {
    format!("tl:{CALENDAR}:{}", root_id(app, CALENDAR))
}

/// The places the root offers to open the window at, as `(action, label)`, in the order the window
/// declares them.
fn offered(app: &mut Session) -> Vec<(String, String)> {
    let resource = root(app);
    offered_by(app, &resource)
}

/// The places `resource` offers to open the window at, as `(action, label)`.
fn offered_by(app: &mut Session, resource: &str) -> Vec<(String, String)> {
    let resources = app.harness().resources();
    flattened(&resources)
        .into_iter()
        .find(|published| published.id.as_str() == resource)
        .expect("the window publishes it")
        .actions
        .iter()
        .filter(|action| action.id.starts_with("show-"))
        .map(|action| (action.id.clone(), action.label.clone()))
        .collect()
}

/// The place the window's sidebar shows chosen, and the list of tasks it shows.
fn standing(app: &Session) -> (Value, Value) {
    let tree = app
        .instance()
        .instance()
        .root_tree()
        .expect("the window shows its root");
    let selected = |at: usize| {
        tree.of_kind("ui.NavList")
            .get(at)
            .and_then(|list| list.props.get("selected"))
            .cloned()
            .unwrap_or(Value::Unit)
    };
    let place = selected(0);
    let shell = tree
        .first_of_kind("ui.NavShell")
        .and_then(|shell| shell.props.get("title"))
        .cloned()
        .unwrap_or(Value::Unit);
    (place, shell)
}

/// Invoke the place `action` of the root, as `invoker` (the person when `None`).
fn open_at(app: &mut Session, invoker: Option<ui::tilo_actions::ActionInvoker>, action: &str) {
    let resource = root(app);
    open_place(app, invoker, &resource, action);
}

/// Invoke the place `action` of `resource`, as `invoker` (the person when `None`).
fn open_place(
    app: &mut Session,
    invoker: Option<ui::tilo_actions::ActionInvoker>,
    resource: &str,
    action: &str,
) {
    let ambient = invoker.as_ref().map(|_| agent_session().1);
    app.invoke(invoker, ambient.as_ref(), resource, action, &Map::new())
        .unwrap_or_else(|error| panic!("{action}: {error}"));
    settle(app);
}

#[test]
fn the_calendar_offers_its_places_in_the_readers_words() {
    let mut app = Session::new(CALENDAR, "ada");
    settle(&mut app);
    let labels = |offered: Vec<(String, String)>| -> Vec<String> {
        offered.into_iter().map(|(_, label)| label).collect()
    };
    assert_eq!(
        labels(offered(&mut app)),
        [
            "Week",
            "Month",
            "Day",
            "Agenda",
            "Tasks",
            "All calendars",
            "Task inbox",
            "Today's tasks",
            "Upcoming tasks",
            "Done tasks"
        ],
        "every place the window declares, as the launcher and the palette list them"
    );
    let mut german = Session::in_language(CALENDAR, "ada", "de-DE");
    settle(&mut german);
    assert_eq!(
        labels(offered(&mut german)),
        [
            "Woche",
            "Monat",
            "Tag",
            "Agenda",
            "Aufgaben",
            "Alle Kalender",
            "Aufgabeneingang",
            "Heutige Aufgaben",
            "Anstehende Aufgaben",
            "Erledigte Aufgaben"
        ],
        "in the words of a desktop that speaks German"
    );
}

#[test]
fn a_person_and_an_agent_open_the_window_at_a_place() {
    let mut app = Session::new(CALENDAR, "ada");
    settle(&mut app);
    assert_eq!(
        standing(&app).0,
        Value::text("month"),
        "it opens on the month"
    );
    // The person picks Tasks from the launcher or the palette.
    open_at(&mut app, None, "show-tasks");
    assert_eq!(standing(&app).0, Value::text("tasks"));
    // An agent acting for her opens the task inbox, by the same action.
    let (agent, _) = agent_session();
    open_at(&mut app, Some(agent.clone()), "show-inbox");
    assert_eq!(
        standing(&app),
        (Value::text("tasks"), Value::text("Inbox")),
        "the tasks, at the inbox"
    );
    open_at(&mut app, Some(agent), "show-week");
    assert_eq!(standing(&app).0, Value::text("week"));
}

const DIFFMERGE: &str = "so.tilo.diffmerge";
const DATABASE: &str = "so.tilo.db-client";

/// The page of the comparison the window's detail shows: the toolbar's Diff, Merge and Folders
/// segment it has chosen.
fn comparison_page(app: &Session) -> Value {
    let tree = app
        .instance()
        .instance()
        .detail_tree()
        .expect("the window shows the comparison");
    tree.first_of_kind("ui.ToggleGroup")
        .and_then(|group| group.props.get("selected"))
        .cloned()
        .unwrap_or(Value::Unit)
}

#[test]
fn a_comparison_opens_at_its_merge_from_outside_the_window() {
    let mut app = Session::new(DIFFMERGE, "ada");
    settle(&mut app);
    let library = format!("tl:{DIFFMERGE}:{}", root_id(&app, DIFFMERGE));
    let load = action_id(&mut app, &library, "Load sample");
    app.invoke(None, None, &library, &load, &Map::new())
        .expect("the sample loads");
    settle(&mut app);
    let resources = app.harness().resources();
    let comparison = flattened(&resources)
        .into_iter()
        .find(|published| {
            published.id.as_str() != library
                && published.actions.iter().any(|a| a.id == "show-merging")
        })
        .expect("the comparison offers its places")
        .id
        .as_str()
        .to_owned();
    assert_eq!(
        offered_by(&mut app, &comparison)
            .into_iter()
            .map(|(_, label)| label)
            .collect::<Vec<_>>(),
        ["Two-way diff", "Three-way merge", "Folder comparison"],
    );
    assert!(
        app.instance().instance().detail_tree().is_none(),
        "loading it from outside leaves the window on its library"
    );
    // The palette or an agent opens the comparison at its merge: the window opens it there.
    let (agent, _) = agent_session();
    open_place(&mut app, Some(agent), &comparison, "show-merging");
    assert_eq!(comparison_page(&app), Value::Int(1), "at the merge");
    // While it shows, it goes to another of its pages, and back.
    open_place(&mut app, None, &comparison, "show-folders");
    assert_eq!(comparison_page(&app), Value::Int(2), "at the folders");
    open_place(&mut app, None, &comparison, "show-diff");
    assert_eq!(comparison_page(&app), Value::Int(0), "at the two-way diff");
}

/// The view the person's own database window shows, by its case's id in the app's ids lock.
fn database_view(app: &Session) -> String {
    let host = app.instance_host();
    let runtime = host.runtime();
    let database = host.root_of(DATABASE).expect("the database root");
    let Ok(Value::List(sessions)) = runtime.read(&database, "sessions") else {
        panic!("the database lists its windows");
    };
    let [Value::Ref(session)] = sessions.as_slice() else {
        panic!("one window: {sessions:?}");
    };
    match runtime.read(&session.target, "tab") {
        Ok(Value::Enum(tab)) => tab.variant,
        other => panic!("the window's view: {other:?}"),
    }
}

#[test]
fn the_database_client_opens_at_any_of_its_views() {
    let mut app = Session::new(DATABASE, "ada");
    settle(&mut app);
    let database = format!("tl:{DATABASE}:{}", root_id(&app, DATABASE));
    assert_eq!(
        offered_by(&mut app, &database),
        [
            ("show-query".to_owned(), "Query editor".to_owned()),
            ("show-data".to_owned(), "Table rows".to_owned()),
            ("show-structure".to_owned(), "Table structure".to_owned()),
        ],
    );
    // `Tab.structure` and `Tab.query` in `apps/tl/db-client/app.ids`.
    open_place(&mut app, None, &database, "show-structure");
    assert_eq!(database_view(&app), "wfq2ds6s", "at the structure");
    let (agent, _) = agent_session();
    open_place(&mut app, Some(agent), &database, "show-query");
    assert_eq!(database_view(&app), "mj57ytfw", "at the query editor");
}
