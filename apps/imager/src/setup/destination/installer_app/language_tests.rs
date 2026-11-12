//! The wizard's words: each step's actions and the breadcrumb resolve through
//! the catalogue, so a German window reads German.

use super::*;

/// Every name a step paints, from its controls and its breadcrumb.
fn accessible_names(widget: &dyn Widget, names: &mut Vec<String>) {
    if let Some(name) = widget.accessibility().and_then(|node| node.name) {
        names.push(name);
    }
    for child in widget.children() {
        accessible_names(child, names);
    }
}

/// Every message the wizard's buttons and its breadcrumb name is one both
/// shipped languages carry, so no window paints a key.
#[test]
fn the_wizard_names_messages_the_catalogues_carry() {
    let catalog = &crate::resource::CATALOG;
    let previous = catalog.current_locale();
    for locale in ["en-US", "de-DE"] {
        catalog.set_locale(locale);
        for key in [
            "installer-preview-back",
            "imager-installer-next",
            "imager-installer-continue",
            "imager-installer-continue-offline",
            "installer-accounts-connect",
            "installer-action-power-off",
            "installer-action-reboot",
            "installer-action-return-to-host",
            "installer-action-refresh-networks",
            "installer-action-refresh-disks",
            "installer-action-review-storage",
            "installer-action-recovery-promise",
            "installer-action-copy-recovery-key",
            "installer-action-copy-recovery-key-again",
            "installer-action-recovery-saved",
            "installer-action-begin",
            "installer-region-use",
            "installer-region-selected",
        ]
        .into_iter()
        .chain(STEP_KEYS)
        {
            assert_ne!(catalog.t(key), key, "{key} is missing from {locale}");
        }
    }
    catalog.set_locale(&previous);
}

/// A German window names the wizard's steps and its actions in German.
#[test]
fn the_wizard_steps_read_in_the_windows_language() {
    let catalog = &crate::resource::CATALOG;
    let previous = catalog.current_locale();
    catalog.set_locale("de-DE");
    let mut painted = Vec::new();
    for (step, forward) in [
        (Step::Keyboard, "Weiter"),
        (Step::Network, "Ohne Netz weiter"),
        (Step::Accounts, "Weiter"),
        (Step::Disk, "Weiter"),
        (Step::Account, "Weiter"),
        (Step::Options, "Weiter"),
        (Step::Media, "Weiter"),
        (Step::Review, "Installation starten"),
    ] {
        let mut app = InstallerApp::with_disks(Disk::samples());
        app.step = step;
        // Media offers its forward action once the formats are chosen.
        app.codecs = step == Step::Media;
        let composition = app.build_composition(&Theme::dark());
        let mut names = Vec::new();
        accessible_names(composition.as_ref(), &mut names);
        painted.push((step, forward, names));
    }
    let captions = step_captions();
    catalog.set_locale(&previous);
    for german in ["Tastatur", "Prüfen", "Installieren"] {
        assert!(
            captions.iter().any(|caption| caption == german),
            "the breadcrumb is not in German: {captions:?}"
        );
    }
    for (step, forward, names) in painted {
        assert!(
            names.iter().any(|name| name == forward),
            "{step:?} does not name its action {forward:?} in German: {names:?}"
        );
        for source in [
            "Next",
            "Continue",
            "Continue offline",
            "Back",
            "Refresh disks",
            "Refresh networks",
            "Connect",
            "Begin Install",
            "Review storage",
            "Power off",
            "Reboot now",
            "I will save my recovery key",
            "Copy recovery key",
        ] {
            assert!(
                !names.iter().any(|name| name == source),
                "{step:?} leaves {source:?} in the German window: {names:?}"
            );
        }
    }
}
