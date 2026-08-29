//! Sheets as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: the workbook declares its
//! searchable type by implementing [`SearchProvider`] over a snapshot of the
//! sheet names, so a sheet is findable from the unified Ctrl+K palette by name.
//! Activating a row hands back an opaque `action_id` the app resolves to a sheet
//! index and switches to.
//!
//! Scope note: the provider indexes **sheet names**, which are the workbook
//! structure the app holds in memory (`SheetsApp::sheet_names`). Cross-sheet
//! *cell* search would require reading every sheet out of the CRDT on the query
//! path, which the synchronous provider contract forbids; the in-sheet "go to
//! cell" jump lives in the app's own command palette instead. There are no
//! named ranges in the model to index.

use ui::icons::Icon;
use ui::search::{SearchProvider, SearchResult};

/// The kind label (and result-group header) Sheets produces.
pub const SHEET_KIND: &str = "Sheets";

/// The `action_id` prefix. A chosen row's `action_id` is `sheet:<index>`;
/// [`parse_action_id`] turns it back into the sheet index.
const ACTION_PREFIX: &str = "sheet:";

/// A [`SearchProvider`] over a snapshot of the workbook's sheet names.
pub struct SheetSearchProvider {
    names: Vec<String>,
}

impl SheetSearchProvider {
    /// Snapshot the current sheet names into a provider.
    pub fn new(names: &[String]) -> Self {
        Self {
            names: names.to_vec(),
        }
    }

    /// The `action_id` for a sheet index — what a chosen row hands back.
    pub fn action_id(index: usize) -> String {
        format!("{ACTION_PREFIX}{index}")
    }

    /// Resolve an `action_id` produced by this provider back into a sheet
    /// index.
    pub fn parse_action_id(action_id: &str) -> Option<usize> {
        action_id
            .strip_prefix(ACTION_PREFIX)
            .and_then(|rest| rest.parse::<usize>().ok())
    }
}

impl SearchProvider for SheetSearchProvider {
    fn name(&self) -> &str {
        SHEET_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        let q = query.to_lowercase();
        self.names
            .iter()
            .enumerate()
            .filter(|(_, name)| name.to_lowercase().contains(&q))
            .map(|(i, name)| {
                SearchResult::new(SHEET_KIND, name.clone(), Self::action_id(i))
                    .with_icon(Icon::LayoutGrid)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchRegistry;

    fn names() -> Vec<String> {
        vec!["Budget".into(), "Sales".into(), "Q1 Forecast".into()]
    }

    #[test]
    fn provider_finds_a_sheet_by_name() {
        let provider = SheetSearchProvider::new(&names());
        let hits = provider.search("forecast");
        assert!(
            hits.iter().any(|r| r.title == "Q1 Forecast"),
            "the provider surfaces the sheet"
        );
        assert!(hits.iter().all(|r| r.kind == SHEET_KIND));
    }

    #[test]
    fn empty_query_browses_every_sheet() {
        let provider = SheetSearchProvider::new(&names());
        assert_eq!(provider.search("").len(), 3);
    }

    #[test]
    fn action_id_round_trips_to_the_sheet_index() {
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(SheetSearchProvider::new(&names())));
        let hit = registry
            .query("sales")
            .into_iter()
            .find(|r| r.title == "Sales")
            .expect("Sales is findable");
        assert_eq!(
            SheetSearchProvider::parse_action_id(&hit.action_id),
            Some(1)
        );
    }

    #[test]
    fn parse_action_id_rejects_foreign_ids() {
        assert!(SheetSearchProvider::parse_action_id("file:/etc/passwd").is_none());
        assert!(SheetSearchProvider::parse_action_id("sheet:not-a-number").is_none());
    }

    #[test]
    fn widget_hooks_find_then_switch_sheet() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        // A seeded workbook has a "Budget" and a "Sales" sheet.
        let mut app = crate::app::SheetsApp::new();
        app.shots_mode(); // seed sample data, no disk persistence
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);

        let providers = app.search_providers();
        assert!(!providers.is_empty(), "sheets publishes a search provider");
        // Find the second sheet (index 1) by browsing, and switch to it.
        let rows = providers[0].search("");
        assert!(rows.len() >= 2, "the seeded workbook has multiple sheets");
        let target = SheetSearchProvider::parse_action_id(&rows[1].action_id).unwrap();
        assert_ne!(
            app.active_sheet_index(),
            target,
            "not already on the target sheet"
        );

        app.activate_search_result(&rows[1].action_id, &mut doc, &ROOT);
        assert_eq!(
            app.active_sheet_index(),
            target,
            "activation switched to the chosen sheet"
        );
    }
}
