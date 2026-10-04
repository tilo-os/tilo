//! Bounded, cursor-paged global search for browser history, bookmarks, and
//! device-local download resources.
//!
//! The retained index is refreshed only after a durable corpus revision.
//! Providers then clone it cheaply, so opening global search does not clone
//! or remap the full browser library.  A provider resolves action resources
//! against the current index, which refuses an address that went stale after
//! the result was rendered.

use tilo_actions::{
    ActionAddress, ActionContract, ActionSpec, ExecutionMode, OperationKind, Resource, ResultSpec,
    RetryPolicy,
};
use ui::icons::Icon;
use ui::search::{
    CatalogSearchProvider, Facet, PagePoll, PendingToken, ScopeDecl, SearchCatalog, SearchCursor,
    SearchPage, SearchProvider, SearchQuery, SearchResult,
};

const DEFAULT_RESULT_LIMIT: usize = 100;

#[derive(Clone)]
struct Row {
    resource_id: String,
    title: String,
    url: String,
    /// The exact resource kind published in Browser's hierarchy, not a
    /// search-only proxy type.  A search result must be interchangeable with
    /// opening the same row from Library.
    resource_kind: &'static str,
    search_kind: &'static str,
    icon: Icon,
    score: i32,
}

struct IndexState {
    history_revision: u64,
    bookmark_revision: u64,
    download_revision: u64,
    catalog: SearchCatalog,
    rows: std::collections::BTreeMap<String, Row>,
}

impl Default for IndexState {
    fn default() -> Self {
        Self {
            history_revision: u64::MAX,
            bookmark_revision: u64::MAX,
            download_revision: u64::MAX,
            catalog: SearchCatalog::new("Browser"),
            rows: std::collections::BTreeMap::new(),
        }
    }
}

#[derive(Clone, Default)]
pub struct BrowserSearchIndex {
    state: std::sync::Arc<std::sync::RwLock<IndexState>>,
}

impl BrowserSearchIndex {
    /// Force the next [`Self::sync`] to rebuild after a model replacement
    /// whose source does not carry this process-local revision token (for
    /// example an incoming CRDT bookmark projection).
    pub fn invalidate(&self) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.history_revision = u64::MAX;
        state.bookmark_revision = u64::MAX;
        state.download_revision = u64::MAX;
    }

    pub fn sync(&self, history: &crate::history::History, bookmarks: &crate::library::Bookmarks) {
        self.sync_all(history, bookmarks, &crate::downloads::Downloads::default());
    }

    pub fn sync_all(
        &self,
        history: &crate::history::History,
        bookmarks: &crate::library::Bookmarks,
        downloads: &crate::downloads::Downloads,
    ) {
        let history_revision = history.revision();
        let bookmark_revision = bookmarks.revision();
        let download_revision = downloads.revision();
        let current = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let unchanged = current.history_revision == history_revision
            && current.bookmark_revision == bookmark_revision
            && current.download_revision == download_revision;
        drop(current);
        if unchanged {
            return;
        }

        let catalog = SearchCatalog::new("Browser");
        let mut rows = std::collections::BTreeMap::new();
        for entry in bookmarks.entries() {
            let row = Row {
                resource_id: crate::actions::bookmark_resource_id(&entry.url),
                title: entry.title.clone(),
                url: entry.url.clone(),
                resource_kind: "browser.bookmark",
                search_kind: "Bookmarks",
                icon: Icon::Star,
                score: 40,
            };
            Self::index_row(&catalog, &mut rows, row);
        }
        for entry in history.entries() {
            let row = Row {
                resource_id: crate::actions::history_resource_id(&entry.url),
                title: if entry.title.trim().is_empty() {
                    entry.url.clone()
                } else {
                    entry.title.clone()
                },
                url: entry.url.clone(),
                resource_kind: "browser.history-entry",
                search_kind: "History",
                icon: Icon::History,
                score: i32::try_from(entry.visit_count.min(20)).unwrap_or(20),
            };
            Self::index_row(&catalog, &mut rows, row);
        }
        for entry in downloads.entries() {
            let destination = entry
                .destination
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_default();
            let row = Row {
                resource_id: entry.resource_id(),
                title: entry.display_name().to_string(),
                url: format!("{} {destination}", entry.source_url),
                resource_kind: "browser.download",
                search_kind: "Downloads",
                icon: Icon::Download,
                score: if entry.status == crate::downloads::DownloadStatus::Completed {
                    35
                } else {
                    20
                },
            };
            Self::index_row(&catalog, &mut rows, row);
        }

        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.history_revision = history_revision;
        state.bookmark_revision = bookmark_revision;
        state.download_revision = download_revision;
        state.catalog = catalog;
        state.rows = rows;
    }

    fn index_row(
        catalog: &SearchCatalog,
        rows: &mut std::collections::BTreeMap<String, Row>,
        row: Row,
    ) {
        let result = SearchResult::for_action(
            row.search_kind,
            &row.title,
            ActionAddress::new(
                &row.resource_id,
                if row.resource_kind == "browser.download" {
                    "show"
                } else {
                    "open"
                },
            ),
        )
        .with_subtitle(&row.url)
        .with_icon(row.icon)
        .with_score(row.score);
        let searchable = format!("{} {}", row.title, row.url);
        // Both values were constructed from a non-empty stable URL hash and
        // canonical action address directly above; an error would signal a
        // programming bug, not user data.  Refuse that row rather than
        // risking an opaque activation.
        if catalog.upsert(&row.resource_id, searchable, result).is_ok() {
            rows.insert(row.resource_id.clone(), row);
        }
    }

    pub fn provider(&self) -> BrowserSearchProvider {
        BrowserSearchProvider {
            library: LibraryCatalog {
                index: self.clone(),
            },
        }
    }
}

