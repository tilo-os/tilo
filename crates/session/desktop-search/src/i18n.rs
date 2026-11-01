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
}
