//! The tilolang shell's navigation surfaces, driven the way a person and an agent reach them: the
//! launcher over everything the search found, the strip menu at the bar's left end, the workspace
//! menu and the welcome cards an empty workspace offers. Each test starts the session with the
//! tilolang shell and drives it through the shell's published actions and the controls it lays out
//! for assistive technology.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_actions::ApiError;
use tilo_actions::remote::RemoteActionTarget;
use tilo_compositor::OutputConfig;
use tilo_desktop::verbs::SHELL_RESOURCE;
use tilo_desktop_search::providers::AppEntry;
use tilo_session::{AppActions, AppLists, Lists, Options, Session};

use crate::common::{HEIGHT, PERSON, WIDTH, key, press, type_text};

/// The Down and Enter keys, as the compositor's keyboard reads them.
const KEY_DOWN: u32 = 108;
const KEY_ENTER: u32 = 28;

/// An action host that keeps what it was asked to run: what a launcher's row or a welcome card
/// reaches.
#[derive(Default)]
struct Asked {
    invoked: RefCell<Vec<RemoteActionTarget>>,
}

impl AppActions for Asked {
    fn invoke(
        &self,
        target: RemoteActionTarget,
        _arguments: Map<String, Value>,
    ) -> Result<(), ApiError> {
        self.invoked.borrow_mut().push(target);
        Ok(())
    }
}

/// A session with the tilolang shell, the apps `apps` installed, and `asked` hearing what runs.
fn session_with(asked: Rc<Asked>, apps: &[(&str, &str)]) -> Session {
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            lists: Lists::fixed(AppLists {
                applications: apps
                    .iter()
                    .map(|(id, title)| AppEntry {
                        id: (*id).into(),
                        title: (*title).into(),
                        category: None,
                    })
                    .collect(),
                ..AppLists::default()
            }),
            apps: Some(asked),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session
        .compositor()
        .set_key_repeat(tilo_compositor::KeyRepeat {
            delay: Duration::from_secs(1),
            rate: 0,
        });
    session
}

/// Run frames until `done` holds.
#[track_caller]
fn until(session: &mut Session, what: &str, done: impl FnMut(&mut Session) -> bool) {
    crate::common::until(session, what, done);
}

/// The arguments of an action call.
fn arguments(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

/// The shell's labels, joined, for an assertion that says what the whole shell says.
fn labels(session: &Session) -> String {
    session.accessible_labels().join("\n")
}

#[test]
fn the_launcher_lists_what_the_search_found_under_its_kinds() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_with(Rc::new(Asked::default()), &[("so.tilo.sheets", "Sheets")]);
    until(&mut session, "the shell to stand", |session| {
        labels(session).contains("All apps") || !session.accessible_labels().is_empty()
    });
    session
        .invoke(
            SHELL_RESOURCE,
            "search",
            &arguments(json!({ "query": "sheets" })),
        )
        .expect("search runs");
    // The search publishes what it found, grouped by kind, and the launcher draws the groups and
    // the rows: a person reads the group and the row on the shell.
    until(&mut session, "the search's rows", |session| {
        session
            .read("session:search")
            .and_then(|search| search["groups"].as_array().map(|groups| !groups.is_empty()))
            .unwrap_or(false)
    });
    let search = session
        .read("session:search")
        .expect("the search is published");
    let groups = search["groups"].as_array().expect("groups");
    assert!(
        groups.iter().any(|group| group["kind"] == "Apps"),
        "the app it found stands under Apps: {search}"
    );
    until(&mut session, "the launcher to list the row", |session| {
        let labels = labels(session);
        labels.contains("Apps") && labels.contains("Sheets")
    });
    assert!(
        labels(&session).contains("Search apps, windows, actions and files"),
        "the launcher's field is on screen: {:?}",
        session.accessible_labels()
    );
}

#[test]
fn the_launcher_walks_its_rows_and_opens_the_one_it_reaches() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_with(
        Rc::new(Asked::default()),
        &[("so.tilo.notes", "Notes"), ("so.tilo.sheets", "Sheets")],
    );
    until(&mut session, "the shell to stand", |session| {
        !session.accessible_labels().is_empty()
    });
    session
        .invoke(
            SHELL_RESOURCE,
            "search",
            &arguments(json!({ "query": "sheets" })),
        )
        .expect("search runs");
    until(&mut session, "the search's row", |session| {
        labels(session).contains("Sheets")
    });
    // The keys walk the rows the search published, and Enter runs the one they reached: the
    // launcher leaves for it, as it does when a press chooses a row.
    key(&mut session, KEY_DOWN);
    key(&mut session, KEY_ENTER);
    until(&mut session, "the row Enter ran", |session| {
        !labels(session).contains("Search apps, windows, actions and files")
    });
}

