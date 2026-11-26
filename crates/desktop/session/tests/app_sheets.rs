//! "Uninstall \<app\>?", as a person on a headless shell meets it: asking about a program they
//! downloaded asks them, listing every copy that goes, and their answer reaches the registry of
//! their downloaded programs — the one place their apps are kept.
//! Uninstalling takes every copy to the Trash and says so; keeping the app changes nothing; and
//! asking about an app that is not there any more says so rather than leaving a question open
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

use std::path::Path;
use std::time::Duration;

use crate::common::{
    person_answers, person_asked, person_asked_next, person_asked_now, person_declines,
};
use tilo_actions::ResourceSpec;
use tilo_session::{Options, Session};
use tilo_session_core::desktop::verbs::{SEARCH_RESOURCE, SEARCH_ROW_KIND, SHELL_RESOURCE};

/// A session on the headless backend, taking no part in the peer network.
fn session() -> Session {
    crate::common::headless(Options {
        collab: false,
        asks: Some(person_asked()),
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

/// The titles and bodies of the notices the shell shows now.
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
        .invoke(SHELL_RESOURCE, "search", &arguments)
        .expect("the shell opens search");
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
    let still = tilo_session_core::desktop::i18n::t("desktop-search-searching");
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
    crate::common::wait_for(session, what, |session| done(session).then_some(()));
}

/// The uninstall question the person is asked next: its handle and the copies it says go.
#[track_caller]
fn asked_to_uninstall(session: &mut Session, name: &str) -> (String, Vec<String>) {
    let question = person_asked_next(session);
    assert_eq!(question.kind, "app.uninstall", "the uninstall question");
    assert_eq!(question.details["name"], name, "naming the app");
    let copies = question.details["copies"]
        .as_array()
        .expect("the copies that go")
        .iter()
        .map(|copy| copy.as_str().expect("a file").to_owned())
        .collect();
    (question.handle, copies)
}

/// The person says yes to the question that stands.
fn uninstall(session: &mut Session) {
    person_answers(session, tilo_service::Answer::default());
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
    let (_, copies) = asked_to_uninstall(&mut session, "Tilo Imager");
    for copy in [&applications, &downloads] {
        assert!(
            copies
                .iter()
                .any(|listed| Path::new(listed) == copy.as_path()),
            "the question lists {}: {copies:?}",
            copy.display()
        );
    }

    uninstall(&mut session);
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
        person_asked_now().is_empty(),
        "and nothing is asked any more"
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
    let (_, copies) = asked_to_uninstall(&mut session, "Dropped Tool");
    assert_eq!(copies.len(), 1, "one file goes");

    uninstall(&mut session);
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
    let (handle, _) = asked_to_uninstall(&mut session, "Tilo Imager");
    assert!(
        person_declines(&handle),
        "the question waits for its answer"
    );
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
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
        person_asked_now().is_empty(),
        "no question is left in place about it"
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
/// question on the shell, and answering it uninstalls the app.
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
    asked_to_uninstall(&mut session, "Tilo Imager");
    assert!(file.is_file(), "nothing goes before the person answers");

    uninstall(&mut session);
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
    asked_to_uninstall(&mut session, "Tilo Imager");
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
    asked_to_uninstall(&mut session, "Dropped Tool");
    uninstall(&mut session);
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
    assert!(person_asked_now().is_empty(), "the question is answered");
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
    ) -> Result<tilo_service::Started, tilo_actions::ApiError> {
        Err(tilo_actions::ApiError::Unavailable {
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
