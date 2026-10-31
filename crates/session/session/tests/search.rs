//! The desktop's search in the headless native session, driven only through
//! what the session publishes: search opens by the desk's `open-search`,
//! takes a query by its `search-for`, shows its rows as resources, and a
//! chosen row runs by its address through the action host.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use tilo_actions::remote::{
    RemoteActionTarget, application_action_target, application_open_target,
};
use tilo_actions::{DoorError, ResourceSpec};
use tilo_compositor::OutputConfig;
use tilo_desktop::desktop_actions::{
    CHROME_RESOURCE, DESK_RESOURCE, SEARCH_RESOURCE, SEARCH_ROW_KIND,
};
use tilo_desktop_search::providers::AppEntry;
use tilo_desktop_search::providers::PublishedActions;
use tilo_desktop_search::providers::jobs::JobEntry;
use tilo_desktop_search::providers::settings::SETTINGS_APP;
use tilo_session::{AppActions, AppLists, Lists, Options, SearchOptions, Session};

type Arguments = serde_json::Map<String, serde_json::Value>;

/// An action host that keeps what it was asked to run.
#[derive(Default)]
struct Host {
    invoked: RefCell<Vec<(RemoteActionTarget, Arguments)>>,
}

impl AppActions for Host {
    fn published(&self) -> Vec<PublishedActions> {
        Vec::new()
    }

    fn jobs(&self) -> Vec<JobEntry> {
        Vec::new()
    }

    fn invoke(&self, target: RemoteActionTarget, arguments: Arguments) -> Result<(), DoorError> {
        self.invoked.borrow_mut().push((target, arguments));
        Ok(())
    }

    fn cancel(&self, _: &str, _: &str, _: &str) -> Result<(), DoorError> {
        Ok(())
    }

    fn open_uri(&self, _: &str, _: &str) -> Result<(), DoorError> {
        Ok(())
    }

    fn start_host_app(&self, _: &str) -> Result<(), DoorError> {
        Ok(())
    }
}

fn session(host: Rc<Host>) -> Session {
    session_over(host, None)
}

/// A session whose search also lists the records `records` keeps current.
fn session_over(host: Rc<Host>, records: Option<tilo_session::Records>) -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            search: SearchOptions {
                lists: Lists::fixed(AppLists {
                    applications: vec![AppEntry {
                        id: "so.tilo.sheets".into(),
                        title: "Sheets".into(),
                        category: None,
                        description: None,
                    }],
                    ..AppLists::default()
                }),
                records,
                ..SearchOptions::default()
            },
            apps: Some(host),
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("session")
}

/// Run frames until `done` holds.
fn until(session: &mut Session, mut done: impl FnMut(&Session) -> bool) {
    for _ in 0..200 {
        session.step(Duration::from_millis(1)).expect("step");
        if done(session) {
            return;
        }
    }
    panic!("condition not reached");
}

fn find(resources: &[ResourceSpec], id: &str) -> Option<ResourceSpec> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource.clone())
        } else {
            find(&resource.children, id)
        }
    })
}

/// The open search's rows, as the desk publishes them.
fn rows(session: &Session) -> Vec<ResourceSpec> {
    find(&session.resources(), SEARCH_RESOURCE)
        .map(|search| search.children)
        .unwrap_or_default()
}

/// The first row whose label starts with `title`.
fn row(session: &Session, title: &str) -> Option<ResourceSpec> {
    rows(session)
        .into_iter()
        .find(|row| row.kind == SEARCH_ROW_KIND && row.label.starts_with(title))
}

fn invoke(session: &mut Session, resource: &str, action: &str, arguments: serde_json::Value) {
    let serde_json::Value::Object(arguments) = arguments else {
        panic!("arguments are an object");
    };
    session
        .invoke(resource, action, &arguments)
        .unwrap_or_else(|error| panic!("{resource} {action}: {error:?}"));
}

