//! Search in Books, as a person uses it: through the window bar's field,
//! over the sample shelf a capture shows.

use ui::framework::testing::TestApp;
use ui::framework::views::Chip;
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
            .all(|section| section.state.is_none()),
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

#[test]
fn the_store_answers_below_the_library_and_its_books_can_be_got() {
    let mut app = TestApp::<EbookShell>::capture(Some("search")).unwrap();
    let results = books(&mut app).query.engine.results().clone();
    let store = results.remote.first().expect("the store has its section");
    assert_eq!(store.label, crate::store::STORE_LABEL);
    assert!(store.is_answered());
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

/// The chapter and paragraph of `book` whose text holds `words`.
fn place_of(book: &crate::model::Book, words: &str) -> (usize, usize) {
    book.chapters
        .iter()
        .enumerate()
        .find_map(|(chapter, held)| {
            held.blocks
                .iter()
                .position(|block| block.plain_text().contains(words))
                .map(|block| (chapter, block))
        })
        .unwrap_or_else(|| panic!("the book holds {words:?}"))
}

#[test]
fn a_book_with_no_local_content_is_found_by_a_phrase_from_its_middle_and_opens_there() {
    use crate::resource::{StoredContent, index_book, shelve};

    let mut app = TestApp::<EbookShell>::new().unwrap();
    // Nothing reaches outside the process, and the work done off the UI
    // thread runs when a frame pumps it.
    books(&mut app).query = super::query_view::BookQuery::hermetic();
    let content = app.content();
    let bytes = crate::test_epub::long_classic(24, 6);
    let book = crate::epub::parse_epub(&bytes, 0, "").unwrap();
    let (chapter, block) = place_of(&book, "Book 12, paragraph 4:");
    // Another device indexed the book; its segment reached this one, its
    // text has not.
    let segment = crate::library_index::Segment::build(&book).encode();
    let index = content.put(&segment).unwrap();
    let text = tilo_content::hash_bytes(&bytes);
    let mut entry =
        crate::content::prepare_imported_book(std::path::Path::new("odyssey.epub"), book.clone()).1;
    entry.shared = false;
    entry.content = crate::model::LibraryEntry::stored(&text, bytes.len() as u64).unwrap();
    app.invoke(shelve(&entry)).unwrap();
    app.invoke(index_book(
        &book.id,
        StoredContent {
            hash: index,
            size: segment.len() as u64,
        },
    ))
    .unwrap();
    app.settle();
    assert!(
        books(&mut app)
            .library
            .iter()
            .all(|held| held.id != book.id),
        "the book's text is not on this device"
    );

    books(&mut app).query_opened();
    books(&mut app).set_query_text("book 12 paragraph 4");
    for _ in 0..3 {
        books(&mut app).settle_query();
        app.settle();
    }
    let anchor = format!("ebook.location:{}:{chapter}:{block}", book.id);
    let item = books(&mut app)
        .query
        .engine
        .results()
        .local
        .iter()
        .flat_map(|group| &group.items)
        .find(|item| item.id == anchor)
        .cloned()
        .unwrap_or_else(|| panic!("the passage is found: {:?}", found(&mut app, PASSAGE)));
    assert_eq!(item.kind, PASSAGE);
    // Its row names the chapter and the book; the words around the match
    // are not kept in the index.
    assert_eq!(item.value("title"), Some("Book 12"));
    assert_eq!(item.value("place"), Some(book.title.as_str()));

    // Opening it fetches the text, which arrives, and lands on the passage.
    content.put(&bytes).unwrap();
    books(&mut app).open_hit(&anchor);
    for _ in 0..3 {
        books(&mut app).settle_query();
        app.settle();
    }
    let books = books(&mut app);
    assert_eq!(books.view, View::Reader, "the passage opens in the reader");
    assert_eq!(
        books.open_book().map(|open| open.id.as_str()),
        Some(book.id.as_str())
    );
    assert_eq!(books.chapter, chapter, "at the passage's chapter");
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

#[test]
fn the_capture_of_a_book_elsewhere_finds_it_from_its_index_beside_the_shelf() {
    let mut app = TestApp::<EbookShell>::capture(Some("search-elsewhere")).unwrap();
    let passages = found(&mut app, PASSAGE);
    assert!(
        passages
            .iter()
            .any(|id| id.starts_with(&format!("ebook.location:{TIDEGLASS}:"))),
        "the shelf's own passages: {passages:?}"
    );
    let elsewhere: Vec<_> = books(&mut app)
        .query
        .engine
        .results()
        .local
        .iter()
        .flat_map(|group| &group.items)
        .filter(|item| item.value("place") == Some("The Glassmaker's Apprentice"))
        .cloned()
        .collect();
    assert_eq!(elsewhere.len(), 2, "both glass paragraphs: {passages:?}");
    assert_eq!(elsewhere[0].value("title"), Some("I. The Furnace"));
    assert_eq!(
        elsewhere[0].value("place"),
        Some("The Glassmaker's Apprentice")
    );
    assert_eq!(elsewhere[0].value("excerpt"), None);
}

#[test]
fn a_book_whose_text_arrives_later_is_indexed_on_that_device() {
    use crate::resource::shelve;

    let mut app = TestApp::<EbookShell>::new().unwrap();
    books(&mut app).query = super::query_view::BookQuery::hermetic();
    let content = app.content();
    let bytes = crate::test_epub::long_classic(24, 6);
    let book = crate::epub::parse_epub(&bytes, 0, "").unwrap();
    let text = tilo_content::hash_bytes(&bytes);
    let mut entry =
        crate::content::prepare_imported_book(std::path::Path::new("odyssey.epub"), book.clone()).1;
    entry.shared = false;
    entry.content = crate::model::LibraryEntry::stored(&text, bytes.len() as u64).unwrap();
    app.invoke(shelve(&entry)).unwrap();
    app.settle();
    let index_of = |app: &TestApp<EbookShell>| {
        app.state()
            .entries
            .values()
            .find(|row| row.id == book.id)
            .and_then(crate::model::LibraryEntry::index_hash)
    };
    assert_eq!(index_of(&app), None, "nobody has indexed it yet");

    // Its text arrives as it is opened here: from the search's card.
    content.put(&bytes).unwrap();
    books(&mut app).query_opened();
    books(&mut app).open_hit(&crate::resource::book_place_resource(&book.id));
    for _ in 0..4 {
        books(&mut app).settle_query();
        app.settle();
    }
    assert_eq!(
        books(&mut app).open_book().map(|open| open.id.clone()),
        Some(book.id.clone()),
        "the book opens once its text is here"
    );
    let index = index_of(&app).expect("the device that now has its text indexed it");
    let segment = crate::library_index::Segment::decode(&content.read(&index).unwrap())
        .expect("the index is kept in the content store");
    let (chapter, block) = place_of(&book, "Book 12, paragraph 4:");
    assert_eq!(
        segment.find("book 12 paragraph 4", 10),
        [crate::library_index::Place { chapter, block }]
    );
}

#[test]
fn a_guest_who_may_only_view_builds_no_index_and_sends_no_write() {
    use crate::resource::shelve;

    let mut app = TestApp::<EbookShell>::new().unwrap();
    books(&mut app).query = super::query_view::BookQuery::hermetic();
    let content = app.content();
    let bytes = crate::test_epub::long_classic(24, 6);
    let book = crate::epub::parse_epub(&bytes, 0, "").unwrap();
    let mut entry =
        crate::content::prepare_imported_book(std::path::Path::new("odyssey.epub"), book.clone()).1;
    entry.shared = false;
    entry.content =
        crate::model::LibraryEntry::stored(&tilo_content::hash_bytes(&bytes), bytes.len() as u64)
            .unwrap();
    app.invoke(shelve(&entry)).unwrap();
    app.settle();

    // A guest who may only view opens the shelf; the text reaches their
    // device as they open the book.
    app.set_role(ui::permissions::Role::Viewer);
    content.put(&bytes).unwrap();
    books(&mut app).query_opened();
    books(&mut app).open_hit(&crate::resource::book_place_resource(&book.id));
    for _ in 0..4 {
        books(&mut app).settle_query();
        app.settle();
    }
    assert_eq!(
        books(&mut app).open_book().map(|open| open.id.clone()),
        Some(book.id.clone()),
        "the book opens for the guest"
    );
    assert!(
        books(&mut app).query.index_mut().take_built().is_empty()
            && !books(&mut app).query.index_mut().building(),
        "no index is built for a write the room would refuse"
    );
    assert!(
        books(&mut app).dispatch.refused().get().is_none(),
        "nothing the guest may not write was sent"
    );
    let row = app
        .state()
        .entries
        .values()
        .find(|row| row.id == book.id)
        .cloned()
        .unwrap();
    assert_eq!(row.index_hash(), None, "the row names no index");
}

#[test]
fn an_index_and_a_text_on_their_way_are_waited_for_and_the_passage_opens() {
    use ui::collaboration::record_content::{ContentLane, RecordContent};

    use crate::resource::{StoredContent, index_book, shelve};

    let mut app = TestApp::<EbookShell>::new().unwrap();
    books(&mut app).query = super::query_view::BookQuery::hermetic();
    // This device's store reaches the record's other devices over its route.
    let lane = ContentLane::of("so.tilo.books-query-test", "arriving");
    books(&mut app).content = Some(RecordContent::store(
        tilo_content::ContentStore::memory(),
        std::sync::Arc::clone(&lane),
    ));
    let bytes = crate::test_epub::long_classic(24, 6);
    let book = crate::epub::parse_epub(&bytes, 0, "").unwrap();
    let (chapter, block) = place_of(&book, "Book 12, paragraph 4:");
    let segment = crate::library_index::Segment::build(&book).encode();
    let mut entry =
        crate::content::prepare_imported_book(std::path::Path::new("odyssey.epub"), book.clone()).1;
    entry.shared = false;
    entry.content =
        crate::model::LibraryEntry::stored(&tilo_content::hash_bytes(&bytes), bytes.len() as u64)
            .unwrap();
    app.invoke(shelve(&entry)).unwrap();
    app.invoke(index_book(
        &book.id,
        StoredContent {
            hash: tilo_content::hash_bytes(&segment),
            size: segment.len() as u64,
        },
    ))
    .unwrap();
    app.settle();

    books(&mut app).query_opened();
    books(&mut app).set_query_text("book 12 paragraph 4");
    let anchor = format!("ebook.location:{}:{chapter}:{block}", book.id);
    let settle = |app: &mut TestApp<EbookShell>| {
        for _ in 0..3 {
            books(app).settle_query();
            app.settle();
        }
    };
    settle(&mut app);
    assert!(
        !found(&mut app, PASSAGE).contains(&anchor),
        "the index is not here yet"
    );
    lane.arrive(tilo_content::hash_bytes(&segment), segment);
    settle(&mut app);
    assert!(
        found(&mut app, PASSAGE).contains(&anchor),
        "found once the index arrives: {:?}",
        found(&mut app, PASSAGE)
    );

    books(&mut app).open_hit(&anchor);
    settle(&mut app);
    assert_ne!(books(&mut app).view, View::Reader, "its text is on its way");
    assert_eq!(
        books(&mut app).load_error.as_deref(),
        None,
        "waiting is not a failure"
    );
    lane.arrive(tilo_content::hash_bytes(&bytes), bytes);
    settle(&mut app);
    let books = books(&mut app);
    assert_eq!(books.view, View::Reader, "it opens once its text is here");
    assert_eq!(books.chapter, chapter, "at the passage's chapter");
}

/// A store book that could not be fetched says to try again, and carries
/// Try again: pressed by pointer or through its semantic action, wide or
/// compact, it fetches the book again.
#[test]
fn a_store_book_that_did_not_arrive_carries_try_again() {
    for (width, semantic) in [
        (1280.0, false),
        (1280.0, true),
        (420.0, false),
        (420.0, true),
    ] {
        let mut app = TestApp::<EbookShell>::capture(Some("search")).unwrap();
        app.harness().present_in_window_bar();
        {
            let books = books(&mut app);
            books.report_failure(format!(
                "Couldn’t open 7.epub: {}",
                crate::store::unreachable_message()
            ));
            books.retry_store = Some(7);
        }
        app.settle();
        let shown = app
            .harness()
            .window_bar_notice(width)
            .expect("the failure is in the bar");
        let names: Vec<String> = shown
            .buttons
            .into_iter()
            .map(|button| button.name)
            .collect();
        assert_eq!(names, ["Try again"], "{width}");
        let before = books(&mut app).import_generation;
        let harness = app.harness();
        let pressed = if semantic {
            harness.invoke_window_notice_action(width, 0)
        } else {
            harness.click_window_notice_action(width, 0)
        };
        pressed.expect("Try again is in the bar");
        app.settle();
        assert!(
            books(&mut app).import_generation > before,
            "Try again fetched the book again ({width}, semantic: {semantic})"
        );
    }
}
