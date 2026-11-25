//! The wrapped text layouts a frame built: what was laid out, at which width, onto how many rows.
//!
//! A retained layout is keyed by the width it wrapped to, so a layout made at a width the page
//! never shows is work thrown away — and a layout at a width no glyph fits in wraps every
//! character onto a row of its own, which for a page's prose is thousands of rows. Counting the
//! layouts and the widths they were asked for reads that cost off, rather than timing it.

/// One layout of a wrapped text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LaidOutText {
    /// The width the text wrapped to, in logical pixels.
    pub width: f32,
    /// How many bytes of text the layout holds: which text it was.
    pub bytes: usize,
    /// The rows the text wrapped onto.
    pub rows: usize,
}

thread_local! {
    static LAID_OUT: std::cell::RefCell<Vec<LaidOutText>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Note one layout of `bytes` of text, wrapped to `width` onto `rows` rows.
pub(crate) fn note(width: f32, bytes: usize, rows: usize) {
    LAID_OUT.with(|laid_out| {
        laid_out
            .borrow_mut()
            .push(LaidOutText { width, bytes, rows });
    });
}

/// The texts laid out on this thread since the last call, and a fresh recording starts here.
pub fn take() -> Vec<LaidOutText> {
    LAID_OUT.with(|laid_out| std::mem::take(&mut *laid_out.borrow_mut()))
}
