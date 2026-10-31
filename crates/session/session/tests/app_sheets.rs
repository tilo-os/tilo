//! "Uninstall \<app\> and remove its data?", as a person on a headless desk
//! meets it: asking about a program they downloaded puts the question on the
//! desk, listing every copy that goes, and its answers reach the registry of
//! their downloaded programs — the one place their apps are kept.
//!
//! Uninstalling takes every copy to the Trash and says so; keeping the app
//! changes nothing; and asking about an app that is not there any more says
//! so rather than leaving a question standing about it.
//!
//! Search's Uninstall row asks that same question, and an uninstall that
//! cannot move the app's file says why instead of quietly doing nothing.

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

use std::path::Path;
use std::time::{Duration, Instant};

use tilo_actions::ResourceSpec;
use tilo_compositor::OutputConfig;
use tilo_desktop::desktop_actions::{DESK_RESOURCE, SEARCH_RESOURCE, SEARCH_ROW_KIND};
use tilo_session::{Options, Session};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// A session on the headless backend, taking no part in the collaboration
/// plane.
fn session() -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// A program the person added to their apps, made of `copies` (the first is
/// the one it starts). Returns the desktop id it was added as.
fn app(home: &tilo_test_env::TestHome, key: &str, name: &str, copies: &[&Path]) -> String {
    use std::os::unix::fs::PermissionsExt as _;

    for copy in copies {
        std::fs::create_dir_all(copy.parent().expect("a folder")).expect("the folder");
        std::fs::write(copy, b"#!/bin/sh\nexit 0\n").expect("the program");
        let mut permissions = std::fs::metadata(copy).expect("the program").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(copy, permissions).expect("a runnable program");
    }
    let store = desktop_entry::downloaded::Store::in_data_home(home.data());
    let identity = desktop_entry::downloaded::Identity {
        key: key.to_owned(),
        name: name.to_owned(),
        icon: None,
        comment: None,
        categories: Vec::new(),
        keywords: Vec::new(),
        mime_types: Vec::new(),
        startup_wm_class: None,
        field_code: None,
    };
    // Each add points the entry at its copy and keeps the one before it.
    let mut desktop_id = String::new();
    for copy in copies {
        desktop_id = store
            .add(
                &identity,
                &desktop_entry::downloaded::Program {
                    path: (*copy).to_path_buf(),
                    kind: desktop_entry::downloaded::ProgramKind::AppImage,
                },
            )
            .expect("the entry");
    }
    desktop_id
}

/// The resource publishing `action` among everything the session publishes.
fn publisher(session: &Session, action: &str) -> Option<String> {
    fn find(resources: &[ResourceSpec], action: &str) -> Option<String> {
        resources.iter().find_map(|resource| {
            if resource.actions.iter().any(|offered| offered.id == action) {
                Some(resource.id.as_str().to_owned())
            } else {
                find(&resource.children, action)
            }
        })
    }
    find(&session.resources(), action)
}

/// Whether the session publishes the app `desktop_id`, which it does while
/// the person has that app.
fn published(session: &Session, desktop_id: &str) -> bool {
    fn find(resources: &[ResourceSpec], id: &str) -> bool {
        resources
            .iter()
            .any(|resource| resource.id.as_str() == id || find(&resource.children, id))
    }
    find(
        &session.resources(),
        &tilo_actions::remote::downloaded_apps::app(desktop_id),
    )
}

/// The titles and bodies of the notices the desk shows now.
fn notices(session: &Session) -> Vec<(String, String)> {
    session
        .notifications()
        .log()
        .active()
        .iter()
        .map(|notice| (notice.title.clone(), notice.body.clone()))
        .collect()
}

/// The rows the open search shows now.
fn rows(session: &Session) -> Vec<ResourceSpec> {
    fn find(resources: &[ResourceSpec], id: &str) -> Option<ResourceSpec> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource.clone())
            } else {
                find(&resource.children, id)
            }
        })
    }
    find(&session.resources(), SEARCH_RESOURCE)
        .map(|search| search.children)
        .unwrap_or_default()
}

/// Search for `query`, as the person does, and wait for its rows.
fn search(session: &mut Session, query: &str) {
    let arguments = serde_json::json!({"query": query})
        .as_object()
        .expect("an object")
        .clone();
    session
        .invoke(DESK_RESOURCE, "open-search", &arguments)
        .expect("the desk opens search");
    until(session, "search's rows", |session| {
        !rows(session).is_empty()
    });
}

/// Press the search row titled `title`, as the person does.
#[track_caller]
fn choose_row(session: &mut Session, title: &str) {
    until(session, &format!("the row {title}"), |session| {
        row(session, title).is_some()
    });
    let chosen = row(session, title).expect("the row asked for");
    let arguments = serde_json::Map::new();
    session
        .invoke(chosen.id.as_str(), "choose", &arguments)
        .expect("the row is chosen");
}

/// The open search's row titled `title`.
fn row(session: &Session, title: &str) -> Option<ResourceSpec> {
    rows(session)
        .into_iter()
        .find(|row| row.kind == SEARCH_ROW_KIND && row.label == title)
}

