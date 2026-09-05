//! Feeds as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: the reader declares its
//! searchable types by implementing [`SearchProvider`] over a snapshot of the
//! store, so both an **article** (by title / author / summary) and a
//! **subscription** (by title / URL) are findable from the unified Ctrl+K
//! palette. Every row names the corresponding resource's exported `open` View
//! action, so search, agents and accessibility use the same brokered route.
//!
//! The provider is deliberately **library-scoped and synchronous**: it matches
//! the already-fetched articles the store holds in memory, never triggering a
//! network fetch — a Ctrl+K query stays instant while live refreshes run on
//! their own worker.

use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_INDEX_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult,
};

use crate::model::{Article, Feed, Id, Store};

/// The group header for article results.
pub const ARTICLE_KIND: &str = "Articles";
/// The group header for feed (subscription) results.
pub const FEED_KIND: &str = "Feeds";

/// A [`SearchProvider`] over a snapshot of the store's feeds and articles.
pub struct FeedsSearchProvider {
    feeds: Vec<Feed>,
    articles: Vec<Article>,
}

impl FeedsSearchProvider {
    /// Snapshot the current store into a provider.
    pub fn new(store: &Store) -> Self {
        let feeds = store
            .feeds
            .iter()
            .take(DEFAULT_SEARCH_INDEX_LIMIT)
            .cloned()
            .collect::<Vec<_>>();
        let remaining = DEFAULT_SEARCH_INDEX_LIMIT.saturating_sub(feeds.len());
        Self {
            feeds,
            articles: store.articles.iter().take(remaining).cloned().collect(),
        }
    }

    /// The title of the feed an article belongs to, for its subtitle.
    fn feed_title(&self, feed_id: Id) -> Option<&str> {
        self.feeds
            .iter()
            .find(|f| f.id == feed_id)
            .map(|f| f.title.as_str())
    }
}

impl SearchProvider for FeedsSearchProvider {
    fn name(&self) -> &'static str {
        // Rows carry their own per-type kind ("Articles" / "Feeds"); this is
        // the default/diagnostic label.
        "Feeds"
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let q = query.to_lowercase();
        let limit = limit.min(DEFAULT_SEARCH_RESULT_LIMIT);
        if limit == 0 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(limit.min(32));

        for a in &self.articles {
            let matches = q.is_empty()
                || a.title.to_lowercase().contains(&q)
                || a.author.to_lowercase().contains(&q)
                || a.summary.to_lowercase().contains(&q);
            if !matches {
                continue;
            }
            let subtitle = match (self.feed_title(a.feed_id), a.author.is_empty()) {
                (Some(feed), false) => format!("{feed} · {}", a.author),
                (Some(feed), true) => feed.to_string(),
                (None, false) => a.author.clone(),
                (None, true) => String::new(),
            };
            let mut r = SearchResult::for_action(
                ARTICLE_KIND,
                a.title.clone(),
                crate::actions::open_article_address(a.id),
            )
            .with_icon(if a.starred {
                Icon::Star
            } else {
                Icon::FileText
            });
            if !subtitle.is_empty() {
                r = r.with_subtitle(subtitle);
            }
            out.push(r);
            if out.len() == limit {
                return out;
            }
        }

        for f in &self.feeds {
            let matches = q.is_empty()
                || f.title.to_lowercase().contains(&q)
                || f.url.to_lowercase().contains(&q)
                || f.site_url.to_lowercase().contains(&q);
            if !matches {
                continue;
            }
            let mut r = SearchResult::for_action(
                FEED_KIND,
                f.title.clone(),
                crate::actions::open_feed_address(f.id),
            )
            .with_icon(Icon::List);
            let site = if f.site_url.is_empty() {
                &f.url
            } else {
                &f.site_url
            };
            if !site.is_empty() {
                r = r.with_subtitle(site.clone());
            }
            out.push(r);
            if out.len() == limit {
                break;
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchRegistry;

    fn seeded() -> Store {
        crate::sample::store()
    }

    #[test]
    fn provider_finds_articles_and_feeds() {
        let store = seeded();
        let provider = FeedsSearchProvider::new(&store);

        let some_article = store.articles.first().expect("sample has articles").clone();
        let word = some_article.title.split_whitespace().next().unwrap();
        let hits = provider.search(word);
        assert!(
            hits.iter().any(|r| r.kind == ARTICLE_KIND),
            "a real article-title word surfaces an article row"
        );

        let some_feed = store.feeds.first().expect("sample has feeds").clone();
        let fword = some_feed.title.split_whitespace().next().unwrap();
        let fhits = provider.search(fword);
        assert!(
            fhits.iter().any(|r| r.kind == FEED_KIND),
            "a feed-title word surfaces a feed row"
        );
    }

    #[test]
    fn empty_query_browses_every_article_and_feed() {
        let store = seeded();
        let provider = FeedsSearchProvider::new(&store);
        let hits = provider.search("");
        assert_eq!(hits.len(), store.articles.len() + store.feeds.len());
    }

    #[test]
    fn every_hit_addresses_an_exported_view_action() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::app::FeedsApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let resources = app.resources(&doc);
        let mut registry = SearchRegistry::new();
        for provider in app.search_providers() {
            registry.register(provider);
        }
        let hits = registry.query("");
        assert_eq!(
            hits.len(),
            app.store_article_ids().len() + app.store_feed_ids().len()
        );
        assert!(hits.iter().all(|hit| {
            hit.action_address().is_some_and(|address| {
                qos_actions::door::resolve(&resources, &address)
                    .is_some_and(|spec| spec.kind == qos_actions::OperationKind::View)
            })
        }));
    }

    #[test]
    fn brokered_search_opens_article_without_legacy_navigation_or_doc_write() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::app::FeedsApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);

        let target = app.store_article_ids()[0];
        let providers = app.search_providers();
        let target_title = providers[0]
            .search("")
            .into_iter()
            .find(|hit| hit.action_address() == Some(crate::actions::open_article_address(target)))
            .expect("the target article is searchable")
            .title;
        let resources = app.resources(&doc);
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.set_providers(providers);
        palette.open();
        palette.set_query(&target_title);
        let hit_index = palette
            .result_labels()
            .iter()
            .position(|label| label == &target_title)
            .expect("the article is browsable through the real hook");
        palette.set_cursor(hit_index);
        let before = doc.get_heads();
        palette.activate_selected();
        let operation = palette
            .take_operation()
            .expect("canonical result queued a broker operation");
        assert_eq!(operation.kind, qos_actions::OperationKind::View);
        assert!(palette.take_navigation().is_none());
        assert!(
            app.invoke_resource_operation(
                operation.kind,
                &operation.encoded.type_id,
                &operation.encoded.payload,
                &doc,
                &ROOT,
            )
            .expect("Feeds handles its exported view operation")
        );
        assert_eq!(
            app.selected_article(),
            Some(target),
            "activation opened the chosen article"
        );
        assert!(
            matches!(app.active_filter(), crate::model::Filter::Feed(_)),
            "activation focused the article's feed so the row is visible"
        );
        assert_eq!(
            doc.get_heads(),
            before,
            "View activation stays presentation-only"
        );
    }
}
