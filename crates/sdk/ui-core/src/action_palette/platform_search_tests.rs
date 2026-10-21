// The palette as the platform search surface: facets parsed from typed
// tokens and picked as chips reach providers structured, a slow provider's
// pages arrive on later frames under a loading row, and a hit's row shape
// reaches the results list.

fn platform_palette() -> ActionPalette {
    ActionPalette::with_inputs(crate::font_catalog::fixture_renderer(), || {
        SearchField::with_language("en_US")
    })
}

/// A snippet library that filters by language, recording each query.
struct Snippets {
    seen: std::rc::Rc<std::cell::RefCell<Vec<crate::search::SearchQuery>>>,
}

impl SearchProvider for Snippets {
    fn name(&self) -> &'static str {
        "Snippets"
    }
    fn search(&self, _: &str) -> Vec<SearchResult> {
        Vec::new()
    }
    fn search_page(
        &self,
        query: &crate::search::SearchQuery,
        cursor: Option<&SearchCursor>,
        _: usize,
        _: usize,
    ) -> crate::search::SearchPage {
        self.seen.borrow_mut().push(query.clone());
        if cursor.is_some() {
            return crate::search::SearchPage::empty();
        }
        let hits = [("Parse JSON", "rust"), ("Parse flags", "go")]
            .into_iter()
            .filter(|(title, language)| {
                title.to_lowercase().contains(&query.text().to_lowercase())
                    && query.admits("language", &[language])
            })
            .map(|(title, _)| {
                SearchResult::new("Snippets", title, format!("snippet:{title}")).with_row(
                    crate::search::SearchRow::file_hit("lib/json.rs", "fn parse(input: &str)")
                        .line(12)
                        .matched(3..8),
                )
            })
            .collect();
        crate::search::SearchPage::of(hits, None)
    }
    fn facets(&self) -> Vec<crate::search::Facet> {
        vec![
            crate::search::Facet::new("language", "Language")
                .value("rust", "Rust")
                .value("go", "Go"),
        ]
    }
}

#[test]
fn typed_and_picked_facets_reach_the_provider_structured() {
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut palette = platform_palette();
    palette.set_providers(vec![Box::new(Snippets { seen: seen.clone() })]);
    palette.open();
    assert_eq!(palette.facets().len(), 1);

    palette.set_query("parse language:rust");
    let query = seen.borrow().last().cloned().unwrap();
    assert_eq!(query.text(), "parse", "the token leaves the free text");
    assert_eq!(
        query.filters(),
        [crate::search::FacetFilter::new("language", "rust")]
    );
    assert_eq!(palette.result_labels(), ["Parse JSON"]);
    // The hit's row shape reaches the list.
    assert!(matches!(
        palette.results.selected().and_then(|hit| hit.row.as_ref()),
        Some(crate::search::SearchRow::FileHit { .. })
    ));

    // Unpicking the chip removes the typed token.
    palette.toggle_facet(&crate::search::FacetFilter::new("language", "rust"));
    assert_eq!(palette.query(), "parse");
    assert_eq!(palette.result_labels().len(), 2);

    // Picking a chip filters without typing.
    palette.toggle_facet(&crate::search::FacetFilter::new("language", "go"));
    assert_eq!(palette.result_labels(), ["Parse flags"]);
    assert_eq!(
        seen.borrow().last().unwrap().filters(),
        [crate::search::FacetFilter::new("language", "go")]
    );
}

/// A mail server: the first page is what is cached locally, the rest is
/// fetched and arrives when `ready` is set.
struct SlowMail {
    ready: std::rc::Rc<std::cell::Cell<bool>>,
}

