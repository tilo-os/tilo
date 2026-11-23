//! What the palette's list shows, as a person reads it: its rows' labels, the line under each, and the
//! rows as the list paints them. A window that owns a palette, and its tests, read these; nothing here
//! changes the list.

use super::ActionPalette;
use crate::search::SearchResult;

impl ActionPalette {
    /// The labels currently shown, in display order (for tests / diagnostics).
    pub fn result_labels(&self) -> Vec<String> {
        self.results
            .groups()
            .iter()
            .flat_map(|g| g.results.iter().map(|r| r.title.clone()))
            .collect()
    }

    /// The rows currently shown as a person reads them, label and the line under it, in display order
    /// (for tests / diagnostics).
    pub fn result_rows(&self) -> Vec<(String, Option<String>)> {
        self.results
            .groups()
            .iter()
            .flat_map(|g| {
                g.results
                    .iter()
                    .map(|r| (r.title.clone(), r.subtitle.clone()))
            })
            .collect()
    }

    /// The rows currently shown as the list paints them, in display order: their titles, underlines,
    /// pictures and shortcuts (for tests and diagnostics).
    pub fn result_hits(&self) -> Vec<SearchResult> {
        self.results
            .groups()
            .iter()
            .flat_map(|group| group.results.iter().cloned())
            .collect()
    }
}
