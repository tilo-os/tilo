//! The shelf of a library whose books' text is on other devices: every book
//! of the library is on it, whichever device has the text.

use std::collections::BTreeSet;

use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;

use super::{BooksApp, View};
use crate::EbookShell;
use crate::model::{LibraryEntry, LibrarySort, Progress};
use crate::resource::{Ebook, shelve};

fn books(app: &mut TestApp<EbookShell>) -> &mut BooksApp {
    app.surface_mut::<BooksApp>()
}

/// A fresh profile, its work done off the UI thread when a frame pumps it.
fn fresh() -> TestApp<EbookShell> {
    let mut app = TestApp::<EbookShell>::new().unwrap();
    books(&mut app).query = super::query_view::BookQuery::hermetic();
    app
}

/// The synced row of a book called `title` whose bytes are `bytes`, kept
/// in a content store.
fn row(title: &str, bytes: &[u8]) -> LibraryEntry {
    LibraryEntry {
        id: crate::epub::content_id(bytes),
        shared: false,
        title: title.to_owned(),
        author: "A. Writer".to_owned(),
        subject: String::new(),
        added: 0,
        content: LibraryEntry::stored(&tilo_content::hash_bytes(bytes), bytes.len() as u64)
            .unwrap(),
        index: ui::tilo_resource::Bytes::Empty,
    }
}

/// The titles on the shelf, in the order it shows them.
fn shelf(app: &mut TestApp<EbookShell>) -> Vec<String> {
    app.frame();
    let books = books(app);
    books
        .shelf_order
        .iter()
        .filter_map(|&index| books.shelf_book(index).map(|book| book.title.clone()))
        .collect()
}

#[test]
fn a_fresh_device_with_a_synced_library_of_n_books_shows_n_tiles() {
    const N: usize = 12;
    let mut app = fresh();
    let titles: BTreeSet<String> = (0..N).map(|n| format!("Volume {n:02}")).collect();
    for (n, title) in titles.iter().enumerate() {
        app.invoke(shelve(&row(title, format!("text {n}").as_bytes())))
            .unwrap();
    }
    app.settle();
    assert!(books(&mut app).library.is_empty(), "no book's text is here");
    let shown = shelf(&mut app);
    assert_eq!(shown.len(), N, "{shown:?}");
    assert_eq!(shown.into_iter().collect::<BTreeSet<_>>(), titles);
}

#[test]
fn sorting_and_hiding_finished_books_treat_books_elsewhere_as_books_here() {
    let mut app = fresh();
    let here = crate::test_epub::long_classic(3, 2);
    app.content().put(&here).unwrap();
    // Its title is the one its text gives it.
    let local = row("The Test Odyssey", &here);
    let finished = row("Adam Bede", b"adam bede");
    let unread = row("Wuthering Heights", b"wuthering heights");
    for entry in [&local, &finished, &unread] {
        app.invoke(shelve(entry)).unwrap();
    }
    app.settle();
    assert_eq!(books(&mut app).library.len(), 1, "one book's text is here");
    let paths = Ebook::paths();
    app.invoke(Invoke::edit(&paths.prefs.sort.set(LibrarySort::Title)).unwrap())
        .unwrap();
    app.settle();
    assert_eq!(
        shelf(&mut app),
        ["Adam Bede", "The Test Odyssey", "Wuthering Heights"],
        "sorted by title across both"
    );

    let progress: ui::tilo_resource::Map<Progress> = [(
        finished.id,
        Progress {
            chapter: 0,
            block: 0,
            fraction: 1.0,
            opened: 1,
        },
    )]
    .into_iter()
    .collect();
    app.invoke(Invoke::edit(&paths.progress.set(progress)).unwrap())
        .unwrap();
    app.invoke(Invoke::edit(&paths.prefs.hide_read.set(true)).unwrap())
        .unwrap();
    app.settle();
    assert_eq!(
        shelf(&mut app),
        ["The Test Odyssey", "Wuthering Heights"],
        "a finished book elsewhere is hidden like one here"
    );
}

#[test]
fn a_book_elsewhere_shows_its_text_on_its_way_and_opens_when_it_arrives() {
    use ui::collaboration::record_content::{ContentLane, RecordContent};

    let mut app = fresh();
    let bytes = crate::test_epub::long_classic(6, 3);
    let entry = row("The Test Odyssey", &bytes);
    app.invoke(shelve(&entry)).unwrap();
    let progress: ui::tilo_resource::Map<Progress> = [(
        entry.id.clone(),
        Progress {
            chapter: 4,
            block: 1,
            fraction: 0.7,
            opened: 1,
        },
    )]
    .into_iter()
    .collect();
    app.invoke(Invoke::edit(&Ebook::paths().progress.set(progress)).unwrap())
        .unwrap();
    app.settle();
    // The record's content arrives on its route from a device that has it.
    let lane = ContentLane::of("so.tilo.books-shelf-test", "arriving");
    books(&mut app).content = Some(RecordContent::store(
        tilo_content::ContentStore::memory(),
        std::sync::Arc::clone(&lane),
    ));

    let place = crate::resource::book_place_resource(&entry.id);
    books(&mut app).open_hit(&place);
    for _ in 0..2 {
        books(&mut app).settle_query();
        app.settle();
    }
    assert!(
        books(&mut app).arriving(&entry.id).is_some(),
        "its tile shows its text on its way"
    );
    assert_ne!(books(&mut app).view, View::Reader);
    assert_eq!(shelf(&mut app), ["The Test Odyssey"], "its tile stays");

    lane.arrive(tilo_content::hash_bytes(&bytes), bytes);
    for _ in 0..3 {
        books(&mut app).settle_query();
        app.settle();
    }
    let books = books(&mut app);
    assert_eq!(books.view, View::Reader, "it opens once its text is here");
    assert_eq!(
        books.open_book().map(|open| open.id.clone()),
        Some(entry.id.clone())
    );
    assert_eq!(books.chapter, 4, "where its reader left off");
    assert!(books.arriving(&entry.id).is_none());
}