#[test]
fn search_opens_by_its_action_and_runs_an_app_and_a_setting_by_address() {
    let _settled = ui::motion::testing::settled();
    let host = Rc::new(Host::default());
    let mut session = session(Rc::clone(&host));
    session.step(Duration::from_millis(1)).unwrap();
    let room = session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.room().to_string())
            .expect("a workspace")
    });

    invoke(
        &mut session,
        DESK_RESOURCE,
        "open-search",
        serde_json::json!({}),
    );
    until(&mut session, |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_some()
    });
    invoke(
        &mut session,
        SEARCH_RESOURCE,
        "search-for",
        serde_json::json!({"query": "sheets"}),
    );
    until(&mut session, |session| row(session, "Sheets").is_some());
    let app = row(&session, "Sheets").unwrap();
    invoke(
        &mut session,
        app.id.as_str(),
        "choose",
        serde_json::json!({}),
    );
    until(&mut session, |_| !host.invoked.borrow().is_empty());
    assert_eq!(
        host.invoked.borrow()[0].0,
        application_open_target(&room, "so.tilo.sheets"),
        "the app opens by the workspace's address for it"
    );
    until(&mut session, |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_none()
    });

    invoke(
        &mut session,
        DESK_RESOURCE,
        "open-search",
        serde_json::json!({"query": "keyboard layout"}),
    );
    until(&mut session, |session| {
        row(session, "Keyboard layout").is_some()
    });
    let setting = row(&session, "Keyboard layout").unwrap();
    invoke(
        &mut session,
        setting.id.as_str(),
        "choose",
        serde_json::json!({}),
    );
    until(&mut session, |_| host.invoked.borrow().len() == 2);
    let (target, arguments) = host.invoked.borrow()[1].clone();
    assert_eq!(
        target,
        application_action_target(
            &room,
            SETTINGS_APP,
            "settings.setting:keyboard.layout",
            "open"
        ),
        "the setting opens in Settings by its address"
    );
    assert_eq!(arguments["anchor"], "settings.setting:keyboard.layout");
}

#[test]
fn the_sessions_own_actions_are_found_and_run_on_its_published_surface() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(Rc::new(Host::default()));
    session.step(Duration::from_millis(1)).unwrap();
    let before = session.accessible_labels();
    invoke(
        &mut session,
        DESK_RESOURCE,
        "open-search",
        serde_json::json!({"query": "overview"}),
    );
    until(&mut session, |session| {
        row(session, "Show or hide the overview").is_some()
    });
    let toggle = row(&session, "Show or hide the overview").unwrap();
    invoke(
        &mut session,
        toggle.id.as_str(),
        "choose",
        serde_json::json!({}),
    );
    until(&mut session, |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_none()
    });
    session.step(Duration::from_millis(1)).unwrap();
    assert_ne!(
        session.accessible_labels(),
        before,
        "the overview shows over the desk"
    );
}

/// A session with the settings service, over a home of its own.
fn session_with_settings() -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            services: vec![Box::new(tilo_settings_service::SettingsService::new(
                "person",
            ))],
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("session")
}

/// Search for `query` and choose the row titled `title`: a switch stays in
/// view, anything else closes search.
fn choose(session: &mut Session, query: &str, title: &str) {
    invoke(
        session,
        DESK_RESOURCE,
        "open-search",
        serde_json::json!({ "query": query }),
    );
    for _ in 0..200 {
        session.step(Duration::from_millis(1)).expect("step");
        if row(session, title).is_some() {
            break;
        }
    }
    let Some(row) = row(session, title) else {
        let labels: Vec<String> = rows(session).into_iter().map(|row| row.label).collect();
        panic!("no row {title:?} for {query:?} among {labels:?}");
    };
    invoke(session, row.id.as_str(), "choose", serde_json::json!({}));
}

/// The field `field` of the settings section `kind` as stored.
fn stored(session: &Session, kind: &str, field: &str) -> serde_json::Value {
    session
        .read(&format!("settings:{kind}"))
        .map(|section| section[field].clone())
        .unwrap_or_default()
}

#[test]
fn a_switch_and_a_choice_are_changed_from_search_and_the_choice_is_kept() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = session_with_settings();
    session.step(Duration::from_millis(1)).unwrap();

    choose(&mut session, "high contrast", "High contrast");
    // The switch flips where it is kept.
    until(&mut session, |session| {
        stored(session, "settings.appearance", "high_contrast") == serde_json::json!(true)
    });
    assert!(
        find(&session.resources(), SEARCH_RESOURCE).is_some(),
        "a switch stays in view"
    );

    choose(&mut session, "dark mode", "Dark mode");
    until(&mut session, |session| {
        stored(session, "settings.appearance", "mode") == serde_json::json!("dark")
    });
    until(&mut session, |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_none()
    });

    // What was chosen outlasts the session.
    until(&mut session, |session| {
        stored(session, "settings.search_history", "chosen")
            .as_str()
            .is_some_and(|chosen| !chosen.is_empty())
    });
}

/// The app whose record index the record of this test is named in: any app of
/// the workspace keeps one, and search reads them all the same way.
const NOTES: &str = "so.tilo.notes";

/// The record a row brings here: a file-backed document, as a workspace that
/// is not this one names it.
const QUARTERLY: &str = "file:/work/Quarterly plan.odt";

/// The workspaces the desk shows, by room and by the name it gives it.
fn workspaces(session: &Session) -> Vec<(String, String)> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .map(|workspace| (workspace.room().to_string(), workspace.name.clone()))
            .collect()
    })
}

