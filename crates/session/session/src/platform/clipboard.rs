//! The clipboard, as a role hands the person something to paste somewhere else.

use std::time::Duration;

/// Where a role puts what it hands the person to paste.
pub trait Clipboard {
    /// Put `text` on the clipboard, for no longer than `kept_for`.
    fn copy(&self, text: String, kept_for: Duration);
}
