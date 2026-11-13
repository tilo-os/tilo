//! Authenticator's Fluent catalogue, resolved through the toolkit's i18n
//! manager.

ui::embedded_catalog!(pub fn, ["de-DE", "en-US"]);

pub fn ensure() {
    CATALOG.ensure();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_the_app_uses_resolves_to_a_real_string() {
        // A key that resolves to itself has no catalogue entry.
        for key in [
            "totp-add-title-add",
            "totp-add-title-edit",
            "totp-add-title-provision",
            "totp-add-scan-title",
            "totp-add-scan-description",
            "totp-scan-unreadable",
            "totp-add-link-title",
            "totp-add-link-description",
            "totp-add-link-label",
            "totp-add-details",
            "totp-add-label-issuer",
            "totp-add-label-account",
            "totp-add-label-secret",
            "totp-add-advanced",
            "totp-add-label-algorithm",
            "totp-add-label-digits",
            "totp-add-label-period",
            "totp-add-cancel",
            "totp-add-save-add",
            "totp-add-save-edit",
            "totp-add-save-provision",
        ] {
            let s = t(key);
            assert_ne!(s, key, "{key} has no entry in the catalogue");
            assert!(!s.trim().is_empty(), "{key} resolves to nothing");
        }
    }
}

/// German copy comes from `locales/de-DE.ftl`, not the English catalogue.
#[cfg(test)]
mod german_window {
    #[test]
    fn a_german_window_reads_the_catalogue() {
        let catalog = &super::CATALOG;
        assert!(
            catalog.languages().contains(&"de-DE"),
            "the app ships German among {:?}",
            catalog.languages()
        );
        catalog.set_locale("de-DE");
        assert_eq!(catalog.t("totp-add-title-add"), "Konto hinzufügen");
        catalog.set_locale("en-US");
        assert_eq!(catalog.t("totp-add-title-add"), "Add account");
    }
}
