//! Search in Books, as a person uses it: through the window bar's field,
//! over the sample shelf a capture shows.

use ui::framework::testing::TestApp;
use ui::framework::views::{Chip, RemoteState};
use ui::widget::Queryable as _;

use super::query_view::{BOOK, PASSAGE};
use super::{BooksApp, View};
use crate::EbookShell;
use crate::search_provider::BOOK_FACET;

const TIDEGLASS: &str = "book:tideglass";
const TIDEGLASS_TITLE: &str = "The Tideglass Letters";

fn books(app: &mut TestApp<EbookShell>) -> &mut BooksApp {
    app.surface_mut::<BooksApp>()
}

/// The ids of the open query's results of `kind`.
fn found(app: &mut TestApp<EbookShell>, kind: &str) -> Vec<String> {
    books(app)
        .query
        .engine
        .results()
        .local
        .iter()
        .filter(|group| group.kind == kind)
        .flat_map(|group| group.items.iter().map(|item| item.id.clone()))
        .collect()
}

// USER REQUIREMENT (2026-10-21): searching finds the words of the books
// themselves, and a match in a book's text opens the book at that passage.
#[test]
fn a_passage_found_opens_its_book_at_the_passage() {
    let mut app = TestApp::<EbookShell>::capture(Some("search")).unwrap();
    assert_eq!(books(&mut app).view, View::Search);
    let passage = found(&mut app, PASSAGE)
        .into_iter()
        .find(|id| id.starts_with(&format!("ebook.location:{TIDEGLASS}:")))
        .expect("the Tideglass letters' words are found");
    let place: Vec<usize> = passage
        .rsplit(':')
        .take(2)
        .map(|part| part.parse().unwrap())
        .collect();
    let (block, chapter) = (place[0], place[1]);
    let excerpt = books(&mut app)
        .query
        .engine
        .results()
        .local
        .iter()
        .flat_map(|group| &group.items)
        .find(|item| item.id == passage)
        .and_then(|item| item.value("excerpt").map(str::to_owned))
        .expect("a passage shows its words");
    assert!(
        excerpt.to_lowercase().contains("glass"),
        "the words around the match: {excerpt}"
    );

    books(&mut app).open_hit(&passage);
    app.settle();
    app.settle();
    let books = books(&mut app);
    assert_eq!(books.view, View::Reader, "the passage opens in the reader");
    assert_eq!(
        books.open_book().map(|book| book.id.as_str()),
        Some(TIDEGLASS)
    );
    assert_eq!(books.chapter, chapter, "at the passage's chapter");
    // The paragraph is on the page shown, wherever on it; before the
    // chapter is laid out, the page waits to be turned to it.
    match books
        .layout
        .as_ref()
        .filter(|layout| layout.key.chapter == chapter)
    {
        Some(layout) => assert_eq!(
            books.page,
            super::page_for_block(layout, block),
            "with the passage's paragraph on the page"
        ),
        None => assert_eq!(
            books.pending_jump_block,
            Some(block),
            "with the passage's paragraph on the page"
        ),
    }
}

// USER REQUIREMENT (2026-10-21): searching from inside a book searches that
// book; clearing its chip searches the whole library.
#[test]
fn a_search_from_inside_a_book_is_the_books_until_its_chip_is_cleared() {
    let mut app = TestApp::<EbookShell>::capture(Some("search-in-book")).unwrap();
    let query = books(&mut app).query.engine.results().query.clone();
    assert_eq!(
        query.chips,
        [Chip::value(BOOK_FACET, TIDEGLASS_TITLE)],
        "the Book chip names the open book"
    );
    let passages = found(&mut app, PASSAGE);
    assert!(!passages.is_empty(), "the book's words are found");
    assert!(
        passages
            .iter()
            .all(|id| id.starts_with(&format!("ebook.location:{TIDEGLASS}:"))),
        "only the open book's: {passages:?}"
    );
    assert!(
        books(&mut app)
            .query
            .engine
            .results()
            .remote
            .iter()
            .all(|section| section.state == RemoteState::Idle),
        "the store is not asked inside a book"
    );

    let wider = query.cleared(BOOK_FACET);
    books(&mut app).on_set_query(&wider);
    books(&mut app).settle_query();
    assert!(
        books(&mut app)
            .query
            .engine
            .results()
            .query
            .chips
            .is_empty(),
        "the chip is cleared"
    );
    assert!(
        found(&mut app, BOOK).len() + found(&mut app, PASSAGE).len() > passages.len(),
        "the library answers"
    );
}

// USER REQUIREMENT (2026-10-21): Escape (closing the search) returns to the
// view before it exactly: the page that was open, or the shelf as it was.
#[test]
fn closing_the_search_returns_to_the_page_it_began_on() {
    let mut app = TestApp::<EbookShell>::capture(Some("search-in-book")).unwrap();
    let reading = books(&mut app);
    assert_eq!(reading.view, View::Search);
    assert_eq!(reading.query.returns_to, View::Reader);
    let (book, chapter, page) = (reading.book_idx, reading.chapter, reading.page);
    reading.query_closed();
    assert_eq!(reading.view, View::Reader);
    assert_eq!(
        (reading.book_idx, reading.chapter, reading.page),
        (book, chapter, page)
    );
    assert!(!reading.query.engine.is_open());

    let mut app = TestApp::<EbookShell>::capture(Some("search")).unwrap();
    let books = books(&mut app);
    assert_eq!(books.query.returns_to, View::Library);
    books.query_closed();
    assert_eq!(books.view, View::Library);
}

// USER REQUIREMENT (2026-10-21): the book store is searched beside the
// library, in its own section below it, and a book found there can be got
// from its row.
#[test]
fn the_store_answers_below_the_library_and_its_books_can_be_got() {
    let mut app = TestApp::<EbookShell>::capture(Some("search")).unwrap();
    let results = books(&mut app).query.engine.results().clone();
    let store = results.remote.first().expect("the store has its section");
    assert_eq!(store.label, crate::store::STORE_LABEL);
    assert_eq!(store.state, RemoteState::Loaded);
    let got: Vec<&str> = store
        .rows()
        .filter_map(|item| item.value("title"))
        .collect();
    assert_eq!(got, ["Through the Looking-Glass"]);
    assert!(
        !results.local.is_empty(),
        "the library's results come first, in their own groups"
    );
    // Its row offers to get it, under the store's name.
    let label = books(&mut app)
        .query
        .label_of("gutenberg:12")
        .map(str::to_owned);
    assert_eq!(
        label.as_deref(),
        Some("Get Through the Looking-Glass from Project Gutenberg")
    );
}