/// Run the session until `done` holds, or fail saying what never happened.
#[track_caller]
fn until(session: &mut Session, what: &str, done: impl Fn(&Session) -> bool) {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(60))
        .expect("a deadline");
    loop {
        session.step(Duration::from_millis(10)).unwrap();
        if done(session) {
            return;
        }
        assert!(Instant::now() < deadline, "{what}");
    }
}

/// Press the question's `action`, as the person does.
fn answer(session: &mut Session, action: &str) {
    let sheet = publisher(session, action).unwrap_or_else(|| panic!("no question offers {action}"));
    assert_eq!(
        sheet, "uninstall.question",
        "the answer is the uninstall question's"
    );
    let arguments = serde_json::json!({})
        .as_object()
        .expect("an object")
        .clone();
    session
        .invoke(&sheet, action, &arguments)
        .expect("the session takes the answer");
}

#[test]
fn uninstalling_an_app_takes_every_copy_to_the_trash_and_says_so() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().expect("a home");
    let applications = home.home().join("Applications/Tilo Imager.AppImage");
    let downloads = home.home().join("Downloads/tilo-imager.AppImage");
    let desktop_id = app(
        &home,
        "tilo-imager",
        "Tilo Imager",
        &[&applications, &downloads],
    );
    let mut session = session();
    until(&mut session, "the app is in their apps", |session| {
        published(session, &desktop_id)
    });

    // Asked to uninstall it, the question names the app and lists both
    // copies that go.
    session.ask_uninstall(&desktop_id);
    until(&mut session, "the question", |session| {
        publisher(session, "confirm").is_some()
    });
    let labels = session.accessible_labels();
    assert!(
        labels.iter().any(|label| label == "Uninstall Tilo Imager?"),
        "the question names the app: {labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|label| label.contains("All 2 copies go to the Trash")),
        "the question says what goes: {labels:?}"
    );
    for copy in [&applications, &downloads] {
        let name = copy.file_name().expect("a name").to_string_lossy();
        assert!(
            labels.iter().any(|label| label.contains(name.as_ref())),
            "the question lists {name}: {labels:?}"
        );
    }

    answer(&mut session, "confirm");
    until(&mut session, "both copies to go", |_| {
        !applications.exists() && !downloads.exists()
    });
    until(&mut session, "the app to leave their apps", |session| {
        !published(session, &desktop_id)
    });
    let trashed = home.data().join("Trash/files");
    assert!(
        trashed.join("Tilo Imager.AppImage").is_file(),
        "the app's file is in the Trash"
    );
    assert!(
        trashed.join("tilo-imager.AppImage").is_file(),
        "and so is its copy"
    );
    assert_eq!(
        notices(&session),
        [(
            "Uninstalled Tilo Imager".to_owned(),
            "All its copies are in the Trash.".to_owned()
        )]
    );
    assert!(
        publisher(&session, "confirm").is_none(),
        "and the question is taken down"
    );
}

#[test]
fn uninstalling_the_only_copy_says_its_file_is_in_the_trash() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().expect("a home");
    let file = home.home().join("Applications/Dropped.AppImage");
    let desktop_id = app(&home, "Dropped", "Dropped Tool", &[&file]);
    let mut session = session();
    until(&mut session, "the app is in their apps", |session| {
        published(session, &desktop_id)
    });

    session.ask_uninstall(&desktop_id);
    until(&mut session, "the question", |session| {
        publisher(session, "confirm").is_some()
    });
    let labels = session.accessible_labels();
    assert!(
        labels
            .iter()
            .any(|label| label.contains("Its file goes to the Trash")),
        "the question says what goes: {labels:?}"
    );

    answer(&mut session, "confirm");
    until(&mut session, "the app to leave their apps", |session| {
        !published(session, &desktop_id)
    });
    assert!(!file.exists(), "its file went");
    assert_eq!(
        notices(&session),
        [(
            "Uninstalled Dropped Tool".to_owned(),
            "Its file is in the Trash.".to_owned()
        )]
    );
}

#[test]
fn keeping_an_app_leaves_it_and_its_file_alone() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().expect("a home");
    let file = home.home().join("Applications/Tilo Imager.AppImage");
    let desktop_id = app(&home, "tilo-imager", "Tilo Imager", &[&file]);
    let mut session = session();
    until(&mut session, "the app is in their apps", |session| {
        published(session, &desktop_id)
    });

    session.ask_uninstall(&desktop_id);
    until(&mut session, "the question", |session| {
        publisher(session, "confirm").is_some()
    });
    answer(&mut session, "cancel");
    until(&mut session, "the question to go", |session| {
        publisher(session, "confirm").is_none()
    });
    assert!(file.is_file(), "the app's file stays");
    assert!(
        published(&session, &desktop_id),
        "and the app stays in their apps"
    );
    assert!(notices(&session).is_empty(), "nothing to report");
}