/// The library's search provider: history, bookmarks and downloads, with each
/// section declared as a scope the Library's search button narrows to.
pub struct BrowserSearchProvider {
    library: LibraryCatalog,
}

/// The catalogue answering every query; [`BrowserSearchProvider`] adds the
/// scopes a catalogue provider cannot declare itself.
struct LibraryCatalog {
    index: BrowserSearchIndex,
}

impl SearchProvider for BrowserSearchProvider {
    fn name(&self) -> &str {
        SearchProvider::name(&self.library)
    }

    fn action_directory_generation(&self) -> Option<u64> {
        self.library.action_directory_generation()
    }

    fn action_directory_cursor_is_current(&self, cursor: &SearchCursor) -> bool {
        self.library.action_directory_cursor_is_current(cursor)
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.library.search(query)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.library.search_limited(query, limit)
    }

    fn search_page(
        &self,
        query: &SearchQuery,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        self.library
            .search_page(query, cursor, work_budget, result_limit)
    }

    fn poll_page(&self, token: &PendingToken) -> PagePoll {
        self.library.poll_page(token)
    }

    fn facets(&self) -> Vec<Facet> {
        SearchProvider::facets(&self.library)
    }

    fn search_scopes(&self) -> Vec<ScopeDecl> {
        vec![
            ScopeDecl::new(crate::actions::HISTORY_SCOPE, "History"),
            ScopeDecl::new(crate::actions::BOOKMARKS_SCOPE, "Bookmarks"),
            ScopeDecl::new(crate::actions::DOWNLOADS_SCOPE, "Downloads"),
        ]
    }

    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<Resource> {
        self.library.resolve_action_resource(address)
    }
}

impl LibraryCatalog {
    fn row_resource(row: &Row) -> Resource {
        let mut resource = Resource::new(&row.resource_id, row.resource_kind, &row.title);
        let (action, label, payload, contract) = if row.resource_kind == "browser.download" {
            (
                "show",
                "Show download",
                serde_json::json!({ "operation": "show-library", "section": "downloads" }),
                ActionContract::view(),
            )
        } else {
            (
                "open",
                "Open page",
                serde_json::json!({ "operation": "navigate", "url": row.url }),
                navigation_contract(),
            )
        };
        if let Ok(spec) = ActionSpec::operation(
            action,
            label,
            OperationKind::Effect,
            "browser.operation",
            &payload,
        ) {
            resource.actions.push(spec.with_contract(contract));
        }
        resource
    }
}

/// This mirrors `BrowserWidget`'s navigation contract.  Search resolution can
/// happen without materialising the whole widget tree, so it owns a value
/// copy rather than a second, weaker action shape.
fn navigation_contract() -> ActionContract {
    ActionContract {
        retry: RetryPolicy::Idempotent,
        execution: ExecutionMode::Background { cancellable: false },
        result: ResultSpec::Json {
            schema: serde_json::json!({ "type": "string" }),
        },
        tags: vec!["network".into(), "navigation".into()],
        ..ActionContract::view()
    }
}

impl CatalogSearchProvider for LibraryCatalog {
    const RESULT_LIMIT: usize = DEFAULT_RESULT_LIMIT;