// When nothing this device can reach has a book's text, opening it says so
// on the book's own tile, as information rather than a failure, and the
// tile stays.
#[test]
fn a_book_nobody_reachable_has_says_so_on_open_and_keeps_its_tile() {
    let mut app = fresh();
    let entry = row("Lost Pages", b"nobody has these bytes");
    app.invoke(shelve(&entry)).unwrap();
    app.settle();
    // A store that reaches nothing beyond this device.
    books(&mut app).content = Some(tilo_content::ContentStore::memory());
    books(&mut app).open_hit(&crate::resource::book_place_resource(&entry.id));
    for _ in 0..2 {
        books(&mut app).settle_query();
        app.settle();
    }
    assert_eq!(
        tile_status(&mut app, "Lost Pages"),
        Some((
            "None of your online devices has this book.".to_owned(),
            None
        )),
        "its tile says so, to a screen reader too"
    );
    assert_eq!(
        books(&mut app).load_error.as_deref(),
        None,
        "nothing failed"
    );
    assert_ne!(books(&mut app).view, View::Reader);
    assert_eq!(shelf(&mut app), ["Lost Pages"], "its tile stays");
}

/// What the tile of the book titled `title` says of its text, as a screen
/// reader hears it: its value and description.
fn tile_status(app: &mut TestApp<EbookShell>, title: &str) -> Option<(String, Option<String>)> {
    fn find(node: &ui::a11y::AccessNode, title: &str) -> Option<(String, Option<String>)> {
        if node
            .name
            .as_deref()
            .is_some_and(|name| name.contains(title))
            && let Some(value) = &node.value
        {
            return Some((value.clone(), node.description.clone()));
        }
        node.children.iter().find_map(|child| find(child, title))
    }
    app.settle();
    let tree = ui::widget::Widget::accessibility(books(app))?;
    find(&tree, title)
}

// A book whose holder is offline is not fetching: opening it shows on its
// tile that it waits for a device that has it, still, with no loading bar;
// the moment a holder connects its tile shows the loading bar, and it opens
// where its reader left off once its text is here.
#[test]
fn a_book_whose_holder_is_offline_waits_and_opens_when_the_holder_connects() {
    use ui::collaboration::record_content::{ContentLane, RecordContent};

    let mut app = fresh();
    let bytes = crate::test_epub::long_classic(6, 3);
    let hash = tilo_content::hash_bytes(&bytes);
    let entry = row("The Test Odyssey", &bytes);
    app.invoke(shelve(&entry)).unwrap();
    let progress: ui::tilo_resource::Map<Progress> = [(
        entry.id.clone(),
        Progress {
            chapter: 4,
            block: 1,
            fraction: 0.7,
            opened: 1,
        },
    )]
    .into_iter()
    .collect();
    app.invoke(Invoke::edit(&Ebook::paths().progress.set(progress)).unwrap())
        .unwrap();
    app.settle();
    let lane = ContentLane::of("so.tilo.books-shelf-test", "waiting");
    books(&mut app).content = Some(RecordContent::store(
        tilo_content::ContentStore::memory(),
        std::sync::Arc::clone(&lane),
    ));
    let settle = |app: &mut TestApp<EbookShell>| {
        for _ in 0..3 {
            books(app).settle_query();
            app.settle();
        }
    };

    books(&mut app).open_hit(&crate::resource::book_place_resource(&entry.id));
    settle(&mut app);
    // The host finds no device that has it connected.
    lane.stands(hash.clone(), tilo_content::ContentState::Waiting);
    settle(&mut app);
    {
        assert_eq!(
            tile_status(&mut app, "The Test Odyssey"),
            Some((
                "Waiting for a device that has it".to_owned(),
                Some("Opens when a device that has it is online.".to_owned())
            )),
            "its tile says it waits, to a screen reader too"
        );
        let books = books(&mut app);
        assert_eq!(
            books.load_error.as_deref(),
            None,
            "waiting is not a failure"
        );
        assert!(
            books.arriving(&entry.id).is_none(),
            "no loading bar while it waits"
        );
        assert!(!books.awaiting_arrival(), "nothing animates while it waits");
        assert_ne!(books.view, View::Reader);
    }
    assert_eq!(shelf(&mut app), ["The Test Odyssey"], "its tile stays");

    // A device that has it connects: it is on its way.
    lane.stands(hash.clone(), tilo_content::ContentState::Pending);
    settle(&mut app);
    {
        assert_eq!(tile_status(&mut app, "The Test Odyssey"), None);
        let books = books(&mut app);
        assert!(
            books.arriving(&entry.id).is_some(),
            "its tile shows the loading bar"
        );
    }

    lane.arrive(hash, bytes);
    settle(&mut app);
    let books = books(&mut app);
    assert_eq!(books.view, View::Reader, "it opens once its text is here");
    assert_eq!(books.chapter, 4, "where its reader left off");
}
