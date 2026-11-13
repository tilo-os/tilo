//! What a palette's empty list says: that more rows are coming, that nothing has been typed yet, or
//! that nothing matches what was — the last naming the query and saying what to try instead.

use super::ActionPalette;
use crate::i18n::{widget_format, widget_text};

impl ActionPalette {
    /// What the empty result list says now, as a person reads it.
    #[must_use]
    pub fn empty_text(&self) -> &str {
        self.results.empty_text()
    }

    /// What the empty result list says to try instead, under that line; `None` while it says nothing
    /// more.
    #[must_use]
    pub fn empty_hint(&self) -> Option<&str> {
        self.results.empty_hint()
    }

    /// Say in the empty list whether rows are still coming, that nothing has been typed yet, or that
    /// nothing matches what was, which the line names.
    pub(crate) fn sync_plain_empty_state(&mut self, query: &str, searching: bool) {
        let named = !query.trim().is_empty();
        let title = if searching {
            widget_text("palette-searching")
        } else if named {
            widget_format("palette-no-results", &[("query", query.into())])
        } else {
            widget_text("palette-empty")
        };
        self.results.set_empty_text(title);
        self.results
            .set_empty_hint((!searching && named).then(|| widget_text("palette-no-results-hint")));
    }
}
