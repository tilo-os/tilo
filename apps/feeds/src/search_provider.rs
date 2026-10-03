//! Feeds as a Tilo global-search source.
//!
//! The reader indexes every durable subscription and cached article in Tilo'
//! shared [`SearchCatalog`]. It is intentionally a catalogue rather than a
//! viewport or legacy capped snapshot: a newly synced article stays discoverable
//! even when a library has hundreds of thousands of entries. Queries are paged
//! and candidate-budgeted, so Ctrl+K never has to inspect or materialise the
//! whole reader library in one frame. Rows only name the canonical `open` View
//! actions exported by [`crate::actions`]; the current resource tree remains the
//! authority at activation time and refuses stale rows after deletion.

use ui::icons::Icon;
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchResult};

use crate::model::{Article, Feed, Store};

/// The group header for article results.
pub const ARTICLE_KIND: &str = "Articles";
/// The group header for feed (subscription) results.
pub const FEED_KIND: &str = "Feeds";
pub const TAG_KIND: &str = "Tags";

/// A complete, bounded-query search catalogue over the reader's durable store.
///
/// Cloning this provider for Ctrl+K is cheap; `SearchCatalog` itself is an
/// `Arc` handle. Building a provider deliberately indexes the complete store,
/// never a visual list or `DEFAULT_SEARCH_INDEX_LIMIT` prefix.
#[derive(Clone)]
pub struct FeedsSearchProvider {
    catalog: SearchCatalog,
}

impl FeedsSearchProvider {
    /// Build a complete catalogue from the current document-backed store.
    pub fn new(store: &Store) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new("Feeds"),
        };
        for feed in &store.feeds {
            provider.index_feed(feed);
        }
        for article in &store.articles {
            // An orphaned cache row is not a live reader resource: it cannot
            // be opened through the current resource tree, so never give it a
            // canonical search address in the first place.
            if let Some(feed) = store.feed(article.feed_id) {
                provider.index_article(article, feed);
            }
        }
        for tag in store.tags() {
            provider.index_tag(&tag);
        }
        provider
    }

    fn index_feed(&self, feed: &Feed) {
        let mut result = SearchResult::for_action(
            FEED_KIND,
            feed_label(feed),
            crate::actions::open_feed_address(feed.id),
        )
        .with_icon(Icon::List);
        let site = if feed.site_url.trim().is_empty() {
            &feed.url
        } else {
            &feed.site_url
        };
        if !site.trim().is_empty() {
            result = result.with_subtitle(site.clone());
        }
        let Some(address) = result.action_address() else {
            return;
        };
        // `for_action` above uses this exact stable resource id. The shared
        // catalogue defensively validates that invariant at this boundary.
        let _ = self.catalog.upsert(
            address.resource,
            format!("{}\0{}\0{}", feed.title, feed.url, feed.site_url),
            result,
        );
    }

    fn index_article(&self, article: &Article, feed: &Feed) {
        let mut result = SearchResult::for_action(
            ARTICLE_KIND,
            article_label(article),
            crate::actions::open_article_address(article.id),
        )
        .with_icon(if article.starred {
            Icon::Star
        } else {
            Icon::FileText
        });
        let subtitle = article_subtitle(article, feed);
        if !subtitle.is_empty() {
            result = result.with_subtitle(subtitle);
        }
        let Some(address) = result.action_address() else {
            return;
        };
        // Index only material the reader can render locally; this search path
        // never fetches a remote URL or parses feed content on demand.
        let _ = self.catalog.upsert(
            address.resource,
            format!(
                "{}\0{}\0{}\0{}\0{}",
                article.title,
                article.author,
                article.summary,
                feed.title,
                article.tags.join("\0")
            ),
            result,
        );
    }

    fn index_tag(&self, tag: &str) {
        let result = SearchResult::for_action(
            TAG_KIND,
            format!("#{tag}"),
            tilo_actions::ActionAddress::new(crate::actions::tag_resource_id(tag), "open"),
        )
        .with_icon(Icon::Pin)
        .with_subtitle("Tagged articles");
        let Some(address) = result.action_address() else {
            return;
        };
        let _ = self.catalog.upsert(address.resource, tag, result);
    }

    #[cfg(test)]
    fn stats(&self) -> ui::search::SearchCatalogStats {
        self.catalog.stats()
    }
}

fn feed_label(feed: &Feed) -> String {
    let title = feed.title.trim();
    if title.is_empty() {
        "Untitled feed".to_string()
    } else {
        title.to_string()
    }
}

fn article_label(article: &Article) -> String {
    let title = article.title.trim();
    if title.is_empty() {
        "Untitled article".to_string()
    } else {
        title.to_string()
    }
}

