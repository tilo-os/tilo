//! Code's global-search projection over canonical resource view actions.
//!
//! The palette is allowed to be fast, but it must not quietly become a view of
//! the first N files in a project. The app retains a complete, incremental
//! catalogue while collaborative state changes; palette keystrokes only ask
//! that catalogue for one budgeted continuation page.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use ui::icons::Icon;
use ui::search::{
    CatalogSearchProvider, PagePoll, PendingToken, SearchCatalog, SearchCursor, SearchPage,
    SearchQuery, SearchResult, SearchRow,
};

use crate::actions::{SearchResultRef, SymbolRef};
use crate::model::NodeId;
use crate::search;

pub const OPEN_KIND: &str = "Open files";
pub const PROJECT_KIND: &str = "Project files";
pub const SYMBOL_KIND: &str = "Symbols";
pub const RESULT_KIND: &str = "In files";

/// The fewest characters a project text search starts on. One letter matches
/// nearly every line, which is a walk of the whole tree for no information.
pub const MIN_TEXT_QUERY_CHARS: usize = 2;

/// How many hits one palette page carries. A broader query is a longer walk,
/// not a longer list: past this the person narrows the query.
pub const TEXT_HITS_PER_PAGE: usize = 200;

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

/// What the project text search is doing, shared between the editor (which
/// publishes the hits it holds as resources) and the providers the palette
/// asks (which start walks and read their progress).
#[derive(Default)]
struct TextSearchState {
    /// The project directory walked; `None` while no folder is open.
    root: Option<PathBuf>,
    /// Identifies the walk a [`PendingToken`] belongs to.
    generation: u64,
    job: Option<search::Job>,
    hits: Vec<search::Hit>,
    /// Why the last walk stopped short of the whole project, until the editor
    /// has told the person.
    limit: Option<SearchLimit>,
}

/// Why a project text search did not cover everything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchLimit {
    /// The list stopped at this many matches.
    Matches(usize),
    /// The walk stopped after this many files.
    Files(usize),
}

impl TextSearchState {
    /// Take whatever the worker produced since the last call.
    fn pump(&mut self) {
        let Some(job) = self.job.as_mut() else {
            return;
        };
        for msg in job.drain() {
            match msg {
                search::Msg::Hits(hits) => self.hits.extend(hits),
                search::Msg::Done {
                    files,
                    truncated: true,
                } => self.limit = Some(SearchLimit::Files(files)),
                search::Msg::Done { .. } => {}
            }
        }
        if self.hits.len() >= TEXT_HITS_PER_PAGE {
            self.hits.truncate(TEXT_HITS_PER_PAGE);
            self.limit = Some(SearchLimit::Matches(TEXT_HITS_PER_PAGE));
            if let Some(job) = self.job.take() {
                job.cancel();
            }
        } else if self.job.as_ref().is_some_and(search::Job::is_finished) {
            self.job = None;
        }
    }
}

/// Code's project text search: a walk of the open project on a worker thread
/// whose hits the platform search lists as they arrive. Cloning shares the
/// walk, so the editor and every provider it hands the palette see one
/// search.
#[derive(Clone, Default)]
pub struct ProjectTextSearch {
    state: Arc<Mutex<TextSearchState>>,
}

impl ProjectTextSearch {
    fn state(&self) -> std::sync::MutexGuard<'_, TextSearchState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Search under `root` (the open project), dropping what an earlier
    /// project found.
    pub fn set_root(&self, root: Option<PathBuf>) {
        let mut state = self.state();
        if state.root != root {
            if let Some(job) = state.job.take() {
                job.cancel();
            }
            state.hits.clear();
            state.root = root;
        }
    }

    /// Take the worker's progress, once a frame.
    pub fn pump(&self) {
        self.state().pump();
    }

    /// Why the last walk stopped short, once.
    pub fn take_limit(&self) -> Option<SearchLimit> {
        self.state().limit.take()
    }

    /// Whether a walk is still running.
    pub fn is_running(&self) -> bool {
        self.state().job.is_some()
    }

    /// The hits found so far, in the form the editor publishes as resources.
    pub fn result_refs(&self) -> Vec<(search::Hit, SearchResultRef)> {
        self.state()
            .hits
            .iter()
            .map(|hit| (hit.clone(), result_ref(hit)))
            .collect()
    }

