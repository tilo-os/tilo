//! Code global search backed exclusively by canonical resource view actions.
//!
//! Files, symbols discovered in collaborative buffers, and the current
//! project-search results all name an action in Code's exported resource tree.

use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_INDEX_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult,
};

use crate::actions::{SearchResultRef, SymbolRef};
use crate::model::NodeId;

pub const OPEN_KIND: &str = "Open files";
pub const PROJECT_KIND: &str = "Project files";
pub const SYMBOL_KIND: &str = "Symbols";
pub const RESULT_KIND: &str = "Search results";

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub node: NodeId,
    pub name: String,
    pub path: String,
    pub open: bool,
}

/// A bounded snapshot of everything Code contributes to global search.
pub struct FileSearchProvider {
    files: Vec<FileEntry>,
    symbols: Vec<SymbolRef>,
    results: Vec<SearchResultRef>,
}

impl FileSearchProvider {
    pub fn new(
        files: Vec<FileEntry>,
        symbols: Vec<SymbolRef>,
        results: Vec<SearchResultRef>,
    ) -> Self {
        let files = files
            .into_iter()
            .take(DEFAULT_SEARCH_INDEX_LIMIT)
            .collect::<Vec<_>>();
        let remaining = DEFAULT_SEARCH_INDEX_LIMIT.saturating_sub(files.len());
        let symbols = symbols.into_iter().take(remaining).collect::<Vec<_>>();
        let remaining = remaining.saturating_sub(symbols.len());
        Self {
            files,
            symbols,
            results: results.into_iter().take(remaining).collect(),
        }
    }

    fn matches(query: &str, values: &[&str]) -> bool {
        query.is_empty()
            || values
                .iter()
                .any(|value| value.to_lowercase().contains(query))
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
        let q = query.trim().to_lowercase();
        let limit = limit.min(DEFAULT_SEARCH_RESULT_LIMIT);
        let mut rows = self
            .files
            .iter()
            .filter(|file| Self::matches(&q, &[&file.name, &file.path]))
            .map(|file| {
                let kind = if file.open { OPEN_KIND } else { PROJECT_KIND };
                let mut row = SearchResult::for_action(
                    kind,
                    file.name.clone(),
                    crate::actions::open_file_address(file.node as u64),
                )
                .with_icon(Icon::FileCode);
                if file.path != file.name && !file.path.is_empty() {
                    row = row.with_subtitle(file.path.clone());
                }
                if file.open {
                    row = row.with_score(1);
                }
                row
            })
            .take(limit)
            .collect::<Vec<_>>();

        if rows.len() == limit {
            return rows;
        }
        let remaining = limit - rows.len();

        rows.extend(
            self.symbols
                .iter()
                .filter(|symbol| Self::matches(&q, &[&symbol.name, &symbol.path]))
                .map(|symbol| {
                    SearchResult::for_action(
                        SYMBOL_KIND,
                        symbol.name.clone(),
                        crate::actions::open_symbol_address(symbol),
                    )
                    .with_subtitle(format!("{}:{}", symbol.path, symbol.line + 1))
                    .with_icon(Icon::Code)
                })
                .take(remaining),
        );
        if rows.len() == limit {
            return rows;
        }
        let remaining = limit - rows.len();
        rows.extend(
            self.results
                .iter()
                .filter(|result| Self::matches(&q, &[&result.preview, &result.path]))
                .map(|result| {
                    SearchResult::for_action(
                        RESULT_KIND,
                        result.preview.clone(),
                        crate::actions::reveal_result_address(result),
                    )
                    .with_subtitle(format!("{}:{}", result.path, result.line + 1))
                    .with_icon(Icon::Search)
                })
                .take(remaining),
        );
        rows
    }
}

/// Extract common declaration forms without requiring an optional LSP. The
/// coordinates are character-based, like the editor and Automerge text model.
pub fn symbols_in(node: NodeId, path: &str, text: &str) -> Vec<SymbolRef> {
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
                node: node as u64,
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

    #[test]
    fn symbol_extraction_keeps_exact_character_location() {
        let symbols = symbols_in(7, "src/main.rs", "  pub fn crab() {}\nlet x = 1;\n");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "crab");
        assert_eq!((symbols[0].line, symbols[0].col), (0, 9));
    }

    #[test]
    fn large_projection_is_bounded_before_queries_materialise_rows() {
        let files = (0..(DEFAULT_SEARCH_INDEX_LIMIT + 1_000))
            .map(|node| FileEntry {
                node,
                name: format!("file-{node}.rs"),
                path: format!("src/file-{node}.rs"),
                open: false,
            })
            .collect();
        let provider = FileSearchProvider::new(files, Vec::new(), Vec::new());
        assert_eq!(provider.files.len(), DEFAULT_SEARCH_INDEX_LIMIT);
        assert_eq!(provider.search_limited("file", 9).len(), 9);
        assert!(provider.search_limited("file", 0).is_empty());
    }
}
