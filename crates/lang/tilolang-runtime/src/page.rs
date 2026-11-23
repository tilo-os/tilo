//! The page a window shows: `page.call`, the page's own actions on it.
//!
//! The engine belongs to the window that draws the page, not to the app's process: a call is
//! carried to that window over its own connection and answered there. The answer is an entry of
//! the invocation's journal, so a run resumed or replayed answers from it and touches no page
//! again. What a call answers is the page protocol's own answer, as JSON text; a call that could
//! not be made answers `{"error": "…"}` with the reason in words.

/// Where a page call reaches: the page the app's window shows, asked one call at a time.
pub trait PageHost {
    /// Do `request` (the page protocol's own request, as JSON text) on the page, and answer
    /// what it answered, as JSON text.
    ///
    /// # Errors
    ///
    /// Why the page could not be asked, or did not answer.
    fn call(&self, request: &str) -> Result<String, String>;
}
