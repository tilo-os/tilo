//! A listing pages a provider that answers in one piece to its end, from
//! one answer however the provider moves meanwhile.
use std::cell::Cell;
use std::rc::Rc;

use super::tests::{FakeProvider, mk_chrome, mk_doc, mk_result};
use super::{
    QueryArgs, ResourceKind, SearchBudget, SearchContext, SearchProvider, SearchRegistry,
    SearchResult,
};
use crate::shell::chrome_state::SearchScope;

/// A provider on the default `query_page` whose answer changes every
/// time it is asked, as a list of windows does while they move.
struct ChurningProvider {
    rows: usize,
    asked: Cell<usize>,
}
impl SearchProvider for ChurningProvider {
    fn id(&self) -> &'static str {
        "churning"
    }
    fn name(&self) -> &'static str {
        "churning"
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }
    fn search_revision(&self, _ctx: &SearchContext<'_>) -> String {
        format!("asked-{}", self.asked.get())
    }
    fn query(&self, _ctx: SearchContext<'_>) -> Vec<SearchResult> {
        let asked = self.asked.get();
        self.asked.set(asked + 1);
        (0..self.rows)
            .map(|row| {
                mk_result(
                    ResourceKind::Action,
                    "churning",
                    &format!("answer{asked}-row{row}"),
                    "churning",
                    1,
                )
            })
            .collect()
    }
}

/// The default `query_page` pages a provider's whole answer to its end
/// for a direct caller, and a continuation cut from an answer that has
/// changed since is stale rather than a slice of a different one.
#[test]
fn a_default_provider_pages_its_answer_past_one_page() {
    let chrome = mk_chrome();
    let (doc, root) = mk_doc();
    let state = chrome.borrow();
    let ctx = SearchContext {
        query_scope: Some(SearchScope::All),
        include_programmatic: false,
        needle: "",
        wanted: &[ResourceKind::Action],
        chrome: &state,
        doc: &doc,
        doc_root: &root,
    };
    let stable = FakeProvider {
        id: "stable".into(),
        kinds: vec![ResourceKind::Action],
        results: (0..130)
            .map(|row| {
                mk_result(
                    ResourceKind::Action,
                    "stable",
                    &format!("row{row}"),
                    "row",
                    1,
                )
            })
            .collect(),
        max_score: None,
    };
    let mut ids = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let page = stable.query_page(ctx, cursor.as_ref(), SearchBudget::PALETTE);
        assert!(!page.stale_cursor, "page {pages} of an unchanged answer");
        pages += 1;
        ids.extend(page.results.into_iter().map(|row| row.id));
        if page.exhausted {
            break;
        }
        cursor = page.next_cursor;
        assert!(cursor.is_some(), "a page short of the end continues");
    }
    assert_eq!(pages, 3, "130 rows take three pages of 48");
    assert_eq!(
        ids,
        (0..130).map(|row| format!("row{row}")).collect::<Vec<_>>()
    );

    let churning = ChurningProvider {
        rows: 100,
        asked: Cell::new(0),
    };
    let first = churning.query_page(ctx, None, SearchBudget::PALETTE);
    let next = churning.query_page(ctx, first.next_cursor.as_ref(), SearchBudget::PALETTE);
    assert!(
        next.stale_cursor,
        "a cursor into an answer that has changed is stale"
    );
}

/// A listing pages a provider on the default `query_page` from the one
/// answer its first page took: complete, every row of that answer, the
/// provider asked once, however its answer moves meanwhile. A caller
/// following cursors one call at a time reads the same way, and the
/// next listing reads the provider as it is then.
#[test]
fn a_listing_reads_one_answer_of_a_provider_whose_answer_keeps_changing() {
    let chrome = mk_chrome();
    let (doc, root) = mk_doc();
    let state = chrome.borrow();
    let args = QueryArgs {
        needle: "",
        scope: SearchScope::All,
        active_provider_id: None,
        wanted_kinds: &[],
        chrome: &state,
        doc: &doc,
        doc_root: &root,
    };
    let provider = Rc::new(ChurningProvider {
        rows: 150,
        asked: Cell::new(0),
    });
    let mut registry = SearchRegistry::new();
    registry.register(provider.clone());

    let (rows, completion) = registry.query_catalogue(&args, true);
    assert!(completion.complete, "{completion:?}");
    assert_eq!(provider.asked.get(), 1, "one answer for the whole listing");
    assert_eq!(
        rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>(),
        (0..150)
            .map(|row| format!("answer0-row{row}"))
            .collect::<Vec<_>>()
    );

    let mut ids = Vec::new();
    let mut cursor = None;
    loop {
        let page = registry
            .query_provider_page("churning", &args, cursor.as_ref(), SearchBudget::PALETTE)
            .expect("the provider answers");
        assert!(!page.stale_cursor, "a held answer never goes stale");
        ids.extend(page.results.into_iter().map(|row| row.id));
        // The provider moves on between two calls.
        let _ = provider.query(SearchContext {
            query_scope: Some(SearchScope::All),
            include_programmatic: false,
            needle: "",
            wanted: &[ResourceKind::Action],
            chrome: &state,
            doc: &doc,
            doc_root: &root,
        });
        if page.exhausted {
            break;
        }
        cursor = page.next_cursor;
    }
    assert_eq!(
        ids,
        (0..150)
            .map(|row| format!("answer1-row{row}"))
            .collect::<Vec<_>>(),
        "the second listing reads the answer its first page took"
    );
}
