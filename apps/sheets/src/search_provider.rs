//! Sheets as a qOS global-search source.
//!
//! Queries run over a document-head-keyed in-memory projection, never over the
//! CRDT. Sheet and populated-cell hits address stable exported `open` View
//! actions, so search, agents and accessibility all use the brokered route and
//! land on the exact tab and selection.

use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_INDEX_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult,
};

pub const SHEET_KIND: &str = "Sheets";
pub const CELL_KIND: &str = "Sheet cells";

pub struct SheetSearchProvider {
    sheets: Vec<crate::actions::SearchableSheet>,
}

impl SheetSearchProvider {
    pub fn new(sheets: &[crate::actions::SearchableSheet]) -> Self {
        let mut remaining = DEFAULT_SEARCH_INDEX_LIMIT;
        let sheets = sheets
            .iter()
            .take(DEFAULT_SEARCH_INDEX_LIMIT)
            .map(|sheet| {
                let cells = sheet
                    .cells
                    .iter()
                    .take(remaining)
                    .cloned()
                    .collect::<Vec<_>>();
                remaining = remaining.saturating_sub(cells.len());
                crate::actions::SearchableSheet {
                    id: sheet.id.clone(),
                    name: sheet.name.clone(),
                    cells,
                }
            })
            .collect();
        Self { sheets }
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
        let limit = limit.min(DEFAULT_SEARCH_RESULT_LIMIT);
        if limit == 0 {
            return Vec::new();
        }
        let q = query.to_lowercase();
        let mut results = Vec::with_capacity(limit.min(32));
        for sheet in &self.sheets {
            if sheet.name.to_lowercase().contains(&q) {
                results.push(
                    SearchResult::for_action(
                        SHEET_KIND,
                        sheet.name.clone(),
                        crate::actions::open_sheet_address(&sheet.id),
                    )
                    .with_icon(Icon::LayoutGrid),
                );
                if results.len() == limit {
                    return results;
                }
            }
        }
        for sheet in &self.sheets {
            for cell in &sheet.cells {
                let reference =
                    format!("{}{}", crate::actions::column_label(cell.col), cell.row + 1);
                let qualified = format!("{}!{reference}", sheet.name);
                if q.is_empty()
                    || reference.to_lowercase().contains(&q)
                    || qualified.to_lowercase().contains(&q)
                    || cell.raw.to_lowercase().contains(&q)
                    || cell.display.to_lowercase().contains(&q)
                {
                    let subtitle = if cell.display.is_empty() || cell.display == cell.raw {
                        cell.raw.clone()
                    } else {
                        format!("{} · {}", cell.display, cell.raw)
                    };
                    results.push(
                        SearchResult::for_action(
                            CELL_KIND,
                            qualified,
                            crate::actions::open_cell_address(&sheet.id, cell.col, cell.row),
                        )
                        .with_subtitle(subtitle)
                        .with_icon(Icon::LayoutGrid),
                    );
                    if results.len() == limit {
                        return results;
                    }
                }
            }
        }
        results
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
