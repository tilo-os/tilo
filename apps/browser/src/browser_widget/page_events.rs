//! What the page's engine tells the window of its own accord.
//!
//! An engine that tells page events says when something other than an
//! action's own edit changed the page: its own script rewrote a field or
//! drew a form, or a person typed into one. The window then reads what it
//! shows of the page again, once, instead of reading on a timer:
//!
//! - the page's review, when it is open, reads the page's forms, fields and
//!   record again ([`super::page_review_view`]);
//! - the window's own read of the page's forms is due again when the engine
//!   names a form that is not the one the page publishes: one the page drew
//!   later, one that asks or offers something else, one that is gone
//!   ([`super::forms_read`]). So a form the page's own script draws is an
//!   action of the page without an actor's read.
//!
//! The engine tells of a document only once it was read or operated, and
//! the window's own read of the page's forms, made when the page has
//! loaded, is that read. What the engine says is never what a field holds:
//! that is read from the page.

use ui::webview::PageChange;

use super::BrowserWidget;
use crate::site_memory::Place;

impl BrowserWidget {
    /// The page finished loading a document: its forms are read, and its
    /// review, which may show the document before it, reads it again.
    pub(super) fn page_document_loaded(&mut self) {
        self.read_forms_again();
        self.page_review_moved();
    }

    /// The engine says the page changed a field or a form of itself, or a
    /// person typed into one.
    pub(super) fn hear_page_changed(&mut self, change: &PageChange) {
        self.page_review_moved();
        // A frame's forms are its own document's, not the page's.
        if change.frame.is_some() {
            return;
        }
        let differs = Place::of(&self.current_uri)
            .is_some_and(|place| self.forms.told_otherwise(&place, &change.forms));
        if differs {
            self.read_forms_again();
        }
    }
}
