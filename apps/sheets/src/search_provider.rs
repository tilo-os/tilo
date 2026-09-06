//! Sheets as a qOS global-search source.
//!
//! Queries run over a document-head-keyed in-memory projection, never over the
//! CRDT. Sheet and populated-cell hits address stable exported `open` View
//! actions, so search, agents and accessibility all use the brokered route and
//! land on the exact tab and selection.

use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult, SearchTextIndex,
    normalize_search_text,
};

pub const SHEET_KIND: &str = "Sheets";
pub const CELL_KIND: &str = "Sheet cells";

/// One display row and all independently searchable text belonging to it.
/// `sources` are kept separate so a query cannot accidentally match across a
/// field boundary (for example, the end of a cell value plus its A1 address).
struct SearchEntry {
    result: SearchResult,
    sources: Vec<String>,
}

/// A complete shared Unicode substring index.  Field values are joined with a
/// newline sentinel so ordinary text cannot accidentally match across fields.
pub struct SheetSearchProvider {
    sheets: Vec<crate::actions::SearchableSheet>,
    entries: Vec<SearchEntry>,
    index: SearchTextIndex,
}

impl SheetSearchProvider {
    pub fn new(sheets: &[crate::actions::SearchableSheet]) -> Self {
        let mut entries = Vec::new();
        for sheet in sheets {
            entries.push(SearchEntry {
                result: SearchResult::for_action(
                    SHEET_KIND,
                    sheet.name.clone(),
                    crate::actions::open_sheet_address(&sheet.id),
                )
                .with_icon(Icon::LayoutGrid),
                sources: vec![sheet.name.clone()],
            });
        }
        for sheet in sheets {
            for cell in &sheet.cells {
                let reference =
                    format!("{}{}", crate::actions::column_label(cell.col), cell.row + 1);
                let qualified = format!("{}!{reference}", sheet.name);
                let subtitle = if cell.display.is_empty() || cell.display == cell.raw {
                    cell.raw.clone()
                } else {
                    format!("{} · {}", cell.display, cell.raw)
                };
                entries.push(SearchEntry {
                    result: SearchResult::for_action(
                        CELL_KIND,
                        qualified.clone(),
                        crate::actions::open_cell_address(&sheet.id, cell.col, cell.row),
                    )
                    .with_subtitle(subtitle)
                    .with_icon(Icon::LayoutGrid),
                    sources: [reference, qualified, cell.raw.clone(), cell.display.clone()]
                        .into_iter()
                        .filter(|source| !source.is_empty())
                        .collect(),
                });
            }
        }

        Self {
            sheets: sheets.to_vec(),
            index: SearchTextIndex::new(entries.iter().map(|row| row.sources.join("\n"))),
            entries,
        }
    }

    /// Return the requested bounded result window. The shared suffix index
    /// resolves matching offsets directly, without a per-row candidate scan.
    fn matching_entry_ids(&self, query: &str, limit: usize) -> (Vec<usize>, usize) {
        if query.is_empty() || limit == 0 {
            return (Vec::new(), 0);
        }
        let mut matches = self.index.matching_ids(query, limit);
        // Retain Sheets' useful field ranking: an exact field hit comes ahead
        // of a generic substring hit without making exactness a requirement.
        matches.sort_by_key(|&entry| {
            !self.entries.get(entry).is_some_and(|row| {
                row.sources
                    .iter()
                    .any(|source| normalize_search_text(source) == query)
            })
        });
        matches.truncate(limit);
        (matches, 0)
    }
}

impl SearchProvider for SheetSearchProvider {
    fn name(&self) -> &str {
        SHEET_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        if limit == 0 {
            return Vec::new();
        }
        if query.is_empty() {
            return self
                .entries
                .iter()
                .take(limit)
                .map(|entry| entry.result.clone())
                .collect();
        }
        let (ids, _) = self.matching_entry_ids(&normalize_search_text(query), limit);
        ids.into_iter()
            .filter_map(|id| self.entries.get(id))
            .map(|entry| entry.result.clone())
            .collect()
    }