#[test]
fn asking_about_an_app_that_is_not_there_any_more_says_so() {
    let _settled = ui::motion::testing::settled();
    let _home = tilo_test_env::TestHome::new().expect("a home");
    let mut session = session();
    session.ask_uninstall("tilo-downloaded-not-here");
    until(&mut session, "the notice", |session| {
        !notices(session).is_empty()
    });
    assert!(
        publisher(&session, "confirm").is_none(),
        "no question is left standing about it"
    );
    assert_eq!(
        notices(&session),
        [(
            "App not found".to_owned(),
            "It's no longer in your apps.".to_owned()
        )]
    );
}

/// Search lists the app the person downloaded with the rows that act on it,
/// under the app's own row; choosing "Uninstall Tilo Imager" puts the
/// question on the desk, and answering it uninstalls the app.
#[test]
fn the_uninstall_row_of_a_downloaded_app_asks_the_question() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().expect("a home");
    let file = home.home().join("Applications/Tilo Imager.AppImage");
    let desktop_id = app(&home, "tilo-imager", "Tilo Imager", &[&file]);
    let mut session = session();
    until(&mut session, "the app is in their apps", |session| {
        published(session, &desktop_id)
    });

    search(&mut session, "tilo imager");
    let uninstalling: Vec<String> = rows(&session)
        .into_iter()
        .map(|row| row.label)
        .filter(|label| label.to_lowercase().contains("uninstall"))
        .collect();
    assert_eq!(
        uninstalling,
        ["Uninstall Tilo Imager"],
        "one row uninstalls the app, and it is the one that asks"
    );
    choose_row(&mut session, "Uninstall Tilo Imager");
    until(&mut session, "the question", |session| {
        publisher(session, "confirm").is_some()
    });
    let labels = session.accessible_labels();
    assert!(
        labels.iter().any(|label| label == "Uninstall Tilo Imager?"),
        "the question is the uninstall question: {labels:?}"
    );
    assert!(file.is_file(), "nothing goes before the person answers");

    answer(&mut session, "confirm");
    until(&mut session, "the app to leave their apps", |session| {
        !published(session, &desktop_id)
    });
    assert!(!file.exists(), "its file went to the Trash");
    assert_eq!(
        notices(&session),
        [(
            "Uninstalled Tilo Imager".to_owned(),
            "Its file is in the Trash.".to_owned()
        )]
    );
}

/// "Uninstall an app", the question's own row, asks about the app by the
/// name the person types: the same question the app's row asks.
#[test]
fn the_uninstall_question_takes_the_app_by_name() {
    use tilo_desktop_search::providers::downloaded::{APP_PARAM, APPS_RESOURCE, ASK_UNINSTALL};

    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().expect("a home");
    let file = home.home().join("Applications/Tilo Imager.AppImage");
    let desktop_id = app(&home, "tilo-imager", "Tilo Imager", &[&file]);
    let mut session = session();
    until(&mut session, "the app is in their apps", |session| {
        published(session, &desktop_id)
    });

    let mut arguments = serde_json::Map::new();
    arguments.insert(APP_PARAM.to_owned(), "tilo imager".into());
    session
        .invoke(APPS_RESOURCE, ASK_UNINSTALL, &arguments)
        .expect("the question is offered");
    until(&mut session, "the question", |session| {
        publisher(session, "confirm").is_some()
    });
    let labels = session.accessible_labels();
    assert!(
        labels.iter().any(|label| label == "Uninstall Tilo Imager?"),
        "the name finds the app: {labels:?}"
    );
    assert!(file.is_file(), "nothing goes before the person answers");
}

/// An uninstall that cannot move the app's file says so, and leaves the app
/// and its file where they were.
#[test]
fn an_uninstall_that_cannot_trash_the_file_says_so() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().expect("a home");
    let file = home.home().join("Applications/Dropped.AppImage");
    let desktop_id = app(&home, "Dropped", "Dropped Tool", &[&file]);
    // Nothing can be moved into a Trash that is not a folder.
    std::fs::write(home.data().join("Trash"), b"a file where the Trash goes").expect("the Trash");
    let mut session = session();
    until(&mut session, "the app is in their apps", |session| {
        published(session, &desktop_id)
    });

    session.ask_uninstall(&desktop_id);
    until(&mut session, "the question", |session| {
        publisher(session, "confirm").is_some()
    });
    answer(&mut session, "confirm");
    until(&mut session, "the notice", |session| {
        !notices(session).is_empty()
    });
    let shown = notices(&session);
    let [(title, body)] = shown.as_slice() else {
        panic!("one notice says it: {shown:?}");
    };
    assert_eq!(title, "Couldn't uninstall Dropped Tool");
    assert!(
        body.contains("Trash"),
        "the notice says what stopped it: {body}"
    );
    assert!(file.is_file(), "the app's file stays");
    assert!(
        published(&session, &desktop_id),
        "and the app stays in their apps"
    );
    assert!(
        publisher(&session, "confirm").is_none(),
        "the question is taken down"
    );
}