fn article_subtitle(article: &Article, feed: &Feed) -> String {
    match (!feed.title.trim().is_empty(), article.author.trim()) {
        (true, author) if !author.is_empty() => format!("{} · {author}", feed.title.trim()),
        (true, _) => feed.title.trim().to_string(),
        (false, author) => author.to_string(),
    }
}

impl CatalogSearchProvider for FeedsSearchProvider {
    fn provider_name(&self) -> &'static str {
        "Feeds"
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{AutoCommit, ROOT};
    use ui::search::SearchProvider;
    use ui::search::testing::{LATE_HIT_NEEDLE, assert_late_hit_reachable};
    use ui::widget::Widget;
    use uuid::Uuid;

    fn seeded() -> Store {
        crate::sample::store()
    }

    fn test_feed(id: u128) -> Feed {
        Feed {
            id: Uuid::from_u128(id),
            title: "Search test feed".into(),
            url: "https://example.test/feed.xml".into(),
            site_url: "https://example.test".into(),
            folder: None,
            order: 0,
            kind: crate::model::FeedKind::Rss,
        }
    }

    fn test_article(id: u128, feed_id: Uuid, title: String) -> Article {
        Article {
            id: Uuid::from_u128(id),
            feed_id,
            title,
            author: String::new(),
            url: String::new(),
            published: 0,
            summary: String::new(),
            blocks: Vec::new(),
            read: false,
            starred: false,
            tags: Vec::new(),
        }
    }

    /// Search finds articles and feeds, browses on empty query, casefolds unicode and bounds paged work in large archives.
    #[test]
    fn search_finds_articles_and_feeds_in_large_archives() {
        // --- phase: provider_finds_articles_and_feeds ---
        {
            let store = seeded();
            let provider = FeedsSearchProvider::new(&store);

            let some_article = store.articles.first().expect("sample has articles");
            let word = some_article.title.split_whitespace().next().unwrap();
            let hits = provider.search(word);
            assert!(
                hits.iter().any(|r| r.kind == ARTICLE_KIND),
                "a real article-title word surfaces an article row"
            );

            let some_feed = store.feeds.first().expect("sample has feeds");
            let fword = some_feed.title.split_whitespace().next().unwrap();
            let fhits = provider.search(fword);
            assert!(
                fhits.iter().any(|r| r.kind == FEED_KIND),
                "a feed-title word surfaces a feed row"
            );
        }

        // --- phase: empty_query_browses_every_article_and_feed_within_the_requested_limit ---
        {
            let store = seeded();
            let provider = FeedsSearchProvider::new(&store);
            let total = store.articles.len() + store.feeds.len() + store.tags().len();
            assert_eq!(provider.search_limited("", total).len(), total);
        }

        // --- phase: catalog_preserves_unicode_casefolded_substrings ---
        {
            let feed = test_feed(1);
            let store = Store {
                feeds: vec![feed.clone()],
                articles: vec![test_article(2, feed.id, "Über den Fluss".into())],
                ..Store::default()
            };
            let page = FeedsSearchProvider::new(&store).search_page("DEN FL", None, 8, 8);
            assert_eq!(page.results.len(), 1);
            assert_eq!(page.results[0].title, "Über den Fluss");
            assert!(page.inspected <= 8);
        }

        // --- phase: unique_late_hit_remains_findable_in_a_large_archive ---
        {
            let (provider, hit) = assert_late_hit_reachable(
                |corpus| {
                    let feed = test_feed(1);
                    let articles = corpus
                        .map(|row| test_article(row.index as u128 + 2, feed.id, row.text))
                        .collect::<Vec<_>>();
                    FeedsSearchProvider::new(&Store {
                        feeds: vec![feed],
                        articles,
                        ..Store::default()
                    })
                },
                3_000,
            );
            assert_eq!(hit.title, format!("ordinary record 2999 {LATE_HIT_NEEDLE}"));
            let page = provider.search_page(LATE_HIT_NEEDLE, None, 4, 4);
            assert_eq!(page.inspected, 1, "rare posting avoids a corpus scan");
            assert_eq!(provider.stats().records, 3_001);
        }

        // --- phase: paged_search_bounds_work_and_materialisation_without_losing_broad_hits ---
        {
            let feed = test_feed(1);
            let articles = (0..257u128)
                .map(|index| test_article(index + 2, feed.id, "Common article".into()))
                .collect();
            let provider = FeedsSearchProvider::new(&Store {
                feeds: vec![feed],
                articles,
                ..Store::default()
            });
            let mut cursor = None;
            let mut seen = 0;
            loop {
                let page = provider.search_page("common", cursor.as_ref(), 17, 5);
                assert!(page.inspected <= 17, "query work must be bounded");
                assert!(page.results.len() <= 5, "result materialisation is paged");
                seen += page.results.len();
                let Some(next) = page.next else {
                    break;
                };
                cursor = Some(next);
            }
            assert_eq!(seen, 257);
        }
    }

