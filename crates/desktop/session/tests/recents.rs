//! What a person opened lately is the system's: the layout's own history of what its windows
//! showed, listed per app or type, newest first, in the shell's history surface. An app opens it
//! narrowed to itself through `desktop:chrome#recents` (`session.recents(app: ...)` in
//! tilolang), and clearing it is `shell:layout#clear-recents`, which hides the list for the
//! person and leaves the workspace's history whole.
//!
//! Every step is the production path: the layout's `open`, the session's reading the shell's
//! views draw, the chrome's and the layout's published actions, and a press on an app's button.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use serde_json::json;
use tilo_session::Session;
use tilo_session_core::layout::LayoutRecentReading;
use tilo_shell_surface::shell_part::OVERLAYS;

use crate::common::{layout, places, press, private_home, reached, seeded_session, until};

const NOTES: &str = "so.tilo.notes";
const TUBE: &str = "so.tilo.tube";

/// Published notes of the seeded workspace, by id, at most `count`.
fn notes(session: &Session, count: usize) -> Vec<String> {
    fn find(resources: &[tilo_actions::ResourceSpec], out: &mut Vec<String>) {
        for resource in resources {
            if resource.kind.ends_with(".Note") {
                out.push(resource.id.as_str().to_owned());
            }
            find(&resource.children, out);
        }
    }
    let mut out = Vec::new();
    find(&session.resources(), &mut out);
    out.truncate(count);
    assert_eq!(out.len(), count, "the seed holds {count} notes: {out:?}");
    out
}

/// The person's recents of `app`, as the shell reads them.
fn recents_of(session: &Session, app: &str) -> Vec<LayoutRecentReading> {
    layout(session)
        .recents
        .into_iter()
        .filter(|recent| recent.origin_app == app)
        .collect()
}

/// Open `resource` through the layout's own `open`, as a reference's press and an agent do, and
/// wait for its place.
fn open(session: &mut Session, resource: &str) {
    session
        .invoke(
            "shell:layout",
            "open",
            json!({ "resource": resource }).as_object().unwrap(),
        )
        .unwrap_or_else(|error| panic!("{resource} opens: {error:?}"));
    until(session, "the opened resource's place", |session| {
        places(session)
            .iter()
            .any(|place| place.resource == resource)
    });
}

/// What the shell's surface over the windows says it shows, and what it is narrowed to.
fn surface(session: &Session) -> (String, String) {
    let overlays = session.read(OVERLAYS).expect("the overlays read");
    (
        overlays["here"].as_str().unwrap_or_default().to_owned(),
        overlays["history_app"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    )
}

/// Three notes opened one after another are the app's recents, newest first, each once; the
/// app's scoped address opens the shell's history on them; clearing them empties the person's
/// list of that app alone, and what they open after shows again.
#[test]
fn an_apps_recents_list_what_was_opened_and_clear_hides_it_for_the_person() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let opened = notes(&session, 3);
    for note in &opened {
        open(&mut session, note);
    }
    let listed_all = reached(&mut session, |session| {
        recents_of(session, NOTES).len() >= 3
    });
    assert!(
        listed_all,
        "the three notes {opened:?} in Notes' recents; the person's recents are {:?}, the places \
         {:?}",
        layout(&session).recents,
        places(&session)
            .into_iter()
            .map(|place| place.resource)
            .collect::<Vec<_>>()
    );
    let listed: Vec<String> = recents_of(&session, NOTES)
        .into_iter()
        .map(|recent| recent.resource)
        .collect();
    let newest_first: Vec<String> = opened.iter().rev().cloned().collect();
    assert_eq!(
        listed[..3],
        newest_first[..],
        "the newest open first, each note once"
    );
    let recents = recents_of(&session, NOTES);
    assert!(
        recents.iter().all(|recent| recent.mine),
        "the person opened them: {recents:?}"
    );
    // The notes are entries of one notebook: each open after the first showed its note in the one
    // window that notebook has, so there is one Notes window, on the newest note, and the history
    // lists every note it showed.
    let notes_places: Vec<String> = places(&session)
        .into_iter()
        .map(|place| place.resource)
        .filter(|resource| resource.starts_with(&format!("tl:{NOTES}:")))
        .collect();
    assert_eq!(
        notes_places,
        [newest_first[0].clone()],
        "one Notes window, showing the note opened last"
    );
    // Opening the oldest again brings it to the front, still once.
    open(&mut session, &opened[0]);
    until(&mut session, "the note opened again first", |session| {
        recents_of(session, NOTES)
            .first()
            .is_some_and(|recent| recent.resource == opened[0])
    });
    assert_eq!(
        recents_of(&session, NOTES)
            .iter()
            .filter(|recent| recent.resource == opened[0])
            .count(),
        1
    );

    // The app's scoped way into the history: the surface opens narrowed to Notes, and lists them.
    session
        .invoke(
            "desktop:chrome",
            "recents",
            json!({ "app": NOTES }).as_object().unwrap(),
        )
        .unwrap_or_else(|error| panic!("the recents open: {error:?}"));
    until(&mut session, "the history narrowed to Notes", |session| {
        surface(session) == ("history".to_owned(), NOTES.to_owned())
    });
    let newest = recents_of(&session, NOTES)[0].title.clone();
    until(
        &mut session,
        "the newest note's row in the history",
        |session| session.shell_says(&newest) && session.shell_says("Opened by you"),
    );

    // Clearing Notes' recents empties the person's list of Notes, and nothing else of the layout.
    let places_before = places(&session).len();
    session
        .invoke(
            "shell:layout",
            "clear-recents",
            json!({ "app": NOTES }).as_object().unwrap(),
        )
        .unwrap_or_else(|error| panic!("the recents clear: {error:?}"));
    until(&mut session, "Notes' recents cleared", |session| {
        recents_of(session, NOTES).is_empty()
    });
    until(&mut session, "the history says it is empty", |session| {
        session.shell_says("Nothing opened lately")
    });
    assert_eq!(
        places(&session).len(),
        places_before,
        "clearing recents closes no window"
    );

    // What the person opens after the clear is theirs to see again; what came before stays hidden.
    // An open is the time its change says, in seconds: the next one stands after the clear. The
    // Notes place shows the oldest note now, so opening another re-points it, an open like any.
    std::thread::sleep(std::time::Duration::from_millis(1_100));
    let later = opened[1].clone();
    open(&mut session, &later);
    until(&mut session, "the note opened after the clear", |session| {
        recents_of(session, NOTES)
            .iter()
            .map(|recent| recent.resource.as_str())
            .eq([later.as_str()])
    });
}

/// A tilolang app's own recents button opens the shell's history narrowed to that app: the
/// video app's header button runs `session.recents(app: "so.tilo.tube")`, which is the chrome's
/// `recents`.
#[test]
fn a_tilolang_apps_recents_button_opens_the_history_scoped_to_it() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let prefix = format!("tl:{TUBE}:root-");
    let tube = session
        .resources()
        .into_iter()
        .map(|spec| spec.id.as_str().to_owned())
        .find(|id| id.starts_with(&prefix))
        .expect("YouTube's root is published");
    open(&mut session, &tube);
    let shown = reached(&mut session, |session| {
        session.shell_says("Recently opened")
    });
    assert!(
        shown,
        "YouTube's recents button; the shell's refusals: {:?}; the session says:\n{}",
        session.shell_refusals(),
        session.accessible_labels().join("\n")
    );
    press(&mut session, "Recently opened");
    until(&mut session, "the history narrowed to YouTube", |session| {
        surface(session) == ("history".to_owned(), TUBE.to_owned())
    });
}
