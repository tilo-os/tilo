//! Every sheet's resource is named in the language the person reads.
//!
//! A sheet declares its resource's name once, in English, and the catalogue
//! beside this crate holds the name each language reads it by. The resource
//! layer resolves that entry when the resource is published, so a sheet put
//! up on a German desktop is published as the German name — and keeps its
//! declared one where a catalogue has no entry for it.

use tilo_sheets::dictation::Dictation;
use tilo_sheets::directory::WorkspaceDirectory;
use tilo_sheets::display_confirm::DisplayConfirm;
use tilo_sheets::displays::DisplaySheet;
use tilo_sheets::lock::LockScreen;
use tilo_sheets::shortcut_guide::ShortcutGuide;
use tilo_sheets::workspace_settings::WorkspaceSettings;
use ui::tilo_resource::{Resource, label};

/// The catalogue every sheet's words are in: this crate's.
const SHEETS: Option<&'static label::Catalogue> = LockScreen::CATALOGUE;

/// Each sheet's resource: its kind, the name it declares, what an English
/// desktop reads it by, and what a German one does.
type Sheet = (&'static str, &'static str, &'static str, &'static str);

fn sheets() -> [Sheet; 7] {
    [
        (
            LockScreen::KIND,
            LockScreen::LABEL,
            "Lock screen",
            "Sperrbildschirm",
        ),
        (Dictation::KIND, Dictation::LABEL, "Dictation", "Diktat"),
        // The glossary's one name for the F1 sheet, in both languages.
        (
            ShortcutGuide::KIND,
            ShortcutGuide::LABEL,
            "Keyboard shortcuts",
            "Tastenkürzel",
        ),
        (
            WorkspaceDirectory::KIND,
            WorkspaceDirectory::LABEL,
            "Public workspaces",
            "Öffentliche Arbeitsbereiche",
        ),
        (
            DisplaySheet::KIND,
            DisplaySheet::LABEL,
            "Displays",
            "Anzeige",
        ),
        (
            DisplayConfirm::KIND,
            DisplayConfirm::LABEL,
            "Display change",
            "Anzeigeänderung",
        ),
        (
            WorkspaceSettings::KIND,
            WorkspaceSettings::LABEL,
            "Workspace settings",
            "Arbeitsbereich-Einstellungen",
        ),
    ]
}

#[test]
fn a_sheet_names_its_resource_in_the_persons_language() {
    // The first use of the catalogue installs the thread's manager in the
    // system's language, so the language is chosen with it, not before it.
    ui::i18n::I18nManager::init("de-DE");
    for (kind, declared, english, german) in sheets() {
        assert_eq!(
            label::localized(kind, declared, SHEETS),
            german,
            "{kind} in German"
        );
        assert_ne!(label::localized(kind, declared, SHEETS), english, "{kind}");
    }

    // A fresh manager for the second language, as the catalogue's first use
    // installs one in the system's language.
    ui::i18n::I18nManager::init("en-US");
    for (kind, declared, english, _) in sheets() {
        assert_eq!(
            label::localized(kind, declared, SHEETS),
            english,
            "{kind} in English"
        );
    }
}

#[test]
fn a_resource_a_sheet_names_itself_keeps_that_name() {
    ui::i18n::I18nManager::init("de-DE");
    // A publisher that names the resource itself (the app a question is
    // about, say) is not read through the kind's entry.
    assert_eq!(
        label::published(
            LockScreen::KIND,
            "Frogs, by name",
            LockScreen::LABEL,
            SHEETS
        ),
        "Frogs, by name"
    );
    assert_eq!(
        label::published(
            LockScreen::KIND,
            LockScreen::LABEL,
            LockScreen::LABEL,
            SHEETS
        ),
        "Sperrbildschirm"
    );
}