    /// Every hit addresses an exported view action; stale resources are refused and brokered opens avoid legacy navigation.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn search_hits_address_current_exported_actions() {
        // --- phase: every_hit_addresses_an_exported_view_action ---
        {
            let mut app = crate::app::FeedsApp::new_demo();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &ROOT);
            let resources = ui::resources::resources(&app, &doc);
            let tag_count = crate::sample::store().tags().len();
            let total = app.store_article_ids().len() + app.store_feed_ids().len() + tag_count;
            let hits = ui::search::capability::search_providers(&app)[0].search_limited("", total);
            assert!(hits.iter().all(|hit| {
                hit.action_address().is_some_and(|address| {
                    tilo_actions::door::resolve(&resources, &address)
                        .is_some_and(|spec| spec.kind == tilo_actions::OperationKind::View)
                        || ui::resources::resource(&app, &doc, address.resource.as_str())
                            .is_some_and(|resource| {
                                tilo_actions::door::resolve(&[resource], &address).is_some_and(
                                    |spec| spec.kind == tilo_actions::OperationKind::View,
                                )
                            })
                })
            }));
        }

        // --- phase: stale_search_resource_is_refused_by_the_current_canonical_tree ---
        {
            let mut app = crate::app::FeedsApp::new_demo();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &ROOT);
            let resources = ui::resources::resources(&app, &doc);
            let feed_resource = resources[0]
                .children
                .iter()
                .find(|resource| {
                    resource.kind == "feeds.subscription" && !resource.children.is_empty()
                })
                .expect("sample has a feed with an article");
            let feed = feed_resource
                .id
                .as_str()
                .strip_prefix("feeds.subscription:")
                .expect("feed resource id")
                .parse()
                .expect("feed id is a UUID");
            let article = feed_resource.children[0]
                .id
                .as_str()
                .strip_prefix("feeds.article:")
                .expect("article resource id")
                .parse()
                .expect("article id is a UUID");
            let provider = ui::search::capability::search_providers(&app)
                .pop()
                .expect("Feeds exposes its article provider");
            let hit = provider
                .search("")
                .into_iter()
                .find(|hit| {
                    hit.action_address() == Some(crate::actions::open_article_address(article))
                })
                .expect("article is searchable before removal");

            crate::doc::FeedsDoc::remove_feed(&mut doc, feed);
            app.sync(&mut doc, &ROOT);
            let mut palette = ui::ActionPalette::new();
            palette.load_open_resources(&ui::resources::resources(&app, &doc));
            palette.set_providers(vec![provider]);
            palette.open();
            palette.set_query(&hit.title);
            let row = palette
                .result_labels()
                .iter()
                .position(|label| label == &hit.title)
                .expect("stale row may be displayed but cannot gain authority");
            palette.set_cursor(row);
            palette.activate_selected();
            assert!(
                palette.take_operation().is_none(),
                "a deleted resource must be refused rather than navigating via a stale search row"
            );
        }

        // --- phase: brokered_search_opens_article_without_legacy_navigation_or_doc_write ---
        {
            let mut app = crate::app::FeedsApp::new_demo();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &ROOT);

            let target = app.store_article_ids()[0];
            let providers = ui::search::capability::search_providers(&app);
            let target_title = providers[0]
                .search("")
                .into_iter()
                .find(|hit| {
                    hit.action_address() == Some(crate::actions::open_article_address(target))
                })
                .expect("the target article is searchable")
                .title;
            let resources = ui::resources::resources(&app, &doc);
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
            assert_eq!(operation.kind, tilo_actions::OperationKind::View);
            assert!(palette.take_navigation().is_none());
            assert!(matches!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        operation.kind,
                        &operation
                            .encoded()
                            .expect("test requires prepared encoding")
                            .type_id,
                        &operation
                            .encoded()
                            .expect("test requires prepared encoding")
                            .payload
                    ),
                    &mut doc,
                    &ROOT
                )
                .expect("Feeds handles its exported view operation"),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
            assert_eq!(app.selected_article(), Some(target));
            assert!(
                matches!(app.active_filter(), crate::model::Filter::Feed(_)),
                "activation focused the article's feed so the row is visible"
            );
            assert_eq!(
                doc.get_heads(),
                before,
                "the immutable View callback queues rather than smuggling a write"
            );
            app.sync(&mut doc, &ROOT);
            assert_ne!(
                doc.get_heads(),
                before,
                "the next frame records opening an unread article through MarkRead"
            );
            assert!(
                crate::doc::FeedsDoc::load(&doc)
                    .article(target)
                    .is_some_and(|article| article.read)
            );
        }
    }
}
