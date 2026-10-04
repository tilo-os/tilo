//! Bounded, cursor-paged global search for the person's browsing history and
//! device-local download resources. Bookmarks are the browser resource's own
//! search entries.
//!
//! The retained index is refreshed only after a durable corpus revision.
//! Providers then clone it cheaply, so opening global search does not clone
//! or remap the full browser library.  A provider resolves action resources
//! against the current index, which refuses an address that went stale after
//! the result was rendered.

use tilo_actions::{ActionAddress, Resource};
use ui::icons::Icon;
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchResult};

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
    /// The download a hit shows; `None` for a page it opens.
    download: Option<String>,
    icon: Icon,
    score: i32,
}

struct IndexState {
    history_revision: u64,
    download_revision: u64,
    catalog: SearchCatalog,
    rows: std::collections::BTreeMap<String, Row>,
}

impl Default for IndexState {
    fn default() -> Self {
        Self {
            history_revision: u64::MAX,
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
        state.download_revision = u64::MAX;
    }

    pub fn sync(&self, history: &crate::history::History) {
        self.sync_all(history, &crate::downloads::Downloads::default());
    }

    pub fn sync_all(
        &self,
        history: &crate::history::History,
        downloads: &crate::downloads::Downloads,
    ) {
        let history_revision = history.revision();
        let download_revision = downloads.revision();
        let current = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let unchanged = current.history_revision == history_revision
            && current.download_revision == download_revision;
        drop(current);
        if unchanged {
            return;
        }

        let catalog = SearchCatalog::new("Browser");
        let mut rows = std::collections::BTreeMap::new();
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
                download: None,
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
                download: Some(entry.id.clone()),
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
                if row.download.is_some() {
                    "show-download"
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
            index: self.clone(),
        }
    }
}

pub struct BrowserSearchProvider {
    index: BrowserSearchIndex,
}

impl BrowserSearchProvider {
    /// The row a hit opens: the same row, with the same effect, the window
    /// publishes for it.
    fn row_resource(row: &Row) -> Resource {
        use ui::tilo_resource::Effect as _;
        let action = match &row.download {
            Some(id) => crate::effects::BrowserEffect::ShowDownload { id: id.clone() }.bound(),
            None => crate::effects::BrowserEffect::Open {
                url: row.url.clone(),
            }
            .bound(),
        };
        Resource::new(&row.resource_id, row.resource_kind, &row.title).with_action(action)
    }
}

impl CatalogSearchProvider for BrowserSearchProvider {
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
        let expected = if row.download.is_some() {
            "show-download"
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
            index.sync(&history);
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

        // Phase `stale_search_addresses_are_refused_after_a_corpus_change`.
        {
            let mut history = crate::history::History::new();
            history.record_at("https://stale.example.test", "Stale", 1);
            let index = BrowserSearchIndex::default();
            index.sync(&history);
            let provider = index.provider();
            let hit = provider.search_limited("stale", 1).pop().unwrap();
            let address = hit.action_address().unwrap();
            assert_eq!(
                address.resource,
                crate::actions::history_resource_id("https://stale.example.test")
            );
            history.clear();
            index.sync(&history);
            assert!(provider.resolve_action_resource(&address).is_none());
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
            index.sync_all(&crate::history::History::new(), &downloads);
            let provider = index.provider();
            let hit = provider.search_limited("Tilo-SDK", 1).pop().unwrap();
            let address = hit.action_address().unwrap();
            assert_eq!(address.resource, "browser.download.stable-download");
            assert_eq!(address.action, "show-download");
            let resource = provider.resolve_action_resource(&address).unwrap();
            assert_eq!(resource.kind, "browser.download");
            assert_eq!(resource.actions[0].id, "show-download");
            assert!(!provider.search_limited("example.test/sdk", 1).is_empty());
            assert!(!provider.search_limited("home/alex/Downloads", 1).is_empty());
        }
    }
}