    /// Start walking for `needle`, replacing a walk for another needle.
    fn begin(&self, needle: &str) -> Option<PendingToken> {
        let mut state = self.state();
        let root = state.root.clone()?;
        if let Some(job) = state.job.take() {
            job.cancel();
        }
        state.hits.clear();
        state.limit = None;
        state.generation = state.generation.wrapping_add(1);
        state.job = Some(search::start(&root, search::Query::new(needle)));
        Some(PendingToken::new(state.generation))
    }

    /// The page the walk behind `token` has reached: pending until the walk
    /// ends, then every hit.
    fn poll(&self, token: &PendingToken) -> PagePoll {
        let mut state = self.state();
        if token.id() != state.generation {
            // The query moved on; its walk is gone.
            return PagePoll::Ready(SearchPage::empty());
        }
        state.pump();
        if state.job.is_some() {
            return PagePoll::Pending;
        }
        PagePoll::Ready(SearchPage::of(
            state.hits.iter().map(hit_result).collect(),
            None,
        ))
    }
}

/// How the editor addresses a text hit: the reveal action of a search-result
/// resource at the hit's line and column.
fn result_ref(hit: &search::Hit) -> SearchResultRef {
    SearchResultRef {
        file_id: None,
        path: hit.rel.clone(),
        disk_path: Some(hit.path.display().to_string()),
        preview: hit.preview.clone(),
        line: hit.line.saturating_sub(1),
        col: hit.col,
    }
}

/// One text hit as a palette row: the file and line, then the matched line
/// with the match marked.
fn hit_result(hit: &search::Hit) -> SearchResult {
    let address = crate::actions::reveal_result_address(&result_ref(hit));
    let start = hit.col.saturating_sub(hit.preview_offset);
    let byte_at = |chars: usize| {
        hit.preview
            .char_indices()
            .nth(chars)
            .map_or(hit.preview.len(), |(at, _)| at)
    };
    let row = SearchRow::file_hit(hit.rel.clone(), hit.preview.clone())
        .line(u32::try_from(hit.line).unwrap_or(u32::MAX))
        .matched(byte_at(start)..byte_at(start.saturating_add(hit.len)));
    SearchResult::for_action(RESULT_KIND, hit.preview.clone(), address)
        .with_icon(Icon::FileCode)
        .with_row(row)
}

/// The palette's door onto [`ProjectTextSearch`]: asks the walk to start on
/// the first page of a query and answers the rest from `poll_page`, so typing
/// never waits on the filesystem.
pub struct TextSearchProvider {
    search: ProjectTextSearch,
}

impl TextSearchProvider {
    pub fn new(search: ProjectTextSearch) -> Self {
        Self { search }
    }
}

impl ui::search::SearchProvider for TextSearchProvider {
    fn name(&self) -> &str {
        RESULT_KIND
    }

    fn search(&self, _query: &str) -> Vec<SearchResult> {
        // Text hits arrive through `search_page`, never in one blocking call.
        Vec::new()
    }

    fn search_limited(&self, _query: &str, _limit: usize) -> Vec<SearchResult> {
        Vec::new()
    }

    fn search_page(
        &self,
        query: &SearchQuery,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        let needle = query.text().trim();
        if cursor.is_some()
            || work_budget == 0
            || result_limit == 0
            || needle.chars().count() < MIN_TEXT_QUERY_CHARS
        {
            return SearchPage::empty();
        }
        match self.search.begin(needle) {
            Some(token) => SearchPage::pending(Vec::new(), token),
            None => SearchPage::empty(),
        }
    }

    fn poll_page(&self, token: &PendingToken) -> PagePoll {
        self.search.poll(token)
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
                .search_page(&SearchQuery::new("needle"), None, 0, 1)
                .results
                .is_empty()
        );
        let first = provider.search_page(&SearchQuery::new("common"), None, 3, 1);
        assert!(first.inspected <= 3);
        assert!(first.next.is_some());

        let mut cursor = None;
        let mut saw_late = false;
        for _ in 0..600 {
            let page = provider.search_page(&SearchQuery::new("common"), cursor.as_ref(), 2, 1);
            assert!(page.inspected <= 2);
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        let needle = provider.search_page(&SearchQuery::new("late/needle"), None, 1, 1);
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
            .search_page(&SearchQuery::new(".rs"), None, 1, 1)
            .next
            .expect("continuation");
        FileSearchProvider::index_into(
            &provider.catalog,
            vec![file(3, "fresh.rs", "src/fresh.rs")],
            Vec::new(),
            Vec::new(),
        );
        let restarted = provider.search_page(&SearchQuery::new("fresh"), Some(&stale), 1, 1);
        assert_eq!(restarted.results[0].title, "fresh.rs");
        assert!(provider.stats().generation > 0);
    }
}
