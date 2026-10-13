#![allow(
    clippy::arithmetic_side_effects,
    reason = "test assertions and counters"
)]

//! On the real component runtime, a view follows each change by reading only
//! what the change touched — for local actions, undo and redo alike.

use std::cell::Cell;

use tilo_app::prelude::*;
use tilo_app::tilo_resource::__private::{ObjId, Value};
use tilo_app::tilo_resource::{Doc, ModelError, Slot};

thread_local! {
    static LOADS: Cell<usize> = const { Cell::new(0) };
}

fn loads() -> usize {
    LOADS.with(Cell::get)
}

/// Text that counts how often it is read from the document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Counted(pub String);

impl Model for Counted {
    tilo_app::tilo_resource::leaf_paths!();
    fn to_json(&self) -> tilo_app::tilo_resource::__private::serde_json::Value {
        self.0.to_json()
    }
    fn from_json(
        value: &tilo_app::tilo_resource::__private::serde_json::Value,
    ) -> Result<Self, ModelError> {
        <String as Model>::from_json(value).map(Self)
    }
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        LOADS.with(|loads| loads.set(loads.get() + 1));
        String::load(doc, value, id).map(Self)
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        self.0.store(doc, slot)
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Library {
    pub books: Collection<Book>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Book {
    pub title: Counted,
    pub read: bool,
}

#[resource(kind = "test.library", label = "Library")]
impl Library {
    /// Add books.
    #[action]
    pub fn add_many(&mut self, cx: &mut ActionCx, count: usize) {
        for n in 0..count {
            self.books.push(
                cx.id(),
                Book {
                    title: Counted(format!("Book {n}")),
                    read: false,
                },
            );
        }
    }

    /// Mark a book read.
    #[action]
    pub fn mark(&mut self, book: &Id, read: bool) -> Result<(), &'static str> {
        self.books.get_mut(book).ok_or("no such book")?.read = read;
        Ok(())
    }

    /// Retitle a book.
    #[action]
    pub fn retitle(&mut self, book: &Id, title: String) -> Result<(), &'static str> {
        self.books.get_mut(book).ok_or("no such book")?.title = Counted(title);
        Ok(())
    }
}

struct LibraryApp;

impl ResourceApp for LibraryApp {
    type Resource = Library;
    const ID: &'static str = "so.tilo.test-library";
    const TITLE: &'static str = "Library";

    fn view(cx: &mut ViewCx<Library>) -> Element {
        Box::new(Column::new().push(Box::new(cx.text(|library| {
            let read = library.books.values().filter(|book| book.read).count();
            format!("{read} read")
        }))))
    }
}

const BOOKS: usize = 300;

#[test]
fn a_view_follows_actions_and_undo_reading_only_what_changed() {
    let mut app = TestApp::<LibraryApp>::new().unwrap();
    app.invoke(Library::ADD_MANY.with(BOOKS)).unwrap();
    let book = app.state().books.ids().nth(BOOKS / 2).cloned().unwrap();

    // Applying a record action reads its record once (BOOKS titles); a view
    // that re-read the record would read them a second time.
    let before = loads();
    app.invoke(Library::RETITLE.with((book.clone(), "Renamed".into())))
        .unwrap();
    let spent = loads() - before;
    assert!(
        spent <= BOOKS + 2,
        "one retitle read {spent} titles; the view should read one"
    );
    let inspection = app.harness().inspection_json().unwrap();
    assert!(inspection.contains("0 read"), "{inspection}");

    let before = loads();
    app.invoke(Library::MARK.with((book, true))).unwrap();
    assert!(loads() - before <= BOOKS + 1);
    let inspection = app.harness().inspection_json().unwrap();
    assert!(inspection.contains("1 read"), "{inspection}");

    // Undo and redo are changes like any other.
    let before = loads();
    assert!(app.undo());
    assert!(
        loads() - before <= 2,
        "undo read {} titles",
        loads() - before
    );
    let inspection = app.harness().inspection_json().unwrap();
    assert!(inspection.contains("0 read"), "{inspection}");
    assert!(app.redo());
    let inspection = app.harness().inspection_json().unwrap();
    assert!(inspection.contains("1 read"), "{inspection}");
}
