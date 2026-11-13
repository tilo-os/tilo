//! Fluent copy for Diff & Merge.
//!
//! Every user-facing sentence with a count or a variable lives in the
//! embedded catalog, so plurals follow the active locale and a `--shots`
//! render, a unit test and a live session resolve the same words.

use ui::i18n::{ArgValue, EmbeddedCatalog};

ui::embedded_catalog!(pub fn, ["de-DE", "en-US"]);

/// The app's catalogs, for the framework's history sentences.
pub fn catalogs() -> &'static EmbeddedCatalog {
    &CATALOG
}

/// A count as a message argument.
pub fn count(n: usize) -> ArgValue {
    ArgValue::Number(n as f64)
}

/// Text as a message argument.
pub fn text(value: &str) -> ArgValue {
    ArgValue::Text(value.to_owned())
}

/// German is one of the languages the app ships, and it reads in German: the
/// copy comes from `locales/de-DE.ftl`, never from the English source beside
/// it, so a device set to German reads a German window.
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
        assert_eq!(catalogue.t("diff-status-empty"), "Keine Dateien geöffnet");
        catalogue.set_locale("en-US");
        assert_eq!(catalogue.t("diff-status-empty"), "No files open");
    }
}
