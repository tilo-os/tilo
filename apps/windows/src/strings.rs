//! Fluent strings for the Windows compatibility manager.

ui::embedded_catalog!(pub(crate) static CATALOG, ["de-DE", "en-US"]);

/// Resolve a Windows Apps Fluent key.
pub fn t(key: &str) -> String {
    CATALOG.t(key)
}

/// Resolve `key` with one named variable (`{ $name }` in the message).
pub fn format(key: &str, name: &str, value: &str) -> String {
    CATALOG.t_args(key, &[(name, value.into())])
}

/// Resolve `key` with every named variable it takes.
pub fn format_all(key: &str, args: &[(&str, String)]) -> String {
    let vars: Vec<(&str, ui::i18n::ArgValue)> = args
        .iter()
        .map(|(name, value)| (*name, value.as_str().into()))
        .collect();
    CATALOG.t_args(key, &vars)
}

/// Names read back as a person says them in a sentence: "`PuTTY`", "`PuTTY` and `PSFTP`", or
/// "`PSFTP`, `PuTTY` and `PuTTYGen`".
pub fn names(names: &[String]) -> String {
    match names.split_last() {
        None => String::new(),
        Some((only, [])) => only.clone(),
        Some((last, leading)) => format_all(
            "windows-names-and",
            &[("leading", leading.join(", ")), ("last", last.clone())],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{format_all, names, t};

    /// Names read back as a person says them, whichever count a setup installed.
    #[test]
    fn names_are_said_as_a_person_says_them() {
        let word = |name: &str| name.to_owned();
        assert_eq!(names(&[]), "");
        assert_eq!(names(&[word("PuTTY")]), "PuTTY");
        assert_eq!(names(&[word("PuTTY"), word("PSFTP")]), "PuTTY and PSFTP");
        assert_eq!(
            names(&[
                word("PSFTP"),
                word("PuTTY"),
                word("PuTTYGen"),
                word("Pageant")
            ]),
            "PSFTP, PuTTY, PuTTYGen and Pageant"
        );
    }

    #[test]
    fn controls_resolve_from_the_bundle() {
        for key in [
            "windows-route-wine",
            "windows-route-proton",
            "windows-route-vm",
            "windows-use-recommendation",
            "windows-choose-windows-iso",
            "windows-open-recovery-desktop",
            "windows-start-appliance",
            "windows-escalate-install-to-vm",
            "windows-restart-appliance",
            "windows-suspend-appliance",
            "windows-resume-appliance",
            "windows-shutdown-appliance",
            "windows-connect-for-activation",
            "windows-ended-with-error",
            "windows-ended-with-error-saying",
            "windows-ended-could-not-open",
            "windows-ended-stopped",
            "windows-ended-stopped-saying",
            "windows-delete-setup",
            "windows-setup",
            "windows-setup-programs",
            "windows-confirm-delete-setup-title",
            "windows-confirm-delete-setup-unnamed-title",
            "windows-confirm-delete-setup-body",
            "windows-confirm-delete-setup-action",
            "windows-setup-data-deleted",
        ] {
            assert_ne!(t(key), key, "missing Fluent key: {key}");
        }
    }

    /// The status and error sentences an appliance reports are the catalogue's too: nothing
    /// a person reads is a literal in the code.
    #[test]
    fn the_appliances_own_sentences_resolve_from_the_bundle() {
        for key in [
            "windows-power-running",
            "windows-power-suspended",
            "windows-power-paused",
            "windows-power-shut-down",
            "windows-power-unknown",
            "windows-phase-not-set-up",
            "windows-phase-installing",
            "windows-phase-finishing-setup",
            "windows-phase-ready",
            "windows-phase-needs-recovery",
            "windows-phase-unavailable",
            "windows-status-running",
            "windows-status-suspended",
            "windows-status-paused",
            "windows-status-shut-down",
            "windows-status-ready",
            "windows-status-not-set-up",
            "windows-status-installing",
            "windows-status-finishing-setup",
            "windows-status-needs-recovery",
            "windows-status-unavailable",
            "windows-license-unknown",
            "windows-license-digital",
            "windows-license-product-key",
            "windows-license-managed",
            "windows-license-evaluation",
            "windows-license-unlicensed",
            "windows-license-unknown-detail",
            "windows-license-unlicensed-detail",
            "windows-license-evaluation-detail",
            "windows-purpose-activation",
            "windows-purpose-update",
            "windows-connection-expired",
            "windows-connection-expires-seconds",
            "windows-connection-expires",
            "windows-app-status-ready",
            "windows-app-status-needs-attention",
            "windows-app-status-uninstalled",
            "windows-confidence-verified",
            "windows-confidence-community",
            "windows-confidence-best-match",
            "windows-confidence-untested",
            "windows-route-capsule-security",
            "windows-route-vm-security",
            "windows-error-unmanageable",
            "windows-error-busy",
            "windows-error-recovery-unavailable",
            "windows-error-connection-no-lease",
            "windows-media-accepted",
            "windows-recovery-open",
            "windows-connection-revoked",
        ] {
            assert_ne!(t(key), key, "missing Fluent key: {key}");
        }
        assert_eq!(
            format_all(
                "windows-connection-expires",
                &[("minutes", "9".to_owned()), ("seconds", "40".to_owned()),],
            ),
            "Expires in 9m 40s"
        );
    }
}

/// German is one of the languages the app ships, and it reads in German: the copy comes
/// from `locales/de-DE.ftl`, never from the English source beside it, so a device set to
/// German reads a German window.
#[cfg(test)]
mod german_catalogue_tests {
    use super::CATALOG;

    #[test]
    fn a_german_window_reads_the_catalogue() {
        let catalogue = &CATALOG;
        assert!(
            catalogue.languages().contains(&"de-DE"),
            "the app ships German among {:?}",
            catalogue.languages()
        );
        catalogue.set_locale("de-DE");
        assert_eq!(catalogue.t("windows-cancel"), "Abbrechen");
        catalogue.set_locale("en-US");
        assert_eq!(catalogue.t("windows-cancel"), "Cancel");
    }
}
