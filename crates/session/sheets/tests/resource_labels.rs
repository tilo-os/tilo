//! Every sheet's resource is named in the language the person reads.
//!
//! A sheet declares its resource's name once, in English, and the catalogue
//! beside this crate holds the name each language reads it by. The resource
//! layer resolves that entry when the resource is published, so a sheet put
//! up on a German desktop is published as the German name — and keeps its
//! declared one where a catalogue has no entry for it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_sheets::add_app::AddToApps;
use tilo_sheets::dictation::Dictation;
use tilo_sheets::directory::WorkspaceDirectory;
use tilo_sheets::display_confirm::DisplayConfirm;
use tilo_sheets::displays::DisplaySheet;
use tilo_sheets::file_picker::FileChooser;
use tilo_sheets::lock::LockScreen;
use tilo_sheets::portal::PortalSheet;
use tilo_sheets::power::Power;
use tilo_sheets::shortcut_guide::ShortcutGuide;
use tilo_sheets::uninstall::Uninstall;
use tilo_sheets::workspace_settings::WorkspaceSettings;
use ui::tilo_resource::{Resource, label};

/// The catalogue every sheet's words are in: this crate's.
const SHEETS: Option<&'static label::Catalogue> = Power::CATALOGUE;

/// Each sheet's resource: its kind, the name it declares, what an English
/// desktop reads it by, and what a German one does.
type Sheet = (&'static str, &'static str, &'static str, &'static str);

fn sheets() -> [Sheet; 12] {
    [
        (Power::KIND, Power::LABEL, "Power", "Ein/Aus"),
        (
            LockScreen::KIND,
            LockScreen::LABEL,
            "Lock screen",
            "Sperrbildschirm",
        ),
        (Dictation::KIND, Dictation::LABEL, "Dictation", "Diktat"),
        (
            Uninstall::KIND,
            Uninstall::LABEL,
            "Uninstall",
            "Deinstallieren",
        ),
        (
            AddToApps::KIND,
            AddToApps::LABEL,
            "Add to apps",
            "Zu den Apps hinzufügen",
        ),
        (
            FileChooser::KIND,
            FileChooser::LABEL,
            "File chooser",
            "Dateiauswahl",
        ),
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
            PortalSheet::KIND,
            PortalSheet::LABEL,
            "System question",
            "Systemfrage",
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
    // Loading a word of the sheets' own loads their catalogue; the language
    // is the desktop's from here on.
    let _ = tilo_sheets::word("power-session");
    ui::i18n::I18nManager::set_locale("de-DE");
    for (kind, declared, english, german) in sheets() {
        assert_eq!(
            label::localized(kind, declared, SHEETS),
            german,
            "{kind} in German"
        );
        assert_ne!(label::localized(kind, declared, SHEETS), english, "{kind}");
    }
    // The same resolution a resource's own name goes through.
    assert_eq!(Power::label(), "Ein/Aus");

    ui::i18n::I18nManager::set_locale("en-US");
    for (kind, declared, english, _) in sheets() {
        assert_eq!(
            label::localized(kind, declared, SHEETS),
            english,
            "{kind} in English"
        );
    }
    assert_eq!(Power::label(), "Power");
}

#[test]
fn a_resource_a_sheet_names_itself_keeps_that_name() {
    let _ = tilo_sheets::word("power-session");
    ui::i18n::I18nManager::set_locale("de-DE");
    // A publisher that names the resource itself (the app a question is
    // about, say) is not read through the kind's entry.
    assert_eq!(
        label::published(Power::KIND, "Frogs, by name", Power::LABEL, SHEETS),
        "Frogs, by name"
    );
    assert_eq!(
        label::published(Power::KIND, Power::LABEL, Power::LABEL, SHEETS),
        "Ein/Aus"
    );
}
