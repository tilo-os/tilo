//! Fluent copy for Diff & Merge: every sentence with a count or a variable lives in the
//! embedded catalogue, so plurals follow the active locale.

use ui::i18n::{ArgValue, EmbeddedCatalog};

ui::embedded_catalog!(pub fn, ["de-DE", "en-US"]);

pub fn catalogs() -> &'static EmbeddedCatalog {
    &CATALOG
}

pub fn count(n: usize) -> ArgValue {
    ArgValue::Number(n as f64)
}

pub fn text(value: &str) -> ArgValue {
    ArgValue::Text(value.to_owned())
}

/// German copy comes from `locales/de-DE.ftl`, not the English catalogue.
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
