//! The person's browsing history and the device-local downloads as the
//! search entries the browser surface contributes to the app's one search
//! source. Bookmarks are the browser resource's own search entries.
//!
//! The entries are rebuilt only after a durable corpus revision, so opening
//! global search does not remap the full browser library. A hit's row is
//! resolved against the current entries, which refuses an address that went
//! stale after the result was rendered.

use tilo_actions::Resource;
use ui::icons::Icon;
use ui::lock::RwLockRecover as _;
use ui::tilo_resource::SearchEntry;

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
    entries: Vec<SearchEntry>,
    rows: std::collections::BTreeMap<String, Row>,
}

impl Default for IndexState {
    fn default() -> Self {
        Self {
            history_revision: u64::MAX,
            download_revision: u64::MAX,
            entries: Vec::new(),
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
        let mut state = self.state.write_recover();
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
        let current = self.state.read_recover();
        let unchanged = current.history_revision == history_revision
            && current.download_revision == download_revision;
        drop(current);
        if unchanged {
            return;
        }

        let mut entries = Vec::new();
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
            Self::index_row(&mut entries, &mut rows, row);
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
            Self::index_row(&mut entries, &mut rows, row);
        }

        let mut state = self.state.write_recover();
        state.history_revision = history_revision;
        state.download_revision = download_revision;
        state.entries = entries;
        state.rows = rows;
    }

    fn index_row(
        entries: &mut Vec<SearchEntry>,
        rows: &mut std::collections::BTreeMap<String, Row>,
        row: Row,
    ) {
        if rows.contains_key(&row.resource_id) {
            return;
        }
        entries.push(
            SearchEntry::new(row.title.clone(), String::new())
                .anchored(&row.resource_id)
                .grouped(row.search_kind)
                .subtitled(row.url.clone())
                .with_icon(row.icon.name())
                .boosted(row.score),
        );
        rows.insert(row.resource_id.clone(), row);
    }

    /// Every history page and download as a search entry.
    pub fn entries(&self) -> Vec<SearchEntry> {
        self.state.read_recover().entries.clone()
    }

    /// What opening the hit anchored at `id` runs: the history page opens,
    /// the download shows. `None` once the entry is gone.
    pub fn opened(&self, id: &str) -> Option<crate::effects::BrowserEffect> {
        let row = self.state.read_recover().rows.get(id).cloned()?;
        Some(match &row.download {
            Some(id) => crate::effects::BrowserEffect::ShowDownload { id: id.clone() },
            None => crate::effects::BrowserEffect::Open {
                url: row.url.clone(),
            },
        })
    }

    /// The row a hit opens: the same row, with the same effect, the window
    /// publishes for it. `None` once the entry is gone.
    pub fn resource(&self, id: &str) -> Option<Resource> {
        use ui::tilo_resource::Effect as _;
        let action = self.opened(id)?.bound();
        let row = self.state.read_recover().rows.get(id).cloned()?;
        Some(Resource::new(&row.resource_id, row.resource_kind, &row.title).with_action(action))
    }

    /// The search source the framework builds over these entries, resolving
    /// hits the way the browser surface does.
    #[cfg(test)]
    pub fn provider(&self) -> TestSearch {
        TestSearch {
            source: Box::new(ui::framework::testing::search_source::<
                crate::resource::Browser,
            >(self.entries())),
        }
    }
}

/// See [`BrowserSearchIndex::provider`].
#[cfg(test)]
pub struct TestSearch {
    source: Box<dyn ui::search::SearchProvider>,
}

#[cfg(test)]
impl ui::search::SearchProvider for TestSearch {
    fn name(&self) -> &str {
        self.source.name()
    }

    fn search(&self, query: &str) -> Vec<ui::search::SearchResult> {
        self.source.search(query)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<ui::search::SearchResult> {
        self.source.search_limited(query, limit)
    }

    fn search_page(
        &self,
        query: &ui::search::SearchQuery,
        cursor: Option<&ui::search::SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> ui::search::SearchPage {
        self.source
            .search_page(query, cursor, work_budget, result_limit)
    }

    fn search_scopes(&self) -> Vec<ui::search::ScopeDecl> {
        vec![
            ui::search::ScopeDecl::new(
                crate::actions::HISTORY_SCOPE,
                crate::resource::CATALOG.t("browser-library-section-history"),
            ),
            ui::search::ScopeDecl::new(
                crate::actions::BOOKMARKS_SCOPE,
                crate::resource::CATALOG.t("browser-library-section-bookmarks"),
            ),
            ui::search::ScopeDecl::new(
                crate::actions::DOWNLOADS_SCOPE,
                crate::resource::CATALOG.t("browser-library-section-downloads"),
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchProvider;
    use ui::search::SearchQuery;

    /// Cursor paging, stale addresses, identity and downloads.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn search_provider_pages_the_library_corpus_canonically() {
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
            assert!(index.opened(&hits[0].action_id).is_some());

            let query = SearchQuery::new("Page");
            let first = provider.search_page(&query, None, 7, 3);
            assert!(first.inspected <= 7);
            assert!(first.results.len() <= 3);
            assert!(first.next.is_some());
            let second = provider.search_page(&query, first.next.as_ref(), 7, 3);
            assert!(second.inspected <= 7);
            assert!(second.results.len() <= 3);
        }

        {
            let mut history = crate::history::History::new();
            history.record_at("https://stale.example.test", "Stale", 1);
            let index = BrowserSearchIndex::default();
            index.sync(&history);
            let provider = index.provider();
            let hit = provider.search_limited("stale", 1).pop().unwrap();
            assert_eq!(
                hit.action_id,
                crate::actions::history_resource_id("https://stale.example.test")
            );
            history.clear();
            index.sync(&history);
            assert!(index.opened(&hit.action_id).is_none());
        }

        {
            use ui::tilo_resource::Resource as _;

            let mut history = crate::history::History::new();
            history.record_at("https://rust.example.test", "Rust notes", 1);
            let mut document = ui::tilo_resource::Replica::<crate::resource::Browser>::new();
            document
                .apply(crate::resource::Browser::ADD_BOOKMARK.with(
                    crate::resource::browser::AddBookmark {
                        url: "https://rust.example.test/book".into(),
                        title: Some("Rust book".into()),
                    },
                ))
                .unwrap();
            let state = document.state().unwrap();
            let index = BrowserSearchIndex::default();
            index.sync(&history);
            let mut entries = index.entries();
            entries.extend(crate::resource::Browser::search(&state));
            let mut provider = index.provider();
            provider.source = Box::new(ui::framework::testing::search_source::<
                crate::resource::Browser,
            >(entries));
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
            assert_eq!(
                hit.action_id,
                format!("{}/stable-download", crate::actions::DOWNLOADS_SCOPE)
            );
            assert_eq!(
                index.opened(&hit.action_id),
                Some(crate::effects::BrowserEffect::ShowDownload {
                    id: "stable-download".into()
                })
            );
            let resource = index.resource(&hit.action_id).unwrap();
            assert_eq!(resource.kind, "browser.download");
            assert_eq!(resource.actions[0].id, "show-download");
            assert!(!provider.search_limited("example.test/sdk", 1).is_empty());
            assert!(!provider.search_limited("home/alex/Downloads", 1).is_empty());
        }
    }
}
