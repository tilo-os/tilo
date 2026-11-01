//! What a palette's empty list says: that more rows are coming, that nothing
//! has been typed yet, or that nothing matches what was — the last naming the
//! query and saying what to try instead.

use super::ActionPalette;

impl ActionPalette {
    /// What the empty result list says now, as a person reads it.
    #[must_use]
    pub fn empty_text(&self) -> &str {
        self.results.empty_text()
    }

    /// What the empty result list says to try instead, under that line;
    /// `None` while it says nothing more.
    #[must_use]
    pub fn empty_hint(&self) -> Option<&str> {
        self.results.empty_hint()
    }

    /// Say in the empty list whether rows are still coming, that nothing has
    /// been typed yet, or that nothing matches what was, which the line
    /// names. A query that matched nothing also says what to try instead; a
    /// card with nothing typed yet does not talk about spelling.
    pub(crate) fn sync_plain_empty_state(&mut self, query: &str, searching: bool) {
        let named = !query.trim().is_empty();
        let title = if searching {
            crate::i18n!("palette-searching")
        } else if named {
            crate::i18n!("palette-no-results", "query" => query)
        } else {
            crate::i18n!("palette-empty")
        };
        self.results.set_empty_text(title);
        self.results
            .set_empty_hint((!searching && named).then(|| crate::i18n!("palette-no-results-hint")));
    }
}
