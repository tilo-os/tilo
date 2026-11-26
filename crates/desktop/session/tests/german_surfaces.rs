//! A German desktop reads the surfaces that survive the one shell in German.
//! The session publishes its resources — the shell's rows, the windows and the chrome's own actions
//! — with the labels the action catalogue, the search and an agent read. Each label is resolved
//! through the toolkit's catalogue when it is published, so a session on a German desktop publishes
//! German: this walks what it publishes and reads it.

#![cfg(target_os = "linux")]

use tilo_actions::ResourceSpec;
use tilo_actions::api::{ActionDescriptor, survey};
use tilo_sheets::SheetState as _;

use crate::common::{private_home, seeded_session};

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

/// The label the session publishes the resource `id` under, wherever it stands in the tree it
/// offers.
fn named_by_id(session: &tilo_session::Session, id: &str) -> Option<String> {
    fn find(specs: &[ResourceSpec], id: &str) -> Option<String> {
        specs.iter().find_map(|spec| {
            if spec.id.0 == id {
                Some(spec.label.clone())
            } else {
                find(&spec.children, id)
            }
        })
    }
    find(&session.resources(), id)
}

/// Sets the desktop's language for everything resolved from here on.
fn speak(locale: &str) {
    tilo_ui_base::i18n::I18nManager::set_locale(locale);
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

    // The shell's own rows: what a person answers on their avatar, the tray item of a running
    // application, and stopping an agent. A row stands only while it applies, so this is what is up
    // now.
    let shell: Vec<&str> = ["desktop:overlays/", "session:tray/", "session:agents/"]
        .into_iter()
        .flat_map(|prefix| labelled(&labels, prefix))
        .collect();
    assert!(
        shell.contains(&"Stoppen"),
        "the shell stops an agent in German: {shell:?}"
    );
    for english in ["Answer", "Ask me later", "Open", "Stop"] {
        assert!(
            !shell.contains(&english),
            "a shell row reads `{english}`: {shell:?}"
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
    assert_eq!(
        named(&session, "desktop.power-menu").as_deref(),
        Some("Power menu")
    );
    assert_eq!(
        named(&session, "uninstall.question").as_deref(),
        Some("Uninstall")
    );
}

/// A sheet names the resource it publishes: the name its declaration gives it,
/// read through the catalogue the moment it is published. Each card is put up
/// the way the session puts it up, and read off what it offers.
fn sheets_name_themselves(session: &tilo_session::Session) {
    let settings = session.workspace_settings().expect("settings mounted");
    settings.presentation().open(false);
    let directory = session
        .workspace_directory()
        .expect("the directory mounted");
    directory.presentation().open(false);
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
        ("desktop.power-menu", "Ein/Aus-Menü"),
        ("workspace-settings.lens", "Arbeitsbereich-Einstellungen"),
        ("workspace-directory.listing", "Öffentliche Arbeitsbereiche"),
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

/// The layout and its history are the person's layout: both name themselves in the person's
/// language, not only the rows and places of it.
#[test]
fn the_layout_names_itself_in_the_persons_language() {
    let root = private_home();
    let session = seeded_session(root.path());
    speak("de-DE");
    assert_eq!(
        named_by_id(&session, "shell:layout").as_deref(),
        Some("Layout")
    );
    assert_eq!(
        named_by_id(&session, "shell:layout-history").as_deref(),
        Some("Layoutverlauf")
    );
    speak("en-US");
    assert_eq!(
        named_by_id(&session, "shell:layout").as_deref(),
        Some("Layout")
    );
    assert_eq!(
        named_by_id(&session, "shell:layout-history").as_deref(),
        Some("Layout history")
    );
}

/// The words the layout's resource publishes for its actions: each action's label, and what each
/// of its arguments is called.
fn layout_actions(session: &tilo_session::Session) -> Vec<(String, Vec<String>)> {
    fn find(specs: &[ResourceSpec]) -> Option<Vec<(String, Vec<String>)>> {
        specs.iter().find_map(|spec| {
            if spec.id.0 == "shell:layout" {
                Some(
                    spec.actions
                        .iter()
                        .map(|action| {
                            (
                                action.label.clone(),
                                action
                                    .params()
                                    .iter()
                                    .map(|param| param.description.to_string())
                                    .collect(),
                            )
                        })
                        .collect(),
                )
            } else {
                find(&spec.children)
            }
        })
    }
    find(&session.resources()).expect("the layout publishes its resource")
}

/// The layout's actions are copy too: what Undo does, what putting a resource on the canvas does,
/// and what each argument of it is called, read in the person's language.
#[test]
fn the_layouts_actions_speak_the_persons_language() {
    let root = private_home();
    let session = seeded_session(root.path());
    speak("de-DE");
    let german = layout_actions(&session);
    let labels: Vec<&str> = german.iter().map(|(label, _)| label.as_str()).collect();
    assert!(
        labels.contains(&"Die letzte Änderung am Layout rückgängig machen"),
        "the layout undoes a change in German: {labels:?}"
    );
    assert!(
        labels.contains(&"Eine Ressource als eine ihrer Ansichten auf die Fläche stellen"),
        "the layout places a resource in German: {labels:?}"
    );
    let hints: Vec<&str> = german
        .iter()
        .flat_map(|(_, params)| params.iter().map(String::as_str))
        .collect();
    assert!(
        hints.contains(&"Der Arbeitsbereich, auf dessen Fläche er steht, nach seiner Kennung; der vorn, wenn ausgelassen"),
        "the layout names a placement's workspace in German: {hints:?}"
    );
    for english in [
        "Undo the last layout change",
        "Put a resource on the canvas as one of its views",
    ] {
        assert!(
            !labels.contains(&english),
            "a layout action reads `{english}` on a German desktop: {labels:?}"
        );
    }

    speak("en-US");
    let english = layout_actions(&session);
    let labels: Vec<&str> = english.iter().map(|(label, _)| label.as_str()).collect();
    assert!(
        labels.contains(&"Undo the last layout change"),
        "an English desktop reads the layout's undo in English: {labels:?}"
    );
}