#[test]
fn the_launcher_offers_the_welcome_cards_and_a_card_runs() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_with(Rc::new(Asked::default()), &[("so.tilo.notes", "Notes")]);
    until(&mut session, "the shell to stand", |session| {
        !session.accessible_labels().is_empty()
    });
    // The welcome cards this device offers are published, and the empty launcher draws them.
    let launchpad = session
        .read("session:launchpad")
        .expect("the launchpad is published");
    let cards = launchpad["cards"].as_array().expect("cards");
    let notes = cards
        .iter()
        .find(|card| card["slug"] == "notes")
        .expect("Notes is installed, so its card stands");
    let notes_label = notes["label"].as_str().unwrap().to_owned();
    session
        .invoke(SHELL_RESOURCE, "search", &Map::new())
        .expect("the launcher opens");
    until(
        &mut session,
        "the welcome card on the launcher",
        |session| labels(session).contains(&notes_label),
    );
    // The card runs the one published verb the launchpad offers, and the app it names opens.
    session
        .invoke(
            SHELL_RESOURCE,
            "launchpad",
            &arguments(json!({ "card": "notes" })),
        )
        .expect("the card runs");
    until(&mut session, "the window the card opened", |session| {
        labels(session).contains("Close Notes")
    });
}

/// The overview answers to the words people use for it: the shell publishes those as the action's
/// search keywords, so system search finds it by any of them.
#[test]
fn the_overview_answers_to_the_words_people_use_for_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_with(Rc::new(Asked::default()), &[("so.tilo.notes", "Notes")]);
    until(&mut session, "the shell to stand", |session| {
        !session.accessible_labels().is_empty()
    });
    for word in ["expose", "mission control", "all windows"] {
        session
            .invoke(
                SHELL_RESOURCE,
                "search",
                &arguments(json!({ "query": word })),
            )
            .expect("search runs");
        until(&mut session, word, |session| {
            session
                .read("session:search")
                .and_then(|search| search["groups"].as_array().cloned())
                .is_some_and(|groups| {
                    groups.iter().any(|group| {
                        group["rows"].as_array().is_some_and(|rows| {
                            rows.iter().any(|row| {
                                row["title"]
                                    .as_str()
                                    .is_some_and(|title| title == "Overview")
                            })
                        })
                    })
                })
        });
    }
}

/// The person's first search: typing a name, then Enter, opens what the first row names. This is
/// the walk's `launcher/open-settings` step: the search's own first row is chosen without an arrow,
/// and the launcher leaves for what it opened.
#[test]
fn a_search_for_settings_opens_it_on_enter() {
    let _settled = ui::motion::testing::settled();
    let asked = Rc::new(Asked::default());
    let mut session = session_with(Rc::clone(&asked), &[("so.tilo.settings", "Settings")]);
    until(&mut session, "the shell to stand", |session| {
        !session.accessible_labels().is_empty()
    });
    session
        .invoke(
            SHELL_RESOURCE,
            "search",
            &arguments(json!({ "query": "Settings" })),
        )
        .expect("search runs");
    until(&mut session, "the search's Settings row", |session| {
        labels(session).contains("Settings")
    });
    key(&mut session, KEY_ENTER);
    until(&mut session, "what the first row opened", |_| {
        !asked.invoked.borrow().is_empty()
    });
    let target = format!("{:?}", asked.invoked.borrow()[0]);
    assert!(
        target.contains("settings"),
        "the first row opened Settings, and not {target}"
    );
    until(&mut session, "the launcher to leave", |session| {
        !labels(session).contains("Search apps, windows, actions and files")
    });
}

/// The strip rail says where the person is among the strips and makes another: "1 of 1" with a
/// strip to make, and, with one made, the strip before it peeking where a press goes to it.
#[test]
fn the_strip_rail_counts_the_strips_and_makes_one() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_with(Rc::new(Asked::default()), &[("so.tilo.notes", "Notes")]);
    until(&mut session, "the rail", |session| {
        labels(session).contains("1 of 1")
    });
    press(&mut session, "New strip");
    until(
        &mut session,
        "the count to follow the strip made",
        |session| labels(session).contains("2 of 2"),
    );
    let rows = session.layout_reading().rows;
    let first = rows
        .iter()
        .find(|row| !row.current)
        .expect("the strip before the one shown")
        .name
        .clone();
    assert!(
        labels(&session).contains(&first),
        "the strip before peeks at the leading edge: {:?}",
        session.accessible_labels()
    );
}

