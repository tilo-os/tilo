//! Code's global-search projection of what only the editor knows: the symbols
//! declared in its files and the hits of the running project search. The
//! files themselves are the resource's own search entries.
//!
//! The app keeps these entries current while collaborative state changes;
//! palette keystrokes only read them, and the framework's search source
//! answers each query one budgeted page at a time.

use ui::tilo_resource::SearchEntry;

use crate::actions::{SearchResultRef, SymbolRef};
use crate::model::NodeId;

pub const SYMBOL_KIND: &str = "Symbols";
pub const RESULT_KIND: &str = "Search results";

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub node: NodeId,
    pub resource_id: String,
    pub name: String,
    pub path: String,
}

/// The declarations found in the collaborative buffers and the live project
/// search results, each opening where it is.
pub fn code_entries(symbols: &[SymbolRef], results: &[SearchResultRef]) -> Vec<SearchEntry> {
    let symbols = symbols.iter().map(|symbol| {
        SearchEntry::new(symbol.name.clone(), String::new())
            .invoking(crate::actions::open_symbol_address(symbol))
            .grouped(SYMBOL_KIND)
            .subtitled(format!("{}:{}", symbol.path, symbol.line + 1))
            .with_icon("code")
    });
    let results = results.iter().map(|result| {
        SearchEntry::new(result.preview.clone(), String::new())
            .invoking(crate::actions::reveal_result_address(result))
            .grouped(RESULT_KIND)
            .subtitled(format!("{}:{}", result.path, result.line + 1))
            .with_icon("search")
    });
    symbols.chain(results).collect()
}

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

    /// The editor's search source over `symbols`, as the framework builds it.
    fn source(symbols: &[SymbolRef]) -> impl SearchProvider + use<> {
        ui::framework::testing::search_source::<crate::project::Project>(code_entries(symbols, &[]))
    }

    fn symbol(index: usize, name: &str, path: &str) -> SymbolRef {
        SymbolRef {
            file_id: format!("test-file-{index}"),
            name: name.into(),
            path: path.into(),
            line: index,
            col: 0,
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
    fn symbol_search_finds_late_partial_and_unicode_matches() {
        complete_projection_keeps_a_late_file_findable();
        partial_matches_are_not_hidden_by_exact_file_names();
        unicode_and_embedded_nul_grams_are_unambiguous();
    }

    fn complete_projection_keeps_a_late_file_findable() {
        let mut symbols = (0..20_000)
            .map(|index| {
                symbol(
                    index,
                    &format!("item_{index}"),
                    &format!("src/file-{index}.rs"),
                )
            })
            .collect::<Vec<_>>();
        symbols.push(symbol(20_000, "needle", "deep/needle.rs"));
        let provider = source(&symbols);
        let found = provider.search_limited("deep/needle", 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "needle");
    }

    fn partial_matches_are_not_hidden_by_exact_file_names() {
        let provider = source(&[
            symbol(1, "needle", "src/needle.rs"),
            symbol(2, "main", "src/contains-needle-here.rs"),
        ]);
        let found = provider.search_limited("needle", 10);
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|hit| hit.title == "needle"));
        assert!(found.iter().any(|hit| hit.title == "main"));
    }

    fn unicode_and_embedded_nul_grams_are_unambiguous() {
        let provider = source(&[
            symbol(1, "éclair", "src/éclair.rs"),
            symbol(2, "nul", "src/a\0b.rs"),
        ]);
        assert_eq!(provider.search_limited("ÉCL", 10).len(), 1);
        assert_eq!(provider.search_limited("a\0b", 10).len(), 1);
    }

    #[test]
    fn symbol_search_pages_are_budgeted_and_restart_after_mutation() {
        pages_have_hard_inspection_budgets_and_reach_late_matches();
        mutation_invalidates_a_stale_cursor_without_skipping_the_new_revision();
    }

    fn pages_have_hard_inspection_budgets_and_reach_late_matches() {
        let symbols = (0..1_000)
            .map(|index| {
                symbol(
                    index,
                    &format!("common_{index}"),
                    &format!("src/common-{index}.rs"),
                )
            })
            .chain(std::iter::once(symbol(1_001, "needle", "late/needle.rs")))
            .collect::<Vec<_>>();
        let provider = source(&symbols);
        assert!(
            provider
                .search_page(&ui::search::SearchQuery::new("needle"), None, 0, 1)
                .results
                .is_empty()
        );
        let first = provider.search_page(&ui::search::SearchQuery::new("common"), None, 3, 1);
        assert!(first.inspected <= 3);
        assert!(first.next.is_some());

        let mut cursor = None;
        let mut saw_late = false;
        for _ in 0..600 {
            let page = provider.search_page(
                &ui::search::SearchQuery::new("common"),
                cursor.as_ref(),
                2,
                1,
            );
            assert!(page.inspected <= 2);
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        let needle = provider.search_page(&ui::search::SearchQuery::new("late/needle"), None, 1, 1);
        saw_late |= needle.results.iter().any(|hit| hit.title == "needle");
        assert!(saw_late, "a late corpus row must remain discoverable");
    }

    fn mutation_invalidates_a_stale_cursor_without_skipping_the_new_revision() {
        let provider = source(&[
            symbol(1, "alpha", "src/alpha.rs"),
            symbol(2, "beta", "src/beta.rs"),
        ]);
        let stale = provider
            .search_page(&ui::search::SearchQuery::new(".rs"), None, 1, 1)
            .next
            .expect("continuation");
        let fresh = source(&[symbol(3, "fresh", "src/fresh.rs")]);
        let restarted =
            fresh.search_page(&ui::search::SearchQuery::new("fresh"), Some(&stale), 1, 1);
        assert_eq!(restarted.results[0].title, "fresh");
    }
}