impl SearchProvider for SlowMail {
    fn name(&self) -> &'static str {
        "Mail"
    }
    fn search(&self, _: &str) -> Vec<SearchResult> {
        Vec::new()
    }
    fn search_page(
        &self,
        query: &crate::search::SearchQuery,
        cursor: Option<&SearchCursor>,
        _: usize,
        _: usize,
    ) -> crate::search::SearchPage {
        if cursor.is_some() || query.text().is_empty() {
            return crate::search::SearchPage::empty();
        }
        crate::search::SearchPage::pending(
            vec![SearchResult::new("Mail", "Cached invoice", "mail:1")],
            crate::search::PendingToken::new(7),
        )
    }
    fn poll_page(&self, token: &crate::search::PendingToken) -> crate::search::PagePoll {
        assert_eq!(token.id(), 7);
        if !self.ready.get() {
            return crate::search::PagePoll::Pending;
        }
        crate::search::PagePoll::Ready(crate::search::SearchPage::of(
            vec![
                SearchResult::new("Mail", "Server invoice", "mail:2").with_row(
                    crate::search::SearchRow::message("Ada", "Server invoice")
                        .snippet("Attached is March")
                        .date("Tue")
                        .unread(true),
                ),
            ],
            None,
        ))
    }
}

/// An app's own search: until the app has taken the query in, it answers
/// from what it showed for the query before (a row offering the previous
/// text), then answers the whole query again.
struct Reanswering;

impl SearchProvider for Reanswering {
    fn name(&self) -> &'static str {
        "Videos"
    }
    fn search(&self, _: &str) -> Vec<SearchResult> {
        Vec::new()
    }
    fn search_page(
        &self,
        query: &crate::search::SearchQuery,
        cursor: Option<&SearchCursor>,
        _: usize,
        _: usize,
    ) -> crate::search::SearchPage {
        if cursor.is_some() || query.text().is_empty() {
            return crate::search::SearchPage::empty();
        }
        crate::search::SearchPage::pending(
            vec![SearchResult::new(
                "Videos",
                "Search for invoic",
                "search:invoic",
            )],
            crate::search::PendingToken::new(3),
        )
    }
    fn poll_page(&self, _: &crate::search::PendingToken) -> crate::search::PagePoll {
        crate::search::PagePoll::Answered(crate::search::SearchPage::of(
            vec![
                SearchResult::new("Videos", "Search for invoice", "search:invoice"),
                SearchResult::new("Videos", "Invoice walkthrough", "video:1"),
            ],
            None,
        ))
    }
}

#[test]
fn a_provider_answering_the_query_again_replaces_its_earlier_rows() {
    let mut palette = platform_palette();
    palette.set_providers(vec![Box::new(Reanswering)]);
    palette.open();
    palette.set_query("invoice");
    assert_eq!(palette.result_labels(), ["Search for invoic"]);
    let answered = palette.advance_search_page();
    assert!(!answered.pending);
    let mut labels = palette.result_labels();
    labels.sort();
    assert_eq!(
        labels,
        ["Invoice walkthrough", "Search for invoice"],
        "the answer stands alone, without the row for the earlier text"
    );
}

#[test]
fn a_slow_providers_pages_append_under_a_loading_row() {
    let ready = std::rc::Rc::new(std::cell::Cell::new(false));
    let mut palette = platform_palette();
    palette.set_providers(vec![Box::new(SlowMail {
        ready: ready.clone(),
    })]);
    palette.open();
    palette.set_query("invoice");
    assert_eq!(palette.result_labels(), ["Cached invoice"]);
    assert!(palette.results.is_loading(), "a loading row ends the list");
    let waiting = palette.advance_search_page();
    assert!(waiting.pending, "the palette keeps asking");
    assert_eq!(palette.result_labels(), ["Cached invoice"]);

    ready.set(true);
    let arrived = palette.advance_search_page();
    assert_eq!(arrived.added, 1);
    assert!(!arrived.pending);
    let mut labels = palette.result_labels();
    labels.sort();
    assert_eq!(
        labels,
        ["Cached invoice", "Server invoice"],
        "appended to the same list"
    );
    assert!(!palette.results.is_loading());

    // A new query forgets the old token.
    ready.set(false);
    palette.set_query("receipt");
    assert!(palette.results.is_loading());
}
