//! Fluent copy for Database.

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

    /// Every German message parses and resolves to German text: the file
    /// being complete is not the same as it being readable, and a broken
    /// message would show its own key in the window the manifest advertises.
    #[test]
    fn the_german_window_says_every_message_in_german() {
        std::thread::spawn(|| {
            ui::i18n::I18nManager::install(
                "en-US",
                "de-DE",
                &[("en-US", SOURCE_FTL), ("de-DE", DE_FTL)],
            )
            .expect("the German catalogue parses");
            for id in ids(DE_FTL) {
                let text = t(&id);
                assert!(!text.trim().is_empty(), "{id} says nothing in German");
                assert_ne!(text, id, "{id} falls back to its key");
            }
        })
        .join()
        .expect("the German window reads");
    }

    #[test]
    fn the_catalogue_holds_the_messages_the_app_asks_for() {
        let source = ids(SOURCE_FTL);
        assert!(source.iter().any(|id| id == "db-run-query"));
        assert_eq!(t("db-run-query"), "Run query");
    }
}
