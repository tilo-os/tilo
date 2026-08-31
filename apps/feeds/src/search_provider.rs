//! Feeds as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: the reader declares its
//! searchable types by implementing [`SearchProvider`] over a snapshot of the
//! store, so both an **article** (by title / author / summary) and a
//! **subscription** (by title / URL) are findable from the unified Ctrl+K
//! palette. Activating a row hands back an opaque `action_id` the app resolves
//! to the article or feed and navigates to.
//!
//! The provider is deliberately **library-scoped and synchronous**: it matches
//! the already-fetched articles the store holds in memory, never triggering a
//! network fetch — a Ctrl+K query stays instant while live refreshes run on
//! their own worker.

use ui::icons::Icon;
use ui::search::{SearchProvider, SearchResult};

use crate::model::{Article, Feed, Id, Store};

/// The group header for article results.
pub const ARTICLE_KIND: &str = "Articles";
/// The group header for feed (subscription) results.
pub const FEED_KIND: &str = "Feeds";

const ARTICLE_PREFIX: &str = "feeds.article:";
const FEED_PREFIX: &str = "feeds.feed:";

/// What an activated row points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activation {
    /// Open an article by id.
    Article(Id),
    /// Focus a feed (subscription) by id.
    Feed(Id),
}

impl Activation {
    /// Resolve an `action_id` this provider emitted back into a target.
    pub fn parse(action_id: &str) -> Option<Self> {
        if let Some(rest) = action_id.strip_prefix(ARTICLE_PREFIX) {
            rest.parse::<Id>().ok().map(Activation::Article)
        } else if let Some(rest) = action_id.strip_prefix(FEED_PREFIX) {
            rest.parse::<Id>().ok().map(Activation::Feed)
        } else {
            None
        }
    }
}

/// A [`SearchProvider`] over a snapshot of the store's feeds and articles.
pub struct FeedsSearchProvider {
    feeds: Vec<Feed>,
    articles: Vec<Article>,
}

impl FeedsSearchProvider {
    /// Snapshot the current store into a provider.
    pub fn new(store: &Store) -> Self {
        Self {
            feeds: store.feeds.clone(),
            articles: store.articles.clone(),
        }
    }

    /// The `action_id` for an article row.
    pub fn article_action_id(id: Id) -> String {
        format!("{ARTICLE_PREFIX}{id}")
    }

    /// The `action_id` for a feed row.
    pub fn feed_action_id(id: Id) -> String {
        format!("{FEED_PREFIX}{id}")
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
        let q = query.to_lowercase();
        let mut out = Vec::new();

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
            let mut r =
                SearchResult::new(ARTICLE_KIND, a.title.clone(), Self::article_action_id(a.id))
                    .with_icon(if a.starred {
                        Icon::Star
                    } else {
                        Icon::FileText
                    });
            if !subtitle.is_empty() {
                r = r.with_subtitle(subtitle);
            }
            out.push(r);
        }

        for f in &self.feeds {
            let matches = q.is_empty()
                || f.title.to_lowercase().contains(&q)
                || f.url.to_lowercase().contains(&q)
                || f.site_url.to_lowercase().contains(&q);
            if !matches {
                continue;
            }
            let mut r = SearchResult::new(FEED_KIND, f.title.clone(), Self::feed_action_id(f.id))
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
    fn action_ids_round_trip_to_the_right_targets() {
        let store = seeded();
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(FeedsSearchProvider::new(&store)));

        let a = store.articles.first().unwrap().clone();
        let hit = registry
            .query("")
            .into_iter()
            .find(|r| r.action_id == FeedsSearchProvider::article_action_id(a.id))
            .expect("the article is browsable through the registry");
        assert_eq!(
            Activation::parse(&hit.action_id),
            Some(Activation::Article(a.id))
        );
        assert!(
            store.article(a.id).is_some(),
            "the id resolves to a real stored article"
        );

        let f = store.feeds.first().unwrap().clone();
        assert_eq!(
            Activation::parse(&FeedsSearchProvider::feed_action_id(f.id)),
            Some(Activation::Feed(f.id))
        );
    }

    #[test]
    fn parse_rejects_foreign_ids() {
        assert!(Activation::parse("file:/etc/passwd").is_none());
        assert!(Activation::parse("feeds.article:not-a-uuid").is_none());
        assert!(Activation::parse("feeds.feed:not-a-uuid").is_none());
    }

    #[test]
    fn widget_hooks_find_then_open_the_article() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::app::FeedsApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);

        // Browse the app's OWN provider (its real ids) and take an article hit.
        let (action_id, id) = {
            let providers = app.search_providers();
            assert!(!providers.is_empty(), "feeds publishes a search provider");
            let hit = providers[0]
                .search("")
                .into_iter()
                .find(|r| r.kind == ARTICLE_KIND)
                .expect("an article is browsable through the real hook");
            let Some(Activation::Article(id)) = Activation::parse(&hit.action_id) else {
                panic!("article row carries an article action_id");
            };
            (hit.action_id, id)
        };

        app.activate_search_result(&action_id, &mut doc, &ROOT);
        assert_eq!(
            app.selected_article(),
            Some(id),
            "activation opened the chosen article"
        );
        assert!(
            matches!(app.active_filter(), crate::model::Filter::Feed(_)),
            "activation focused the article's feed so the row is visible"
        );
    }
}
