//! Code's global-search projection over canonical resource view actions.
//!
//! The palette is allowed to be fast, but it must not quietly become a view of
//! the first N files in a project. The app retains a complete, incremental
//! catalogue while collaborative state changes; palette keystrokes only ask
//! that catalogue for one budgeted continuation page.

use ui::icons::Icon;
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchResult};

use crate::actions::{SearchResultRef, SymbolRef};
use crate::model::NodeId;

pub const OPEN_KIND: &str = "Open files";
pub const PROJECT_KIND: &str = "Project files";
pub const SYMBOL_KIND: &str = "Symbols";
pub const RESULT_KIND: &str = "Search results";

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub node: NodeId,
    pub resource_id: String,
    pub name: String,
    pub path: String,
    pub open: bool,
}

/// A cheap handle to Code's retained complete search projection. Cloning this
/// provider does not rescan buffers, project paths, results, or LSP state.
#[derive(Clone)]
pub struct FileSearchProvider {
    catalog: SearchCatalog,
}

impl FileSearchProvider {
    pub fn new(
        files: Vec<FileEntry>,
        symbols: Vec<SymbolRef>,
        results: Vec<SearchResultRef>,
    ) -> Self {
        let catalog = SearchCatalog::new("Code");
        Self::index_into(&catalog, files, symbols, results);
        Self { catalog }
    }

    pub fn from_catalog(catalog: SearchCatalog) -> Self {
        Self { catalog }
    }

    /// Update a retained catalogue from a coherent app-state projection. This
    /// work deliberately happens during state synchronization, never while the
    /// palette is handling an input character.
    pub fn index_into(
        catalog: &SearchCatalog,
        files: Vec<FileEntry>,
        symbols: Vec<SymbolRef>,
        results: Vec<SearchResultRef>,
    ) {
        for file in files {
            let kind = if file.open { OPEN_KIND } else { PROJECT_KIND };
            let address = crate::actions::open_file_address(&file.resource_id);
            let mut result = SearchResult::for_action(kind, file.name.clone(), address.clone())
                .with_icon(Icon::FileCode);
            if file.path != file.name && !file.path.is_empty() {
                result = result.with_subtitle(file.path.clone());
            }
            if file.open {
                result = result.with_score(1);
            }
            let _ = catalog.upsert(
                address.resource,
                format!("{}\n{}", file.name, file.path),
                result,
            );
        }
        for symbol in symbols {
            let address = crate::actions::open_symbol_address(&symbol);
            let subtitle = format!("{}:{}", symbol.path, symbol.line + 1);
            let searchable = format!("{}\n{}", symbol.name, symbol.path);
            let result = SearchResult::for_action(SYMBOL_KIND, symbol.name, address.clone())
                .with_subtitle(subtitle)
                .with_icon(Icon::Code);
            let _ = catalog.upsert(address.resource, searchable, result);
        }
        for result_ref in results {
            let address = crate::actions::reveal_result_address(&result_ref);
            let subtitle = format!("{}:{}", result_ref.path, result_ref.line + 1);
            let searchable = format!("{}\n{}", result_ref.preview, result_ref.path);
            let result = SearchResult::for_action(RESULT_KIND, result_ref.preview, address.clone())
                .with_subtitle(subtitle)
                .with_icon(Icon::Search);
            let _ = catalog.upsert(address.resource, searchable, result);
        }
    }

    #[cfg(test)]
    fn stats(&self) -> ui::search::SearchCatalogStats {
        self.catalog.stats()
    }
}

impl CatalogSearchProvider for FileSearchProvider {
    fn provider_name(&self) -> &'static str {
        "Code"
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }
}

