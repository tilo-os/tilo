//! Localized: every word the user reads comes from a Fluent catalog.
//!
//! Widgets take message ids, never prose: `Text::new("localized-title")`,
//! `Button::new(id, "localized-add")`, placeholders and labels alike.
//! Values that vary go in as Fluent arguments, and the catalog decides the
//! words around them, including plural forms, which differ by language:
//! see `locales/en-US.ftl` and `locales/de-DE.ftl`.
//!
//! Bind the message id and its arguments, not a formatted string. A `Text`
//! resolves its message when it draws, so when the system language changes
//! the whole view re-renders in the new one with no rebuild. Here the count
//! is followed with `cx.value` and only the `Text` showing it is rebuilt,
//! with the new argument, when the count changes.
//!
//! `tilo-app add locale de-DE` added the German catalog and registered it in
//! `src/locales.rs`. The system picks the catalog matching the user's
//! language and falls back to en-US, message by message.

/// The application id.
pub const APP_ID: &str = "so.tilo.example.localized";
/// The window and launcher title.
pub const APP_TITLE: &str = "Localized";

pub mod locales;

fn install_catalog() {
    locales::install();
}

use ui::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Shelf {
    pub reader: String,
    pub books: u32,
}

#[resource(kind = "example.shelf", label = "Shelf")]
impl Shelf {
    /// Set the reader's name.
    #[action]
    pub fn set_reader(&mut self, #[param("Reader's name")] reader: String) {
        self.reader = reader;
    }

    /// Put a book on the shelf.
    #[action]
    pub fn add_book(&mut self) -> Result<(), &'static str> {
        self.books = self.books.checked_add(1).ok_or("the shelf is full")?;
        Ok(())
    }

    /// Take a book off the shelf.
    #[action]
    pub fn remove_book(&mut self) -> Result<(), &'static str> {
        self.books = self.books.checked_sub(1).ok_or("the shelf is empty")?;
        Ok(())
    }
}

fn greeting(reader: &str) -> Element {
    let text = if reader.trim().is_empty() {
        Text::new("localized-greeting-anonymous")
    } else {
        Text::new("localized-greeting").with_arg("reader", reader.trim())
    };
    Box::new(text.with_id("greeting").with_role(TextRole::Heading))
}

fn book_count(books: u32) -> Element {
    Box::new(
        Text::new("localized-books")
            .with_arg("count", books)
            .with_id("book-count"),
    )
}

pub struct App;

impl ResourceApp for App {
    type Resource = Shelf;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn view(cx: &mut ViewCx<Shelf>) -> Element {
        install_catalog();
        let reader = cx
            .input_with(|shelf| shelf.reader.clone(), Shelf::SET_READER)
            .with_id("reader-input")
            .with_placeholder("localized-reader-placeholder");
        let name = cx.value(|shelf| shelf.reader.clone());
        let books = cx.value(|shelf| shelf.books);
        let actions = cx.actions();
        let add = actions.bind_invocation(
            Button::new("add-book", "localized-add"),
            Shelf::ADD_BOOK.invoke(),
        );
        let remove = actions.bind_invocation(
            Button::new("remove-book", "localized-remove"),
            Shelf::REMOVE_BOOK.invoke(),
        );
        Box::new(ui::column![
            Text::new("localized-title").with_role(TextRole::Title),
            labeled_field("localized-reader-label", reader),
            ModelView::from_state(&name, |name| greeting(name)),
            ModelView::from_state(&books, |books| book_count(*books)),
            ui::row![remove, add],
            Text::new("localized-hint").with_role(TextRole::Caption),
        ])
    }
}

ui::dev_reload_entry!(App);
