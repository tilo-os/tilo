//! A German desktop reads the surfaces that survive the one shell in German.
//! The session publishes its resources — the desk's rows, the windows and the chrome's own actions
//! — with the labels the action catalogue, the search and an agent read. Each label is resolved
//! through the toolkit's catalogue when it is published, so a session on a German desktop publishes
//! German: this walks what it publishes and reads it.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

mod common;

use tilo_actions::ResourceSpec;
use tilo_actions::door::{ActionDescriptor, survey};
use tilo_sheets::Seat as _;

use common::{private_home, seeded_session};

/// The words the person reads the actions of `prefix` 's resources by.
fn labelled<'a>(labels: &'a [(String, String)], prefix: &str) -> Vec<&'a str> {
    labels
        .iter()
        .filter(|(address, _)| address.starts_with(prefix))
        .map(|(_, label)| label.as_str())
        .collect()
}

/// Every action the session publishes, as its address and the words the person reads it by.
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

/// The name the session publishes `kind` under, wherever it stands in the tree it offers.
fn named(session: &tilo_session::Session, kind: &str) -> Option<String> {
    fn find(specs: &[ResourceSpec], kind: &str) -> Option<String> {
        specs.iter().find_map(|spec| {
            if spec.kind == kind {
                Some(spec.label.clone())
            } else {
                find(&spec.children, kind)
            }
        })
    }
    find(&session.resources(), kind)
}

/// Sets the desktop's language for everything resolved from here on.
fn speak(locale: &str) {
    ui::i18n::I18nManager::set_locale(locale);
}

#[test]
fn the_surviving_surfaces_speak_the_persons_language() {
    let root = private_home();
    // A session first: what publishes its catalogue publishes it once, and every label after this
    // is resolved in the language set below.
    let session = seeded_session(root.path());
    speak("de-DE");
    let labels = published(&session);
    speak("en-US");
    let in_english = published(&session);

    // The desk's own rows: what a person answers on their avatar, the tray item of a running
    // application, and stopping an agent. A row stands only while it applies, so this is what is up
    // now.
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

    // What the shell's own views carry, as the old session said it: no English where the old
    // session had German, and the German the old session said in its place.
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

    // A sheet names the resource it publishes, in the person's language.
    speak("de-DE");
    sheets_name_themselves(&session);

    // The same cards on an English desktop say what they declare.
    speak("en-US");
    let power = session.power_menu().expect("the power menu mounted");
    power.presentation().open(false);
    assert_eq!(named(&session, "power.session").as_deref(), Some("Power"));
    assert_eq!(
        named(&session, "uninstall.question").as_deref(),
        Some("Uninstall")
    );
}

/// A sheet names the resource it publishes: the name its declaration gives it,
/// read through the catalogue the moment it is published. Each card is put up
/// the way the session puts it up, and read off what it offers.
fn sheets_name_themselves(session: &tilo_session::Session) {
    let power = session.power_menu().expect("the power menu mounted");
    power.presentation().open(false);
    let settings = session.workspace_settings().expect("settings mounted");
    settings.presentation().open(false);
    let directory = session
        .workspace_directory()
        .expect("the directory mounted");
    directory.presentation().open(false);
    let portal = session.portal().expect("the portal sheet mounted");
    portal.presentation().open(false);
    let chooser = session.file_picker().expect("the file chooser mounted");
    chooser.presentation().open(false);
    let guide = session
        .shortcut_guide()
        .expect("the shortcut guide mounted");
    guide.presentation().open(false);
    let confirm = session
        .display_confirm()
        .expect("the confirm sheet mounted");
    confirm.presentation().open(false);
    let displays = session.display_sheet().expect("the display sheet mounted");
    displays.presentation().open(false);
    let add = session.add_app_sheet().expect("the add sheet mounted");
    add.presentation().open(false);
    let uninstall = session
        .uninstall_sheet()
        .expect("the uninstall sheet mounted");
    uninstall.presentation().open(false);

    for (kind, german) in [
        ("power.session", "Ein/Aus"),
        ("workspace-settings.lens", "Arbeitsbereich-Einstellungen"),
        ("workspace-directory.listing", "Öffentliche Arbeitsbereiche"),
        ("portal-sheet.question", "Systemfrage"),
        ("file-chooser.request", "Dateiauswahl"),
        ("shortcut-guide.guide", "Tastenkürzel"),
        ("display-confirm.question", "Anzeigeänderung"),
        ("display-sheet.layouts", "Anzeige"),
        ("add-app.question", "Zu den Apps hinzufügen"),
        ("uninstall.question", "Deinstallieren"),
    ] {
        assert_eq!(
            named(session, kind).as_deref(),
            Some(german),
            "a German desktop reads `{kind}` in German"
        );
    }
}
