//! Fluent copy for Database.
//!
//! Every sentence a person reads comes from the catalogue, so the German name
//! the manifest advertises is a German window: the panes, the dialogs, the
//! status lines and the window bar. A message the catalogue lacks falls back
//! to its English source rather than to a bare key.

use ui::i18n::{ArgValue, EmbeddedCatalog};

ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

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

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE_FTL: &str = include_str!("../locales/en-US.ftl");
    const DE_FTL: &str = include_str!("../locales/de-DE.ftl");

    fn ids(source: &str) -> Vec<String> {
        source
            .lines()
            .filter(|line| !line.starts_with([' ', '#', '*', '[', '}']) && line.contains(" ="))
            .filter_map(|line| line.split(" =").next().map(str::to_string))
            .collect()
    }

    /// Every message the source catalogue has, the German one has too, so the
    /// German window the manifest advertises is really German.
    #[test]
    fn the_german_catalogue_covers_every_message() {
        let english = ids(SOURCE_FTL);
        let german = ids(DE_FTL);
        assert!(english.len() > 20, "{english:?}");
        for id in &english {
            assert!(german.contains(id), "de-DE lacks {id}");
        }
        for id in &german {
            assert!(english.contains(id), "en-US lacks {id}");
        }
    }

    /// Every key the app asks for exists, so a fallback to a bare key is a
    /// test failure rather than something a person has to read.
    #[test]
    fn the_catalogue_holds_the_messages_the_app_asks_for() {
        let source = ids(SOURCE_FTL);
        assert!(source.iter().any(|id| id == "db-run-query"));
        assert_eq!(t("db-run-query"), "Run query");
    }
}
