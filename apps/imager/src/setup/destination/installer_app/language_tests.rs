//! The wizard's words: each step's actions and the breadcrumb resolve through the
//! catalogue, so a German window reads German.

use super::surface::step_name;
use super::*;

fn accessible_names(widget: &dyn Widget, names: &mut Vec<String>) {
    if let Some(name) = widget.accessibility().and_then(|node| node.name) {
        names.push(name);
    }
    for child in widget.children() {
        accessible_names(child, names);
    }
}

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
        .chain(INSTALL_METHOD_KEYS)
        .chain(FIDO2_KEYS)
        .chain([
            "installer-state-idle",
            "installer-state-running",
            "installer-state-done",
            "installer-state-failed",
            "installer-action-confirm-calendars",
            "installer-item-region",
            "installer-item-storage",
            "installer-item-storage-erase",
            "installer-item-review",
            "installer-item-review-no-target",
            "installer-review-erase",
            "installer-review-alongside",
            "installer-detail-step",
            "installer-status-type-confirmation",
            "installer-alongside-change-guided",
            "installer-alongside-change-prepared",
            "installer-alongside-change-note",
            "installer-blocked-other-step",
            "installer-blocked-password-hash",
            "installer-blocked-next-review",
            "installer-blocked-next-install",
            "installer-blocked-back-install",
            "installer-blocked-back-first",
            "installer-blocked-not-staged",
            "installer-blocked-return-running",
            "installer-blocked-return-done",
            "installer-blocked-network-busy",
            "installer-blocked-no-networks",
            "installer-blocked-no-network-choice",
            "installer-blocked-network-connected",
            "installer-blocked-signing-in",
            "installer-blocked-retry-not-failed",
            "installer-blocked-no-recovery-key",
            "installer-blocked-power-not-finished",
            "installer-blocked-recovery-unsaved-reboot",
            "installer-blocked-recovery-unsaved-power-off",
            "installer-blocked-installer-disk",
            "installer-blocked-read-only",
            "installer-blocked-begin-no-target",
            "installer-blocked-begin-recovery",
        ]) {
            assert_ne!(catalog.t(key), key, "{key} is missing from {locale}");
        }
    }
    catalog.set_locale(&previous);
}

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

#[test]
fn the_wizards_choices_reasons_and_badges_read_in_the_windows_language() {
    let catalog = &crate::resource::CATALOG;
    let previous = catalog.current_locale();
    catalog.set_locale("de-DE");
    let methods = captions(&INSTALL_METHOD_KEYS);
    let keys = captions(&FIDO2_KEYS);
    let elsewhere = InstallerApp::with_disks(Disk::samples())
        .blocked(&DestinationEffect::RefreshDisks)
        .map(|(_, reason)| reason);
    let done = InstallerApp {
        install_state: InstallState::Failed,
        ..InstallerApp::with_disks(Disk::samples())
    };
    let badge = crate::setup::strings::t(done.install_state_key());
    let caption = step_name(Step::Review);
    let mut names = Vec::new();
    let mut disk = InstallerApp::with_disks(Disk::samples());
    disk.step = Step::Disk;
    accessible_names(disk.build_composition(&Theme::dark()).as_ref(), &mut names);
    catalog.set_locale(&previous);

    assert_eq!(
        methods,
        ["Datenträger löschen", "Daneben installieren"],
        "the disk method is left in the source language"
    );
    assert_eq!(
        keys,
        [
            "Kein Sicherheitsschlüssel",
            "Sicherheitsschlüssel hinterlegen"
        ],
        "the security-key choice is left in the source language"
    );
    let reason = elsewhere.expect("another step owns the disk list");
    assert!(
        reason.starts_with("Das gehört zum Schritt"),
        "the blocked reason is left in the source language: {reason}"
    );
    assert_eq!(badge, "Fehlgeschlagen", "the badge is left in English");
    assert_eq!(caption, "Prüfen", "the step name is left in English");
    assert!(
        names
            .iter()
            .all(|name| name != "Erase disk" && name != "Next"),
        "the German disk step paints the source language: {names:?}"
    );
}
