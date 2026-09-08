//! Bounded, cursor-paged global search for browser history and bookmarks.
//!
//! The retained index is refreshed only after a durable corpus revision.
//! Providers then clone it cheaply, so opening global search does not clone
//! or remap the full browser library.  A provider resolves action resources
//! against the current index, which refuses an address that went stale after
//! the result was rendered.

use qos_actions::{
    ActionAddress, ActionContract, ActionSpec, ExecutionMode, OperationKind, Resource, ResultSpec,
    RetryPolicy,
};
use ui::icons::Icon;
use ui::search::{SearchCatalog, SearchPage, SearchProvider, SearchResult};

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
    catalog: SearchCatalog,
    rows: std::collections::BTreeMap<String, Row>,
}

impl Default for IndexState {
    fn default() -> Self {
        Self {
            history_revision: u64::MAX,
            bookmark_revision: u64::MAX,
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
    }

    pub fn sync(&self, history: &crate::history::History, bookmarks: &crate::library::Bookmarks) {
        let history_revision = history.revision();
        let bookmark_revision = bookmarks.revision();
        let current = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let unchanged = current.history_revision == history_revision
            && current.bookmark_revision == bookmark_revision;
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

        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.history_revision = history_revision;
        state.bookmark_revision = bookmark_revision;
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
            ActionAddress::new(&row.resource_id, "open"),
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
            index: self.clone(),
        }
    }
}

pub struct BrowserSearchProvider {
    index: BrowserSearchIndex,
}

impl BrowserSearchProvider {
    fn row_resource(row: &Row) -> Resource {
        let mut resource = Resource::new(&row.resource_id, row.resource_kind, &row.title);
        let payload = serde_json::json!({ "operation": "navigate", "url": row.url });
        if let Ok(spec) = ActionSpec::operation(
            "open",
            "Open page",
            OperationKind::Effect,
            "browser.operation",
            &payload,
        ) {
            resource
                .actions
                .push(spec.with_contract(navigation_contract()));
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

impl SearchProvider for BrowserSearchProvider {
    #[allow(
        clippy::unnecessary_literal_bound,
        reason = "The shared SearchProvider trait requires a borrowed literal name."
    )]
    fn name(&self) -> &str {
        "Browser"
    }

    fn action_directory_generation(&self) -> Option<u64> {
        Some(
            self.index
                .state
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .catalog
                .stats()
                .generation,
        )
    }

    fn action_directory_cursor_is_current(&self, cursor: &ui::search::SearchCursor) -> bool {
        self.index
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .cursor_is_current(cursor)
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, 100)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.search_page(query, None, limit, limit.min(DEFAULT_RESULT_LIMIT))
            .results
    }

    fn search_page(
        &self,
        query: &str,
        cursor: Option<&ui::search::SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        let catalog = self
            .index
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .clone();
        catalog.query_page(query, cursor, work_budget, result_limit)
    }

    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<Resource> {
        if address.action != "open" {
            return None;
        }
        self.index
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .rows
            .get(&address.resource)
            .map(Self::row_resource)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_corpus_is_cursor_paged_and_canonical() {
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

        let first = provider.search_page("Page", None, 7, 3);
        assert!(first.inspected <= 7);
        assert!(first.results.len() <= 3);
        assert!(first.next.is_some());
        let second = provider.search_page("Page", first.next.as_ref(), 7, 3);
        assert!(second.inspected <= 7);
        assert!(second.results.len() <= 3);
    }

    #[test]
    fn stale_search_addresses_are_refused_after_a_corpus_change() {
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

    #[test]
    fn search_uses_the_library_resource_identity_and_contract() {
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
}