    fn provider_name(&self) -> &'static str {
        "Browser"
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(
            self.index
                .state
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .catalog
                .clone(),
        )
    }

    fn limited_work_budget(&self, limit: usize) -> usize {
        // A one-shot query inspects no more candidates than results it asked for.
        limit
    }

    fn resolve_action_address(&self, address: &ActionAddress) -> Option<Resource> {
        let row = self
            .index
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .rows
            .get(&address.resource)
            .cloned()?;
        let expected = if row.resource_kind == "browser.download" {
            "show"
        } else {
            "open"
        };
        (address.action == expected).then(|| Self::row_resource(&row))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchProvider;

    /// Cursor paging, stale addresses, identity and downloads.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn search_provider_pages_the_library_corpus_canonically() {
        // Phase `full_corpus_is_cursor_paged_and_canonical`.
        {
            let mut history = crate::history::History::new();
            for index in 0..6_000 {
                history.record_at(
                    &format!("https://{index}.example.test"),
                    &format!("Page {index}"),
                    index,
                );
            }
            let index = BrowserSearchIndex::default();
            index.sync(&history, &crate::library::Bookmarks::default());
            let provider = index.provider();
            let hits = provider.search_limited("Page", 12);
            assert_eq!(hits.len(), 12);
            let address = hits[0].action_address().unwrap();
            assert!(provider.resolve_action_resource(&address).is_some());

            let query = SearchQuery::new("Page");
            let first = provider.search_page(&query, None, 7, 3);
            assert!(first.inspected <= 7);
            assert!(first.results.len() <= 3);
            assert!(first.next.is_some());
            let second = provider.search_page(&query, first.next.as_ref(), 7, 3);
            assert!(second.inspected <= 7);
            assert!(second.results.len() <= 3);
        }

        // Phase `stale_search_addresses_are_refused_after_a_corpus_change`.
        {
            let mut bookmarks = crate::library::Bookmarks::default();
            assert!(bookmarks.add("https://stale.example.test", "Stale"));
            let index = BrowserSearchIndex::default();
            index.sync(&crate::history::History::new(), &bookmarks);
            let provider = index.provider();
            let hit = provider.search_limited("stale", 1).pop().unwrap();
            let address = hit.action_address().unwrap();
            assert!(bookmarks.remove("https://stale.example.test"));
            index.sync(&crate::history::History::new(), &bookmarks);
            assert!(provider.resolve_action_resource(&address).is_none());
        }

        // Phase `search_uses_the_library_resource_identity_and_contract`.
        {
            let mut bookmarks = crate::library::Bookmarks::default();
            // Legacy profile rows may omit the trailing slash.  The library uses
            // a normalized resource id, and search must resolve that exact same
            // object rather than a second raw-URL identity.
            assert!(bookmarks.add("https://canonical.example.test", "Canonical"));
            let index = BrowserSearchIndex::default();
            index.sync(&crate::history::History::new(), &bookmarks);
            let provider = index.provider();
            let hit = provider.search_limited("canonical", 1).pop().unwrap();
            let address = hit.action_address().unwrap();
            assert_eq!(
                address.resource,
                crate::actions::bookmark_resource_id("https://canonical.example.test")
            );
            let resource = provider.resolve_action_resource(&address).unwrap();
            assert_eq!(resource.kind, "browser.bookmark");
            assert!(matches!(
                resource.actions[0].contract.execution,
                ExecutionMode::Background { cancellable: false }
            ));
        }

        // Phase `library_sections_are_scopes_that_admit_only_their_own_hits`.
        {
            let mut history = crate::history::History::new();
            history.record_at("https://rust.example.test", "Rust notes", 1);
            let mut bookmarks = crate::library::Bookmarks::default();
            assert!(bookmarks.add("https://rust.example.test/book", "Rust book"));
            let index = BrowserSearchIndex::default();
            index.sync(&history, &bookmarks);
            let provider = index.provider();
            let scopes = provider.search_scopes();
            assert_eq!(
                scopes
                    .iter()
                    .map(|scope| scope.label.as_str())
                    .collect::<Vec<_>>(),
                ["History", "Bookmarks", "Downloads"]
            );
            let everywhere = provider.search_page(&SearchQuery::new("rust"), None, 50, 10);
            assert_eq!(everywhere.results.len(), 2);
            let only_bookmarks = SearchQuery::new("rust").within(crate::actions::BOOKMARKS_SCOPE);
            let narrowed = provider.search_page(&only_bookmarks, None, 50, 10);
            assert_eq!(narrowed.results.len(), 1);
            assert_eq!(narrowed.results[0].title, "Rust book");
        }

        // Phase `completed_downloads_are_searchable_by_name_source_and_destination`.
        {
            let mut downloads = crate::downloads::Downloads::default();
            assert!(downloads.requested(
                "stable-download".into(),
                "https://example.test/sdk".into(),
                "Tilo-SDK.tar.zst",
            ));
            downloads.finished(
                "stable-download",
                "/home/alex/Downloads/Tilo-SDK.tar.zst".into(),
            );
            let index = BrowserSearchIndex::default();
            index.sync_all(
                &crate::history::History::new(),
                &crate::library::Bookmarks::default(),
                &downloads,
            );
            let provider = index.provider();
            let hit = provider.search_limited("Tilo-SDK", 1).pop().unwrap();
            let address = hit.action_address().unwrap();
            assert_eq!(address.resource, "browser.downloads/stable-download");
            assert_eq!(address.action, "show");
            let resource = provider.resolve_action_resource(&address).unwrap();
            assert_eq!(resource.kind, "browser.download");
            assert_eq!(resource.actions[0].id, "show");
            assert!(!provider.search_limited("example.test/sdk", 1).is_empty());
            assert!(!provider.search_limited("home/alex/Downloads", 1).is_empty());
        }
    }
}
