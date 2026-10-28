//! Which of the page's apps declare their windows withheld from what leaves
//! the device (`ComponentBehaviour::capture_protected`), as each window's
//! component states it when the page opens it. A report never records a
//! window of such an app.

use std::cell::RefCell;
use std::collections::HashSet;

thread_local! {
    static DECLARED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// The window of `app` the page just opened declares capture protection
/// when `protected`. The declaration is the app's, so it holds for every
/// window of it from then on.
pub(crate) fn note(app: &str, protected: bool) {
    if protected {
        DECLARED.with_borrow_mut(|declared| declared.insert(app.to_owned()));
    }
}

/// Whether `app` declares its windows capture-protected.
pub(crate) fn declared(app: &str) -> bool {
    DECLARED.with_borrow(|declared| declared.contains(app))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_whose_window_declares_protection_stays_protected() {
        assert!(!declared("so.tilo.vault"));
        note("so.tilo.vault", true);
        note("so.tilo.notes", false);
        assert!(declared("so.tilo.vault"));
        assert!(!declared("so.tilo.notes"));
        note("so.tilo.vault", false);
        assert!(declared("so.tilo.vault"), "a later window does not undo it");
    }
}
