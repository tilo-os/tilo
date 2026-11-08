//! "Uninstall \<app\> and remove its data?", as a person on a headless desk meets it: asking about
//! a program they downloaded puts the question on the desk, listing every copy that goes, and its
//! answers reach the registry of their downloaded programs — the one place their apps are kept.
//! Uninstalling takes every copy to the Trash and says so; keeping the app changes nothing; and
//! asking about an app that is not there any more says so rather than leaving a question standing
//! about it.
//! Search's Uninstall row asks that same question, and an uninstall that cannot move the app's file
//! says why instead of quietly doing nothing.

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

mod common;

use std::path::Path;
use std::time::Duration;

use tilo_actions::ResourceSpec;
use tilo_desktop::desk_verbs::{DESK_RESOURCE, SEARCH_RESOURCE, SEARCH_ROW_KIND};
use tilo_session::{Options, Session};

/// A session on the headless backend, taking no part in the collaboration plane.
fn session() -> Session {
    crate::common::headless(Options {
        collab: false,
        ..Options::default()
    })
}

/// A program the person added to their apps, made of `copies` (the first is the one it starts).
/// Returns the desktop id it was added as.
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

/// Whether the session publishes the app `desktop_id` , which it does while the person has that
/// app.
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

/// Search for `query` , as the person does, and wait for its rows to be final: the open search says
/// it is still searching until every source has answered and its rows are cut to the card, so a row
/// read before that may be all a first source had to say.
fn search(session: &mut Session, query: &str) {
    let arguments = serde_json::json!({"query": query})
        .as_object()
        .expect("an object")
        .clone();
    session
        .invoke(DESK_RESOURCE, "search", &arguments)
        .expect("the desk opens search");
    until(session, "search's rows", |session| {
        !rows(session).is_empty() && !searching(session)
    });
}

/// Whether the open search still says it is searching.
fn searching(session: &Session) -> bool {
    fn find(resources: &[ResourceSpec], id: &str) -> Option<ResourceSpec> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource.clone())
            } else {
                find(&resource.children, id)
            }
        })
    }
    let still = tilo_desktop::i18n::t("desktop-search-searching");
    find(&session.resources(), SEARCH_RESOURCE)
        .and_then(|search| search.presentation)
        .and_then(|presentation| presentation.detail)
        .is_some_and(|detail| detail == still)
}

/// Press the search row titled `title` , as the person does.
#[track_caller]
fn choose_row(session: &mut Session, title: &str) {
    until(session, &format!("the row {title}"), |session| {
        row(session, title).is_some()
    });
    let chosen = row(session, title).expect("the row asked for");
    let arguments = serde_json::Map::new();
    session
        .invoke(chosen.id.as_str(), "choose-search-row", &arguments)
        .expect("the row is chosen");
}

/// The open search's row titled `title` .
fn row(session: &Session, title: &str) -> Option<ResourceSpec> {
    rows(session)
        .into_iter()
        .find(|row| row.kind == SEARCH_ROW_KIND && row.label == title)
}

/// Run the session until `done` holds, or fail saying what never happened.
#[track_caller]
fn until(session: &mut Session, what: &str, mut done: impl FnMut(&Session) -> bool) {
    common::wait_for(session, what, |session| done(session).then_some(()));
}

/// Press the question's `action` , as the person does.
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

    // Asked to uninstall it, the question names the app and lists both copies that go.
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
    // What it did is said once the registry reads the app as gone, which is a pass after the app
    // leaves their apps.
    until(&mut session, "the notice saying where it went", |session| {
        !notices(session).is_empty()
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
    // What it did is said once the registry reads the app as gone, which is a pass after the app
    // leaves their apps.
    until(&mut session, "the notice saying where it went", |session| {
        !notices(session).is_empty()
    });
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
    until(&mut session, "the question's title", |session| {
        session
            .accessible_labels()
            .iter()
            .any(|label| label == "Uninstall Tilo Imager?")
    });
    assert!(file.is_file(), "nothing goes before the person answers");

    answer(&mut session, "confirm");
    until(&mut session, "the app to leave their apps", |session| {
        !published(session, &desktop_id)
    });
    assert!(!file.exists(), "its file went to the Trash");
    // What it did is said once the registry reads the app as gone, which is a pass after the app
    // leaves their apps.
    until(&mut session, "the notice saying where it went", |session| {
        !notices(session).is_empty()
    });
    assert_eq!(
        notices(&session),
        [(
            "Uninstalled Tilo Imager".to_owned(),
            "Its file is in the Trash.".to_owned()
        )]
    );
}

/// "Uninstall an app", the question's own row, asks about the app by the name the person types: the
/// same question the app's row asks.
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
    until(&mut session, "the question's title", |session| {
        session
            .accessible_labels()
            .iter()
            .any(|label| label == "Uninstall Tilo Imager?")
    });
    assert!(file.is_file(), "nothing goes before the person answers");
}

/// An uninstall that cannot move the app's file says so, and leaves the app and its file where they
/// were.
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

/// A registry of the person's apps that publishes a reading this build does not read: what a
/// registry a newer build wrote reads like here.
struct NotReadBack;

impl tilo_service::Service for NotReadBack {
    fn resources(&self) -> Vec<ResourceSpec> {
        Vec::new()
    }

    fn read(&self, resource: &str) -> Option<serde_json::Value> {
        (resource == tilo_app_launch::downloaded::RESOURCE)
            .then(|| serde_json::json!({"apps": "kept by a newer build"}))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        _arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_service::Started, tilo_actions::DoorError> {
        Err(tilo_actions::DoorError::Unavailable {
            code: "not-read-back".to_owned(),
            reason: format!("{resource}:{action} is not read by this build"),
        })
    }
}

#[test]
fn a_registry_that_does_not_read_back_says_nothing_was_removed() {
    let _settled = ui::motion::testing::settled();
    let _home = tilo_test_env::TestHome::new().expect("a home");
    // A registry of theirs this build cannot read: their apps would simply be gone from the
    // launcher, so they are told once instead.
    let mut session = crate::common::headless(Options {
        collab: false,
        services: vec![Box::new(NotReadBack)],
        ..Options::default()
    });
    until(&mut session, "the notice", |session| {
        !notices(session).is_empty()
    });
    assert_eq!(
        notices(&session),
        [(
            "Your downloaded apps could not be read".to_owned(),
            "This computer could not read the list of programs you added to \
             your apps, so they are not listed now. Nothing was removed: they \
             are still on this computer."
                .to_owned()
        )]
    );
    // Told once: the registry is read again every pass of the session's loop.
    for _ in 0..50 {
        session.step(Duration::from_millis(10)).expect("a pass");
    }
    assert_eq!(notices(&session).len(), 1, "said once");
}