/// The room of the workspace in front.
fn active_room(session: &Session) -> String {
    session
        .handle()
        .read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.room().to_string())
        })
        .expect("a workspace")
}

/// Save `rows` (`(id, kind)`) as `package`'s index of `room` holds them, and
/// publish it in one rename: the index of a workspace that appears while the
/// session runs appears as one directory, which is what the worker following
/// the device's indexes is told about.
fn write_index(home: &tilo_test_env::TestHome, package: &str, room: &str, rows: &[(&str, &str)]) {
    let directory =
        app_state::room_index_directory(home.data(), package, room).expect("an index directory");
    let staged = directory.with_file_name(format!("staging-{room}"));
    std::fs::create_dir_all(&staged).expect("the index's directory");
    let mut index = automerge::AutoCommit::new();
    for (id, kind) in rows {
        let id = ui::record::RecordId::parse(id).expect("a record id");
        assert!(
            ui::record::room::reference(&mut index, &id, &ui::record::RecordKind::new(*kind)),
            "the index names {id}"
        );
    }
    std::fs::write(staged.join(app_state::DOCUMENT_FILE), index.save()).expect("the index");
    std::fs::rename(&staged, &directory).expect("the index published");
}

/// The records `package`'s index of `room` names, read the way the workspace's
/// apps read them: the record set's own discovery over the index document.
fn known_in(package: &str, room: &str) -> Vec<String> {
    let Ok(mut store) = app_state::DocStore::open_room(package, room) else {
        return Vec::new();
    };
    let Some(index) = store.load().ok().flatten() else {
        return Vec::new();
    };
    let mut records = ui::record::RecordSet::in_memory().with_home(room);
    records.discover_room(&index);
    records
        .known_records()
        .iter()
        .map(|record| format!("{} {}", record.id.as_str(), record.kind.as_str()))
        .collect()
}

/// Wait for `done` as long as a loaded machine takes: the session writes the
/// reference on a worker of its own.
fn within(mut done: impl FnMut() -> bool) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    done()
}

/// The row titled `title` for `query`, reopening search until the session has
/// read the device's indexes again: a record another workspace holds is listed
/// once the worker following them has read that workspace's index.
fn find_row(session: &mut Session, query: &str, title: &str) -> ResourceSpec {
    let started = std::time::Instant::now();
    loop {
        invoke(
            session,
            DESK_RESOURCE,
            "open-search",
            serde_json::json!({ "query": query }),
        );
        for _ in 0..10 {
            session.step(Duration::from_millis(1)).expect("step");
        }
        if let Some(row) = row(session, title) {
            return row;
        }
        if started.elapsed() > Duration::from_secs(10) {
            let labels: Vec<String> = rows(session).into_iter().map(|row| row.label).collect();
            panic!("no row {title:?} for {query:?} among {labels:?}");
        }
    }
}

#[test]
fn a_record_of_another_workspace_is_found_by_name_and_brought_here() {
    let home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    // An app of this device already keeps an index, in a room the desk does
    // not show: the directories the session follows as more appear.
    write_index(
        &home,
        NOTES,
        "elsewhere",
        &[("file:/work/Receipts.odt", "document")],
    );
    let records = tilo_session::Records::of(home.data().to_path_buf());
    let mut session = session_over(Rc::new(Host::default()), Some(records));
    session.step(Duration::from_millis(1)).unwrap();

    // A second workspace, so a record can be held somewhere else than here.
    invoke(
        &mut session,
        CHROME_RESOURCE,
        "new-workspace",
        serde_json::json!({}),
    );
    until(&mut session, |session| workspaces(session).len() == 2);
    let here = active_room(&session);
    let (other_room, other_name) = workspaces(&session)
        .into_iter()
        .find(|(room, _)| *room != here)
        .expect("a workspace that is not the one in front");
    write_index(&home, NOTES, &other_room, &[(QUARTERLY, "document")]);

    // The row says which workspace holds the record.
    let row = find_row(&mut session, "quarterly", "Quarterly plan.odt");
    assert_eq!(
        row.presentation
            .and_then(|presentation| presentation.detail),
        Some(format!("in {other_name}, /work/Quarterly plan.odt")),
        "the row says the record is held in a workspace that is not this one"
    );

    // Choosing it names the record in this workspace: one reference, no copy.
    invoke(
        &mut session,
        row.id.as_str(),
        "choose",
        serde_json::json!({}),
    );
    until(&mut session, |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_none()
    });
    assert!(
        within(|| known_in(NOTES, &here).contains(&format!(
            "{QUARTERLY} {}",
            ui::record::RecordKind::new("document").as_str()
        ))),
        "this workspace holds the record: {:?}",
        known_in(NOTES, &here)
    );
}