    fn resolve_action_resource(
        &self,
        address: &qos_actions::ActionAddress,
    ) -> Option<qos_actions::Resource> {
        self.sheets.iter().find_map(|sheet| {
            sheet
                .cells
                .iter()
                .find(|cell| {
                    crate::actions::open_cell_address(&sheet.id, cell.col, cell.row) == *address
                })
                .map(|cell| crate::actions::searchable_cell_resource(&sheet.id, cell))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{SearchableCell, SearchableSheet};
    use ui::search::SearchRegistry;

    fn sheets() -> Vec<SearchableSheet> {
        vec![
            SearchableSheet {
                id: "budget-id".into(),
                name: "Budget".into(),
                cells: vec![SearchableCell {
                    col: 0,
                    row: 0,
                    raw: "=SUM(B1:B4)".into(),
                    display: "$4,200.00".into(),
                }],
            },
            SearchableSheet {
                id: "sales-id".into(),
                name: "Sales".into(),
                cells: vec![],
            },
        ]
    }

    #[test]
    fn sheet_and_cell_hits_use_canonical_addresses() {
        let provider = SheetSearchProvider::new(&sheets());
        let sheet = provider.search("sales").pop().expect("sheet hit");
        assert_eq!(
            sheet.action_address(),
            Some(crate::actions::open_sheet_address("sales-id"))
        );

        let cell = provider.search("4,200").pop().expect("computed result hit");
        assert_eq!(cell.title, "Budget!A1");
        assert_eq!(
            cell.action_address(),
            Some(crate::actions::open_cell_address("budget-id", 0, 0))
        );
    }

    #[test]
    fn empty_query_browses_sheets_and_populated_cells() {
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(SheetSearchProvider::new(&sheets())));
        let hits = registry.query("");
        assert_eq!(hits.len(), 3);
        assert!(hits.iter().all(|hit| hit.action_address().is_some()));
    }

    #[test]
    fn limited_search_honours_the_materialisation_budget() {
        let provider = SheetSearchProvider::new(&sheets());
        assert_eq!(provider.search_limited("", 2).len(), 2);
        assert!(provider.search_limited("", 0).is_empty());
    }

    #[test]
    fn late_cells_are_searchable_in_a_large_workbook() {
        let mut large = vec![SearchableSheet {
            id: "large-id".into(),
            name: "Large workbook".into(),
            cells: (0..10_000)
                .map(|row| SearchableCell {
                    col: 0,
                    row,
                    raw: format!("ordinary {row}"),
                    display: String::new(),
                })
                .collect(),
        }];
        large[0].cells[9_999].raw = "needle beyond the old index limit".into();

        let hits = SheetSearchProvider::new(&large).search("needle beyond");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Large workbook!A10000");
        assert_eq!(
            hits[0].action_address(),
            Some(crate::actions::open_cell_address("large-id", 0, 9_999))
        );
    }

    #[test]
    fn complete_index_honors_large_broad_result_windows_without_row_scans() {
        let cells = (0..12_000)
            .map(|row| SearchableCell {
                col: 0,
                row,
                raw: format!("common broad value {row}"),
                display: String::new(),
            })
            .collect();
        let provider = SheetSearchProvider::new(&[SearchableSheet {
            id: "scale-id".into(),
            name: "Scale".into(),
            cells,
        }]);

        let (broad, broad_checks) = provider.matching_entry_ids("common", 256);
        assert_eq!(broad.len(), 256);
        assert_eq!(broad_checks, 0, "suffix lookup does not inspect row text");

        let (missing, missing_checks) = provider.matching_entry_ids("not-present-anywhere", 7);
        assert!(missing.is_empty());
        assert_eq!(missing_checks, 0);
    }

    #[test]
    fn exact_token_ranking_never_excludes_a_longer_substring_hit() {
        let provider = SheetSearchProvider::new(&[SearchableSheet {
            id: "substring-id".into(),
            name: "Substring".into(),
            cells: vec![
                SearchableCell {
                    col: 0,
                    row: 0,
                    raw: "need".into(),
                    display: String::new(),
                },
                SearchableCell {
                    col: 0,
                    row: 1,
                    raw: "needle late substring".into(),
                    display: String::new(),
                },
            ],
        }]);
        let hits = provider.search_limited("need", 2);
        let titles = hits
            .iter()
            .map(|hit| hit.title.as_str())
            .collect::<Vec<_>>();
        assert!(titles.contains(&"Substring!A1"));
        assert!(
            titles.contains(&"Substring!A2"),
            "an exact-token fast path must not hide a longer substring match"
        );
    }

    #[test]
    fn broker_resolves_every_hit_and_opens_the_exact_sheet_or_cell() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::app::SheetsApp::new();
        app.shots_mode();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);

        let resources = Widget::resources(&app, &doc);
        let rent_resource_id = Widget::search_providers(&app)[0]
            .search("Rent")
            .into_iter()
            .find(|hit| hit.title.ends_with("!A2"))
            .and_then(|hit| hit.action_address())
            .map(|address| address.resource)
            .expect("Rent cell has a canonical resource id");
        let hits = Widget::search_providers(&app)[0].search("");
        let providers = Widget::search_providers(&app);
        assert!(
            hits.iter()
                .all(|hit| hit.action_address().is_some_and(|address| {
                    qos_actions::door::resolve(&resources, &address).is_some()
                        || providers[0].resolve_action_resource(&address).is_some()
                })),
            "every search hit resolves eagerly or through the lazy index"
        );

        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.set_providers(Widget::search_providers(&app));
        palette.open();
        palette.set_query("Rent");
        let cell_resource = Widget::resource(&app, &doc, &rent_resource_id)
            .expect("host resolves the lazy cell through the owning widget");
        let mut current_authority = resources.clone();
        current_authority.push(cell_resource);
        palette.refresh_open_resources(&current_authority);
        let row = palette
            .result_labels()
            .iter()
            .position(|label| label == "Budget!A2")
            .expect("cell content produces a named result");
        palette.set_cursor(row);
        palette.activate_selected();
        let operation = palette.take_operation().expect("broker operation");
        assert_eq!(operation.kind, qos_actions::OperationKind::View);
        assert!(palette.take_navigation().is_none(), "no legacy callback");
        assert!(
            Widget::invoke_resource_operation(
                &mut app,
                operation.kind,
                &operation.encoded.type_id,
                &operation.encoded.payload,
                &doc,
                &ROOT,
            )
            .expect("view operation succeeds")
        );
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.active_sheet_index(), 0);
        assert_eq!(app.active_cell(), (0, 1), "Rent is exactly Budget!A2");

        let resources = Widget::resources(&app, &doc);
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.set_providers(Widget::search_providers(&app));
        palette.open();
        palette.set_query("Sales");
        let row = palette
            .result_labels()
            .iter()
            .position(|label| label == "Sales")
            .expect("sheet name produces a result");
        palette.set_cursor(row);
        palette.activate_selected();
        let operation = palette.take_operation().expect("sheet broker operation");
        assert_eq!(operation.kind, qos_actions::OperationKind::View);
        assert!(
            Widget::invoke_resource_operation(
                &mut app,
                operation.kind,
                &operation.encoded.type_id,
                &operation.encoded.payload,
                &doc,
                &ROOT,
            )
            .expect("sheet view operation succeeds")
        );
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.active_sheet_index(), 1, "Sales is the exact sheet");
        assert_eq!(app.active_cell(), (0, 0), "sheet hits land at A1");
    }
}
