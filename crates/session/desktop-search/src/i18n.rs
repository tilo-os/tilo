//! Desktop search's Fluent catalogue: what its own rows say (an answer, a
//! web search), in the desktop's language, the same on every desktop. The
//! source catalogue is embedded; other languages come from the language
//! packs, and a message missing from one falls back to `en-US`.

ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

/// Load the palette's catalogue on this thread, so the rows it builds read in
/// the desktop's language without reading a language pack there.
pub fn ensure() {
    CATALOG.ensure();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_read_in_words() {
        assert_eq!(
            t_args("search-web-for", &[("query", "cats".into())]),
            "Search the web for cats"
        );
        assert_eq!(t("search-kind-answers"), "Answers");
    }

    /// The row's hint is its own words, so it follows the desktop's
    /// language as the row's label does.
    #[test]
    fn the_alternate_hints_read_in_the_desktops_language() {
        let english = (t("search-open-as-tab"), t("search-open-beside"));
        ui::i18n::I18nManager::set_locale("de-DE");
        let german = (t("search-open-as-tab"), t("search-open-beside"));
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(
            english,
            (
                "Opens as a tab here".to_owned(),
                "Opens a window beside".to_owned()
            )
        );
        assert_eq!(
            german,
            (
                "Öffnet als Tab hier".to_owned(),
                "Öffnet ein Fenster daneben".to_owned()
            )
        );
    }
}
