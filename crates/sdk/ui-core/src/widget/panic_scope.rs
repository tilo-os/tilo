//! Which window's application code is running, for a panic hook to name.
//!
//! The browser build aborts on a panic, so an application that panics
//! cannot be caught and unwound; the page has to start again. A host that
//! knows which window was running can start it again without that window
//! (see the browser runtime's crash quarantine) instead of losing every
//! window to the same panic on each load.
//!
//! A [`Scope`] marks the window (its tab id) for as long as it lives; scopes
//! nest, and the innermost names the window. Nothing unwinds past an abort,
//! so the window running when a panic hook fires is still the current one.

use std::cell::Cell;

thread_local! {
    static CURRENT: Cell<Option<u64>> = const { Cell::new(None) };
}

/// The window whose application code is running on this thread, if any.
#[must_use]
pub fn current() -> Option<u64> {
    CURRENT.with(Cell::get)
}

/// Marks `tab` as running until dropped. `None` keeps the enclosing window.
#[must_use = "the scope ends when the guard is dropped"]
pub fn enter(tab: Option<u64>) -> Scope {
    let earlier = CURRENT.with(|current| match tab {
        Some(tab) => current.replace(Some(tab)),
        None => current.get(),
    });
    Scope { earlier }
}

/// Restores the enclosing window when dropped: see [`enter`].
pub struct Scope {
    earlier: Option<u64>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|current| current.set(self.earlier));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_innermost_window_is_current_until_its_scope_ends() {
        assert_eq!(current(), None);
        {
            let _outer = enter(Some(7));
            assert_eq!(current(), Some(7));
            {
                let _inner = enter(Some(9));
                assert_eq!(current(), Some(9));
                let _unnamed = enter(None);
                assert_eq!(current(), Some(9));
            }
            assert_eq!(current(), Some(7));
        }
        assert_eq!(current(), None);
    }
}
