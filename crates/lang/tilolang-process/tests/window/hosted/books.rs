//! Books' shelf as its shots start it: the app's own scenario imports the walk's EPUB through
//! the library's own action, and the window lays the book out on the shelf.

use super::{BOOKS, Session};

/// Whether a laid-out widget says `said` to a reader, at any depth.
fn says(node: &ui::headless::InspectionNode, said: &str) -> bool {
    node.semantics
        .as_ref()
        .and_then(|semantics| semantics.name.as_deref())
        .is_some_and(|name| name == said)
        || node.children.iter().any(|child| says(child, said))
}

// The library's scenario is the shelf the shots open on: the book is the one the app's walk
// opens from the person's files, taken in by the library's own import.
#[test]
fn the_shelves_scenario_puts_its_book_on_the_shelf() {
    let mut session = Session::new(BOOKS, "ada");
    session.stage("a shelf with a book");
    session.settle();
    let root = session
        .instance_host()
        .root_of(BOOKS)
        .expect("the library's root");
    let books = session
        .instance_host()
        .runtime()
        .read(&root, "books")
        .expect("the root lists its books");
    assert!(
        matches!(&books, tilolang_tilo::runtime::Value::List(books) if books.len() == 1),
        "the import put one book on the shelf: {books:?}"
    );
    let inspection = session.window.harness().inspection();
    assert!(
        says(&inspection.root, "The Tide Glass"),
        "the shelf lays the book out: {:#?}",
        inspection.root
    );
}
