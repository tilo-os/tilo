//! Clicks a test observes: a button's handler records its name, and the test
//! asks whether it was clicked.

use std::cell::RefCell;
use std::collections::HashMap;

thread_local! {
    static CLICKS: RefCell<HashMap<&'static str, usize>> = RefCell::new(HashMap::new());
}

/// A click handler that records a click named `name`.
pub(crate) fn record_click(name: &'static str) -> impl FnMut() + 'static {
    move || CLICKS.with_borrow_mut(|clicks| *clicks.entry(name).or_default() += 1)
}

/// How many clicks named `name` were recorded on this thread.
pub(crate) fn clicks(name: &'static str) -> usize {
    CLICKS.with_borrow(|clicks| clicks.get(name).copied().unwrap_or_default())
}
