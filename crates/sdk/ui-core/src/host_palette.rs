//! Asking the host to open its palette from inside an app: a chord or a
//! button of the app's own (Ctrl+P for quick open, Ctrl+G for go to line)
//! that lands in the same palette as Ctrl+K, over the same published
//! actions and search entries, instead of a palette of the app's own.
//!
//! The request is latched on the UI thread and taken by the host on its
//! next turn; the newest request wins.

use std::cell::RefCell;

/// What the host palette opens to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteRequest {
    /// The palette searching for `query` (empty: everything it offers).
    Search(String),
    /// The palette on one published action, asking for its arguments.
    Action(tilo_actions::ActionAddress),
}

thread_local! {
    static PENDING: RefCell<Option<PaletteRequest>> = const { RefCell::new(None) };
}

/// Open the host palette searching for `query`.
pub fn search(query: impl Into<String>) {
    PENDING.with(|pending| *pending.borrow_mut() = Some(PaletteRequest::Search(query.into())));
}

/// Open the host palette on the published action `address`.
pub fn action(address: tilo_actions::ActionAddress) {
    PENDING.with(|pending| *pending.borrow_mut() = Some(PaletteRequest::Action(address)));
}

/// The request latched on this thread, if any, taking it.
pub fn take() -> Option<PaletteRequest> {
    PENDING.with(|pending| pending.borrow_mut().take())
}
