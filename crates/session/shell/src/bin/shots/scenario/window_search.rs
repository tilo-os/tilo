//! The window bar's search in place, on Notes in the shell and as a window
//! of its own: the field over the long note, its options, and a list
//! narrowing over the shell-ui connection.

use winit::keyboard::{Key, NamedKey};

use super::{Action, OwnApp, Scenario};

pub(super) fn notes_search_in_place() -> Scenario {
    Scenario {
        name: "notes_search_in_place",
        description: "Search in the window bar: the control after the title expands into a field, the long note highlights every match with n of m, and Enter steps on.",
        actions: vec![
            Action::SeedLongNotes(0),
            Action::Wait(2),
            Action::Relayout,
            Action::Snapshot("00-control-at-rest"),
            Action::ClickNamed("window-search-open"),
            Action::Wait(24),
            Action::Type("capture 1"),
            Action::Wait(8),
            Action::Snapshot("01-search-in-place"),
            Action::Key(Key::Named(NamedKey::Enter)),
            Action::Key(Key::Named(NamedKey::Enter)),
            Action::Wait(8),
            Action::Snapshot("02-third-match"),
        ],
    }
}

pub(super) fn notes_search_options() -> Scenario {
    Scenario {
        name: "notes_search_options",
        description: "The window bar's search on the long note with its options open: the control after the steps opens a panel under the field with Match case and Whole word, and turning Match case on finds again under it.",
        actions: vec![
            Action::SeedLongNotes(0),
            Action::Wait(2),
            Action::Relayout,
            Action::ClickNamed("window-search-open"),
            Action::Wait(24),
            Action::Type("Capture"),
            Action::Wait(8),
            Action::ClickNamed("window-search-options"),
            Action::Wait(24),
            Action::Relayout,
            Action::Snapshot("01-options-open"),
            Action::ClickNamed("window-search-match-case"),
            Action::Wait(8),
            Action::Relayout,
            Action::Snapshot("02-match-case-on"),
            Action::Key(Key::Named(NamedKey::Escape)),
            Action::Wait(24),
            Action::Relayout,
            Action::Snapshot("03-options-closed"),
        ],
    }
}

pub(super) fn notes_replace_in_place() -> Scenario {
    Scenario {
        name: "notes_replace_in_place",
        description: "Replace in the window bar's search on the long note: the options panel holds the replacement, Replace changes the current match in the note and the count drops by one, Replace all changes the rest.",
        actions: vec![
            Action::SeedLongNotes(0),
            Action::Wait(2),
            Action::Relayout,
            Action::ClickNamed("window-search-open"),
            Action::Wait(24),
            Action::Type("capture 1"),
            Action::Wait(8),
            Action::ClickNamed("window-search-options"),
            Action::Wait(24),
            Action::ClickNamed("window-search-replacement"),
            Action::Type("draft 1"),
            Action::Wait(8),
            Action::Relayout,
            Action::Snapshot("01-replacement-typed"),
            Action::ClickNamed("window-search-replace"),
            Action::Wait(8),
            Action::Relayout,
            Action::Snapshot("02-one-replaced"),
            Action::ClickNamed("window-search-replace-all"),
            Action::Wait(8),
            Action::Relayout,
            Action::Snapshot("03-all-replaced"),
        ],
    }
}

pub(super) fn own_process_notes_search() -> Scenario {
    Scenario {
        name: "own_process_notes_search",
        description: "Notes as a window of its own, as on a device: the window bar's search opens on it over its shell-ui connection, the long note highlights every match with n of m, and Enter steps on. Notes draws no search control of its own.",
        actions: vec![
            Action::OwnProcessWindow {
                app: OwnApp::Notes,
                width: 700,
            },
            Action::Wait(4),
            Action::Relayout,
            Action::Snapshot("00-control-at-rest"),
            Action::ClickNamed("window-search-open"),
            Action::Wait(24),
            Action::Type("capture 1"),
            Action::Wait(8),
            Action::Relayout,
            Action::Snapshot("01-search-in-place"),
            Action::Key(Key::Named(NamedKey::Enter)),
            Action::Key(Key::Named(NamedKey::Enter)),
            Action::Wait(8),
            Action::Relayout,
            Action::Snapshot("02-third-match"),
        ],
    }
}

pub(super) fn own_process_files_search() -> Scenario {
    Scenario {
        name: "own_process_files_search",
        description: "Files as a window of its own: the window bar's search narrows its list over its shell-ui connection, with the count it answers.",
        actions: vec![
            Action::OwnProcessWindow {
                app: OwnApp::Files,
                width: 700,
            },
            Action::Wait(4),
            Action::Relayout,
            Action::Snapshot("00-control-at-rest"),
            Action::ClickNamed("window-search-open"),
            Action::Wait(24),
            Action::Type("re"),
            Action::Wait(8),
            Action::Relayout,
            Action::Snapshot("01-search-in-place"),
        ],
    }
}
