//! Sheets as a retained Tilo global-search source.
//!
//! The app refreshes this catalogue only after its Automerge heads change.
//! Palette opens and individual keystrokes receive cheap clones of this
//! provider, never a freshly-built index or a CRDT walk. Every indexed row is
//! keyed by the canonical resource id it opens, and the catalogue's cursors
//! keep broad searches bounded without hiding a late row in a large workbook.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchResult};

use ui::icons::Icon;

pub const SHEET_KIND: &str = "Sheets";
pub const CELL_KIND: &str = "Sheet cells";

/// A durable, incrementally reconciled search projection of a workbook.
///
/// `SearchCatalog` is internally reference counted, so the app can retain one
/// instance while each Ctrl+K session receives a provider clone. `cells` is a
/// small exact-resolution map for search hits that are deliberately omitted
/// from the eager resource tree.
#[derive(Clone)]
pub struct SheetSearchProvider {
    catalog: SearchCatalog,
    indexed_ids: Arc<RwLock<BTreeSet<String>>>,
    cells: Arc<RwLock<BTreeMap<String, (String, crate::actions::SearchableCell)>>>,
}

impl SheetSearchProvider {
    /// Make a self-contained provider for tests and short-lived callers. The
    /// live app instead retains this instance and calls [`Self::refresh`] at a
    /// document-revision boundary.
    pub fn new(sheets: &[crate::actions::SearchableSheet]) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new(SHEET_KIND),
            indexed_ids: Arc::new(RwLock::new(BTreeSet::new())),
            cells: Arc::new(RwLock::new(BTreeMap::new())),
        };
        provider.refresh(sheets);
        provider
    }

    fn index_sheet(
        &self,
        sheet: &crate::actions::SearchableSheet,
        current: &mut BTreeSet<String>,
        cells: &mut BTreeMap<String, (String, crate::actions::SearchableCell)>,
    ) {
        let sheet_id = crate::actions::sheet_resource_id(&sheet.id);
        let sheet_result = SearchResult::for_action(
            SHEET_KIND,
            &sheet.name,
            crate::actions::open_sheet_address(&sheet.id),
        )
        .with_icon(Icon::LayoutGrid);
        let _ = self
            .catalog
            .upsert(sheet_id.clone(), &sheet.name, sheet_result);
        current.insert(sheet_id);

        for cell in &sheet.cells {
            let reference = format!("{}{}", crate::actions::column_label(cell.col), cell.row + 1);
            let qualified = format!("{}!{reference}", sheet.name);
            // The row's title is the cell; its one secondary line is what the
            // cell shows, or what was typed into it when it shows nothing.
            let subtitle = if cell.display.is_empty() {
                cell.raw.clone()
            } else {
                cell.display.clone()
            };
            let result = SearchResult::for_action(
                CELL_KIND,
                qualified.clone(),
                crate::actions::open_cell_address(&sheet.id, cell.col, cell.row),
            )
            .with_subtitle(subtitle)
            .with_icon(Icon::LayoutGrid);
            let id = crate::actions::cell_resource_id(&sheet.id, cell.col, cell.row);
            // NUL is a field separator: searching cannot accidentally bridge
            // an A1 reference and a value as one synthetic term.
            let searchable = format!("{reference}\0{qualified}\0{}\0{}", cell.raw, cell.display);
            let _ = self.catalog.upsert(id.clone(), searchable, result);
            current.insert(id.clone());
            cells.insert(id, (sheet.id.clone(), cell.clone()));
        }
    }

    /// Reconcile a new, complete workbook projection. Unchanged rows leave
    /// their postings and catalogue generation intact; removed rows are
    /// dropped, making stale search addresses unresolvable.
    pub fn refresh(&self, sheets: &[crate::actions::SearchableSheet]) {
        let mut current = BTreeSet::new();
        let mut cells = BTreeMap::new();
        for sheet in sheets {
            self.index_sheet(sheet, &mut current, &mut cells);
        }
        let mut indexed = self
            .indexed_ids
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for removed in indexed.difference(&current) {
            self.catalog.remove(removed);
        }
        *indexed = current;
        drop(indexed);
        *self
            .cells
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = cells;
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> ui::search::SearchCatalogStats {
        self.catalog.stats()
    }
}

impl CatalogSearchProvider for SheetSearchProvider {
    fn provider_name(&self) -> &str {
        SHEET_KIND
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }

    fn resolve_action_address(
        &self,
        address: &tilo_actions::ActionAddress,
    ) -> Option<tilo_actions::Resource> {
        let cell = self
            .cells
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&address.resource)
            .cloned()?;
        (address == &crate::actions::open_cell_address(&cell.0, cell.1.col, cell.1.row))
            .then(|| crate::actions::searchable_cell_resource(&cell.0, &cell.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{SearchableCell, SearchableSheet};
    use ui::search::SearchProvider;
    use ui::search::SearchRegistry;
    use ui::search::testing::{LATE_HIT_NEEDLE, assert_late_hit_reachable};

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

    /// Canonical hits, browsing, incremental refresh and paged budgets.
    #[test]
    fn sheet_search_addresses_browses_refreshes_and_pages() {
        // Case `sheet_and_cell_hits_use_canonical_addresses`.
        {
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

        // Case `empty_query_browses_sheets_and_populated_cells`.
        {
            let mut registry = SearchRegistry::new();
            registry.register(Box::new(SheetSearchProvider::new(&sheets())));
            let hits = registry.query("");
            assert_eq!(hits.len(), 3);
            assert!(hits.iter().all(|hit| hit.action_address().is_some()));
        }

        // Case `refresh_is_incremental_and_removes_stale_cells`.
        {
            let provider = SheetSearchProvider::new(&sheets());
            let before = provider.stats();
            provider.refresh(&sheets());
            assert_eq!(
                provider.stats(),
                before,
                "identical revisions do not reindex"
            );
            let mut revised = sheets();
            revised[0].cells.clear();
            provider.refresh(&revised);
            assert!(provider.search("4,200").is_empty());
            assert!(
                provider
                    .resolve_action_resource(&crate::actions::open_cell_address("budget-id", 0, 0))
                    .is_none()
            );
        }

        // Case `late_cells_are_reachable_in_a_large_catalog_with_paged_work_budgets`.
        {
            let (provider, hit) = assert_late_hit_reachable(
                |corpus| {
                    let large = vec![SearchableSheet {
                        id: "large-id".into(),
                        name: "Large workbook".into(),
                        cells: corpus
                            .map(|row| SearchableCell {
                                col: 0,
                                row: u32::try_from(row.index).expect("corpus fits a sheet"),
                                raw: row.text,
                                display: String::new(),
                            })
                            .collect(),
                    }];
                    SheetSearchProvider::new(&large)
                },
                3_000,
            );
            assert_eq!(hit.title, "Large workbook!A3000");
            let needle = provider.search_page(&LATE_HIT_NEEDLE.into(), None, 1, 1);
            assert_eq!(needle.results, vec![hit]);
            assert!(needle.inspected <= 1);

            let mut cursor = None;
            let mut total = 0;
            loop {
                let page = provider.search_page(&"ordinary".into(), cursor.as_ref(), 17, 5);
                assert!(page.inspected <= 17);
                assert!(page.results.len() <= 5);
                total += page.results.len();
                cursor = page.next;
                if cursor.is_none() || total > 20 {
                    break;
                }
            }
            assert!(total >= 20, "continuation makes broad matches reachable");
        }
    }
}
