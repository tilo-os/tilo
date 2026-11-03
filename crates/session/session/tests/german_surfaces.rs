//! A German desktop reads the surfaces that survive the one shell in German.
//!
//! The session publishes its resources — the desk's rows, the agent runs and
//! the chrome's own actions — with the labels the action catalogue, the search
//! and an agent read. Each label is resolved through the toolkit's catalogue
//! when it is published, so a session on a German desktop publishes German:
//! this walks what it publishes and reads it.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

mod common;

use tilo_actions::door::{ActionDescriptor, survey};

use common::{private_home, seeded_session};

/// The action `action` of `resource`, as the person reads it.
#[track_caller]
fn label_of<'a>(labels: &'a [(String, String)], resource: &str, action: &str) -> &'a str {
    labels
        .iter()
        .find(|(address, _)| address == &format!("{resource}/{action}"))
        .map_or_else(
            || panic!("{resource}/{action} is published; published: {labels:?}"),
            |(_, label)| label.as_str(),
        )
}

/// The words the person reads the actions of `prefix`'s resources by.
fn labelled<'a>(labels: &'a [(String, String)], prefix: &str) -> Vec<&'a str> {
    labels
        .iter()
        .filter(|(address, _)| address.starts_with(prefix))
        .map(|(_, label)| label.as_str())
        .collect()
}

/// Every action the session publishes, as its address and the words the
/// person reads it by.
fn published(session: &tilo_session::Session) -> Vec<(String, String)> {
    survey(&session.resources())
        .into_iter()
        .map(
            |ActionDescriptor {
                 address,
                 action_label,
                 ..
             }| {
                (
                    format!("{}/{}", address.resource, address.action),
                    action_label,
                )
            },
        )
        .collect()
}

/// Sets the desktop's language for everything resolved from here on.
fn speak(locale: &str) {
    ui::i18n::I18nManager::set_locale(locale);
}

#[test]
fn the_surviving_surfaces_speak_the_persons_language() {
    let root = private_home();
    // A session first: what publishes its catalogue publishes it once, and
    // every label after this is resolved in the language set below.
    let session = seeded_session(root.path());
    speak("de-DE");
    let labels = published(&session);
    speak("en-US");
    let in_english = published(&session);

    // The desk's own rows: what a person answers on their avatar, the tray
    // item of a running application, and stopping an agent. A row stands
    // only while it applies, so this is what is up now.
    let desk = labelled(&labels, "session:desk/");
    assert!(
        desk.contains(&"Stoppen"),
        "the desk stops an agent in German: {desk:?}"
    );
    for english in ["Answer", "Ask me later", "Open", "Stop"] {
        assert!(
            !desk.contains(&english),
            "a desk row reads `{english}`: {desk:?}"
        );
    }

    // The agent runs.
    assert_eq!(
        label_of(&labels, "agent-runs", "ask"),
        "Einen Agenten fragen"
    );
    let runs = labelled(&labels, "agent-runs/");
    for english in [
        "Ask an agent",
        "Send a message",
        "Stop",
        "Record a step",
        "Record an answer",
    ] {
        assert!(
            !runs.contains(&english),
            "a run's action reads `{english}`: {runs:?}"
        );
    }

    // What the shell's own views carry, as the old session said it: no
    // English where the old session had German, and the German the old
    // session said in its place.
    for (english, german) in [
        ("Show a readout", "Eine Anzeige zeigen"),
        ("About this computer", "Über diesen Computer"),
        ("Catch up", "Auf dem Laufenden bleiben"),
    ] {
        assert!(
            !labels.iter().any(|(_, label)| label == english),
            "`{english}` is published on a German desktop: {labels:?}"
        );
        if in_english.iter().any(|(_, label)| label == english) {
            assert!(
                labels.iter().any(|(_, label)| label == german),
                "`{german}` is published where `{english}` is: {labels:?}"
            );
        }
    }
}