#[test]
fn the_bars_strip_menu_adds_switches_renames_and_removes_a_strip() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_with(Rc::new(Asked::default()), &[("so.tilo.notes", "Notes")]);
    let rows = |session: &Session| session.layout_reading().rows;
    until(&mut session, "the bar to name its strip", |session| {
        rows(session).iter().any(|row| row.current)
    });
    let first = rows(&session)
        .into_iter()
        .find(|row| row.current)
        .expect("a strip in front")
        .name;
    // The strip's name opens its menu, and a new strip is one of its rows.
    press(&mut session, &first);
    until(&mut session, "the strip menu", |session| {
        session.shell_control("New strip").is_some()
    });
    press(&mut session, "New strip");
    until(&mut session, "the new strip", |session| {
        rows(session).len() == 2
    });
    // A strip made is the one shown, so its name names the bar: rename it where it stands.
    let second = rows(&session)
        .into_iter()
        .find(|row| row.name != first)
        .expect("the strip it made")
        .name;
    press(&mut session, &second);
    until(&mut session, "the strip's rename row", |session| {
        session.shell_control(&format!("Rename {second}")).is_some()
    });
    press(&mut session, &format!("Rename {second}"));
    type_text(&mut session, "Research");
    key(&mut session, KEY_ENTER);
    until(&mut session, "the strip's new name", |session| {
        rows(session).iter().any(|row| row.name == "Research")
    });
    // It holds nothing, so the menu takes it away, and the workspace keeps the strip it had.
    press(&mut session, "Research");
    until(&mut session, "the strip's remove row", |session| {
        session.shell_control("Remove Research").is_some()
    });
    press(&mut session, "Remove Research");
    until(&mut session, "the strip to go", |session| {
        rows(session).len() == 1 && rows(session).iter().all(|row| row.name != "Research")
    });
}

#[test]
fn the_strips_verb_lists_the_workspaces_strips_and_a_strip_is_chosen() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_with(Rc::new(Asked::default()), &[("so.tilo.notes", "Notes")]);
    let rows = |session: &Session| session.layout_reading().rows;
    until(&mut session, "the bar to name its strip", |session| {
        rows(session).iter().any(|row| row.current)
    });
    session
        .invoke(
            "desktop:chrome",
            "new-strip",
            &arguments(json!({ "name": "Errands" })),
        )
        .expect("a strip is made");
    until(&mut session, "the strip it made", |session| {
        rows(session).len() == 2
    });
    let errands = rows(&session)
        .into_iter()
        .find(|row| row.name == "Errands")
        .expect("the strip by the name it was made with");
    assert!(errands.current, "a strip made is the one the shell shows");
    let first = rows(&session)
        .into_iter()
        .find(|row| row.name != "Errands")
        .expect("the strip it was made beside")
        .name;
    // `show-strips` opens the shell's own list of the workspace's strips, and choosing one shows
    // it and puts the list away.
    session
        .invoke("desktop:chrome", "show-strips", &Map::new())
        .expect("the strips list opens");
    until(&mut session, "the strips list", |session| {
        let labels = labels(session);
        labels.contains("Errands") && labels.contains("New strip")
    });
    press(&mut session, &first);
    until(&mut session, "the strip the list chose", |session| {
        rows(session)
            .iter()
            .any(|row| row.current && row.name == first)
    });
    until(&mut session, "the list put away", |session| {
        !labels(session).contains("Done")
    });
}

#[test]
fn the_bars_workspace_menu_offers_renaming_and_deleting_a_workspace() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_with(Rc::new(Asked::default()), &[("so.tilo.notes", "Notes")]);
    until(&mut session, "the bar to name its workspace", |session| {
        session
            .layout_reading()
            .workspaces
            .iter()
            .any(|workspace| workspace.current)
    });
    let first = session
        .layout_reading()
        .workspaces
        .into_iter()
        .find(|workspace| workspace.current)
        .expect("a workspace in front")
        .name;
    press(&mut session, &first);
    until(&mut session, "the workspace's menu", |session| {
        session.shell_control(&format!("Rename {first}")).is_some()
    });
    until(&mut session, "the workspace's delete row", |session| {
        session.shell_control(&format!("Delete {first}")).is_some()
    });
}