/// Extract common declaration forms without requiring an optional LSP. The
/// coordinates are character-based, like the editor and Automerge text model.
pub fn symbols_in(file_id: &str, path: &str, text: &str) -> Vec<SymbolRef> {
    text.lines()
        .enumerate()
        .filter_map(|(line, source)| {
            let trimmed = source.trim_start();
            let prefixes = [
                "pub fn ",
                "fn ",
                "pub struct ",
                "struct ",
                "pub enum ",
                "enum ",
                "pub trait ",
                "trait ",
                "class ",
                "def ",
                "function ",
                "interface ",
                "type ",
                "const ",
            ];
            let rest = prefixes
                .iter()
                .find_map(|prefix| trimmed.strip_prefix(prefix))?;
            let name = rest
                .split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                .next()
                .unwrap_or("");
            if name.is_empty() {
                return None;
            }
            let col = source.chars().take_while(|ch| ch.is_whitespace()).count()
                + trimmed.find(name).unwrap_or(0);
            Some(SymbolRef {
                file_id: file_id.to_string(),
                name: name.to_string(),
                path: path.to_string(),
                line,
                col,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchProvider;

    fn file(node: usize, name: &str, path: &str) -> FileEntry {
        FileEntry {
            node,
            resource_id: format!("test-file-{node}"),
            name: name.into(),
            path: path.into(),
            open: false,
        }
    }

    #[test]
    fn symbol_extraction_keeps_exact_character_location() {
        let symbols = symbols_in(
            "test-file",
            "src/main.rs",
            "  pub fn crab() {}\nlet x = 1;\n",
        );
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "crab");
        assert_eq!((symbols[0].line, symbols[0].col), (0, 9));
    }

    #[test]
    fn file_search_finds_late_partial_and_unicode_matches() {
        // Each phase builds its own provider.
        complete_projection_keeps_a_late_file_findable();
        partial_matches_are_not_hidden_by_exact_file_names();
        unicode_and_embedded_nul_grams_are_unambiguous();
    }

    fn complete_projection_keeps_a_late_file_findable() {
        let mut files = (0..20_000)
            .map(|node| {
                file(
                    node,
                    &format!("file-{node}.rs"),
                    &format!("src/file-{node}.rs"),
                )
            })
            .collect::<Vec<_>>();
        files.push(file(20_000, "needle.rs", "deep/needle.rs"));
        let provider = FileSearchProvider::new(files, Vec::new(), Vec::new());
        let found = provider.search_limited("deep/needle", 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "needle.rs");
    }

    fn partial_matches_are_not_hidden_by_exact_file_names() {
        let provider = FileSearchProvider::new(
            vec![
                file(1, "needle.rs", "src/needle.rs"),
                file(2, "main.rs", "src/contains-needle-here.rs"),
            ],
            Vec::new(),
            Vec::new(),
        );
        let found = provider.search_limited("needle", 10);
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|hit| hit.title == "needle.rs"));
        assert!(found.iter().any(|hit| hit.title == "main.rs"));
    }

    fn unicode_and_embedded_nul_grams_are_unambiguous() {
        let provider = FileSearchProvider::new(
            vec![
                file(1, "éclair.rs", "src/éclair.rs"),
                file(2, "nul.rs", "src/a\0b.rs"),
            ],
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(provider.search_limited("ÉCL", 10).len(), 1);
        assert_eq!(provider.search_limited("a\0b", 10).len(), 1);
    }

    #[test]
    fn file_search_pages_are_budgeted_and_restart_after_mutation() {
        // Each phase builds its own provider.
        pages_have_hard_inspection_budgets_and_reach_late_matches();
        mutation_invalidates_a_stale_cursor_without_skipping_the_new_revision();
    }

    fn pages_have_hard_inspection_budgets_and_reach_late_matches() {
        let files = (0..1_000)
            .map(|node| {
                file(
                    node,
                    &format!("common-{node}.rs"),
                    &format!("src/common-{node}.rs"),
                )
            })
            .chain(std::iter::once(file(1_001, "needle.rs", "late/needle.rs")))
            .collect();
        let provider = FileSearchProvider::new(files, Vec::new(), Vec::new());
        assert!(
            provider
                .search_page("needle", None, 0, 1)
                .results
                .is_empty()
        );
        let first = provider.search_page("common", None, 3, 1);
        assert!(first.inspected <= 3);
        assert!(first.next.is_some());

        let mut cursor = None;
        let mut saw_late = false;
        for _ in 0..600 {
            let page = provider.search_page("common", cursor.as_ref(), 2, 1);
            assert!(page.inspected <= 2);
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        let needle = provider.search_page("late/needle", None, 1, 1);
        saw_late |= needle.results.iter().any(|hit| hit.title == "needle.rs");
        assert!(saw_late, "a late corpus row must remain discoverable");
    }

    fn mutation_invalidates_a_stale_cursor_without_skipping_the_new_revision() {
        let provider = FileSearchProvider::new(
            vec![
                file(1, "alpha.rs", "src/alpha.rs"),
                file(2, "beta.rs", "src/beta.rs"),
            ],
            Vec::new(),
            Vec::new(),
        );
        let stale = provider
            .search_page(".rs", None, 1, 1)
            .next
            .expect("continuation");
        FileSearchProvider::index_into(
            &provider.catalog,
            vec![file(3, "fresh.rs", "src/fresh.rs")],
            Vec::new(),
            Vec::new(),
        );
        let restarted = provider.search_page("fresh", Some(&stale), 1, 1);
        assert_eq!(restarted.results[0].title, "fresh.rs");
        assert!(provider.stats().generation > 0);
    }
}
