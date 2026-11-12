//! Mail's Fluent catalogue.
//!
//! User-facing strings are keys, never literals, resolved through the toolkit's
//! [`mod@ui::i18n`] manager. The source catalogue is embedded with `include_str!`
//! and the other languages come from the language packs, so there is no build
//! step and no runtime file I/O — a `--shots` render and a unit test resolve the
//! same strings a live session does.

use ui::i18n::{ArgValue, EmbeddedCatalog};

// The catalogue every host mounting Mail loads before its view. It is loaded
// per thread on first use; a host's manager is joined, never replaced.
ui::embedded_catalog!(pub static CATALOG, ["de-DE", "en-US"]);

/// The catalogue, which every host mounting Mail loads before its view.
pub fn catalog() -> &'static EmbeddedCatalog {
    &CATALOG
}

/// Resolve a Mail string key.
pub fn t(key: &str) -> String {
    CATALOG.t(key)
}

/// The subject a message with none is listed under.
pub fn no_subject() -> String {
    CATALOG.t("mail-no-subject")
}

/// Resolve a Mail string key that interpolates variables.
///
/// The values are the app's own words — an operation's label, a participant's
/// name — so the *sentence* around them stays in the catalogue and only the
/// nouns are substituted. Building the line with `format!` instead would put
/// English word order in the source and leave nothing to translate.
pub fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    CATALOG.t_args(key, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_resolves_every_used_key() {
        {
            // A key that resolves to itself is a key with no catalogue entry — the
            // failure mode that ships an untranslatable literal to a screen.
            for key in [
                "mail-compose-label-to",
                "mail-compose-label-cc",
                "mail-compose-label-bcc",
                "mail-compose-label-subject",
                "mail-compose-placeholder-to",
                "mail-compose-cc-bcc",
                "mail-compose-send",
                "mail-compose-save-draft",
                "mail-compose-discard",
                "mail-compose-prompt-link",
                "mail-compose-prompt-add",
                "mail-compose-prompt-cancel",
                "mail-contact-write",
                "mail-a11y-bulk-select",
                "mail-a11y-bulk-done",
                "mail-a11y-preview-unread",
                "mail-a11y-thread-messages",
            ] {
                let s = t(key);
                assert_ne!(s, key, "{key} has no entry in the catalogue");
                assert!(!s.trim().is_empty(), "{key} resolves to nothing");
            }
        }
    }
}

/// German is one of the languages the app ships, and it reads in German: the
/// copy comes from `locales/de-DE.ftl`, never from the English source
/// catalogue beside it.
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
        assert_eq!(catalog.t("mail-compose-label-subject"), "Betreff");
        assert_eq!(
            catalog.t("mail-a11y-preview-unread"),
            "Vorschau, weiterhin ungelesen"
        );
        catalog.set_locale("en-US");
        assert_eq!(catalog.t("mail-compose-label-subject"), "Subject");
    }
}
