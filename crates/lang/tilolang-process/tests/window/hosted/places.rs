//! A window's places are offered wherever its actions are: the Calendar's root publishes its
//! week, month, day, agenda and tasks, all its calendars and each list of its tasks, as actions
//! labelled in the reader's language. The launcher, the palette and an agent invoke the same one,
//! and the window goes to the place for the person at it.

use super::{Session, agent_session, root_id};
use crate::change_marks::{Driven as _, flattened, settle};
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
    let resources = app.harness().resources();
    flattened(&resources)
        .into_iter()
        .find(|published| published.id.as_str() == resource)
        .expect("the window publishes its root")
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
    let ambient = invoker.as_ref().map(|_| agent_session().1);
    app.invoke(invoker, ambient.as_ref(), &resource, action, &Map::new())
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
