//! Books' shelf as its shots start it: the app's own scenario imports the walk's EPUB through
//! the library's own action, and the window lays the book out as one column of the shelf.

use super::{BOOKS, Session};

/// The laid-out card that says `name` to a reader, at any depth.
fn card(node: &ui::headless::InspectionNode, name: &str) -> Option<[f64; 4]> {
    let says = node
        .semantics
        .as_ref()
        .and_then(|semantics| semantics.name.as_deref())
        .is_some_and(|said| said == name);
    if says {
        return Some(node.rect);
    }
    node.children.iter().find_map(|child| card(child, name))
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
    let rect = card(&inspection.root, "The Tide Glass")
        .unwrap_or_else(|| panic!("the shelf lays the book out: {:#?}", inspection.root));
    let (width, _) = session.window.harness().device_size();
    assert!(
        rect[2] < f64::from(width) / 2.0,
        "the card keeps a column's width, not the whole shelf: {rect:?} in {width} px"
    );
    let rgba = session.window.harness().render_rgba();
    let mut lit = 0;
    for y in 60..400 {
        for x in 0..width.min(900) {
            let at = (y as usize * width as usize + x as usize) * 4;
            let (r, g, b) = (rgba[at], rgba[at + 1], rgba[at + 2]);
            let lum = (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000;
            if lum > 110 {
                lit += 1;
            }
        }
    }
    assert!(lit > 100, "the card's words are painted: {lit} lit pixels");
}
