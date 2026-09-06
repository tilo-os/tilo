//! Code's global-search projection over canonical resource view actions.
//!
//! The palette is allowed to be fast, but it must not quietly become a view of
//! the first N files in a project. This provider snapshots every resource Code
//! exports, builds a compact Unicode-scalar 1/2/3-gram index once, and uses the
//! index to bound full-string verification for one query.

use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult,
    SearchTextIndex, normalize_search_text,
};

use crate::actions::{SearchResultRef, SymbolRef};
use crate::model::NodeId;

pub const OPEN_KIND: &str = "Open files";
pub const PROJECT_KIND: &str = "Project files";
pub const SYMBOL_KIND: &str = "Symbols";
pub const RESULT_KIND: &str = "Search results";

/// The index is complete, while verification is deliberately bounded. A common
/// query must never inspect unbounded source previews on the UI thread.
const MAX_QUERY_CANDIDATES: usize = DEFAULT_SEARCH_CANDIDATE_LIMIT;

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub node: NodeId,
    pub resource_id: String,
    pub name: String,
    pub path: String,
    pub open: bool,
}

struct SearchEntry {
    result: SearchResult,
    title: String,
}

/// A complete snapshot of every Code resource which participates in global
/// search. `entries` is intentionally never capped: an omitted resource cannot
/// be found.
pub struct FileSearchProvider {
    entries: Vec<SearchEntry>,
    /// qOS's shared Unicode-aware complete-corpus text index. Keeping this in
    /// the toolkit makes search semantics, memory behavior and candidate
    /// bounds consistent for every app.
    index: SearchTextIndex,
}

impl FileSearchProvider {
    pub fn new(
        files: Vec<FileEntry>,
        symbols: Vec<SymbolRef>,
        results: Vec<SearchResultRef>,
    ) -> Self {
        let capacity = files.len() + symbols.len() + results.len();
        let mut entries = Vec::with_capacity(capacity);
        let mut texts = Vec::with_capacity(capacity);
        for file in files {
            let kind = if file.open { OPEN_KIND } else { PROJECT_KIND };
            let mut result = SearchResult::for_action(
                kind,
                file.name.clone(),
                crate::actions::open_file_address(&file.resource_id),
            )
            .with_icon(Icon::FileCode);
            if file.path != file.name && !file.path.is_empty() {
                result = result.with_subtitle(file.path.clone());
            }
            if file.open {
                result = result.with_score(1);
            }
            entries.push(SearchEntry {
                title: normalize_search_text(&file.name),
                result,
            });
            texts.push(format!("{}\n{}", file.name, file.path));
        }
        for symbol in symbols {
            let address = crate::actions::open_symbol_address(&symbol);
            let subtitle = format!("{}:{}", symbol.path, symbol.line + 1);
            texts.push(format!("{}\n{}", symbol.name, symbol.path));
            entries.push(SearchEntry {
                title: normalize_search_text(&symbol.name),
                result: SearchResult::for_action(SYMBOL_KIND, symbol.name, address)
                    .with_subtitle(subtitle)
                    .with_icon(Icon::Code),
            });
        }
        for result_ref in results {
            let address = crate::actions::reveal_result_address(&result_ref);
            let subtitle = format!("{}:{}", result_ref.path, result_ref.line + 1);
            texts.push(format!("{}\n{}", result_ref.preview, result_ref.path));
            entries.push(SearchEntry {
                title: normalize_search_text(&result_ref.preview),
                result: SearchResult::for_action(RESULT_KIND, result_ref.preview, address)
                    .with_subtitle(subtitle)
                    .with_icon(Icon::Search),
            });
        }
        let index = SearchTextIndex::new(texts);
        Self { entries, index }
    }
}

impl SearchProvider for FileSearchProvider {
    fn name(&self) -> &'static str {
        "Code"
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let query = normalize_search_text(query.trim());
        let limit = limit.min(DEFAULT_SEARCH_RESULT_LIMIT);
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
        let mut matches = self
            .index
            .matching_ids(&query, MAX_QUERY_CANDIDATES)
            .into_iter()
            .filter_map(|index| self.entries.get(index))
            .collect::<Vec<_>>();
        // The shared index deliberately keeps partial matches. Within its hard
        // candidate bound, a title-prefix match is the most direct file/symbol
        // intent and should beat a path-only partial.
        matches.sort_by_key(|entry| {
            (
                !entry.title.starts_with(&query),
                !entry.title.contains(&query),
            )
        });
        matches
            .into_iter()
            .take(limit)
            .map(|entry| entry.result.clone())
            .collect()
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

    #[test]
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
        assert_eq!(found[0].title, "needle.rs");
        assert_eq!(found[1].title, "main.rs");
    }

    #[test]
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
    fn queries_are_bounded_and_zero_limit_never_materialises_rows() {
        let files = (0..(MAX_QUERY_CANDIDATES * 4))
            .map(|node| {
                file(
                    node,
                    &format!("aaaa-{node}.rs"),
                    &format!("src/aaaa-{node}.rs"),
                )
            })
            .collect();
        let provider = FileSearchProvider::new(files, Vec::new(), Vec::new());
        assert!(provider.search_limited("a", 0).is_empty());
        assert_eq!(
            provider
                .search_limited("a", DEFAULT_SEARCH_RESULT_LIMIT)
                .len(),
            DEFAULT_SEARCH_RESULT_LIMIT
        );
    }
}
