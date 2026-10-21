//! Reading on Tilo: one reader for every long-form text.
//!
//! Feeds' articles, Books' chapters, the Browser's reader mode and Mail's
//! messages are read through the same parts, so text reads the same
//! everywhere and is set once:
//!
//! - [`Document`]: what a reader shows, blocks of inline runs;
//! - [`extract`]: from HTML to a document (a page's article, or markup that
//!   is the text), over the tolerant parser in [`html`];
//! - [`flow`]: the layout every reader sets text with, paginated or not;
//! - [`ReaderView`]: the scrolling reader an app's pane holds;
//! - [`ReadingPreferences`] and [`ReaderPalette`]: the person's face, size,
//!   measure and paper, shared by every reader.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "text layout is bounded floating-point pixel math over finite text, and \
              character and block counts far below the integer limits"
)]

mod document;
pub mod extract;
pub mod flow;
pub mod html;
mod menu;
mod paint;
mod palette;
mod preferences;
mod view;

#[cfg(test)]
mod tests;

pub use document::{
    Baseline, Block, DEFAULT_WORDS_PER_MINUTE, Document, Footnote, Image, Span, TableCell,
    TableRow, same_words, spans_text, strip_leading_title,
};
pub use menu::{OnReadingChange, ReadingChange, reading_menu};
pub use paint::{FlowHits, FlowPaint, FlowRange, char_at, paint_flow};
pub use palette::{Paper, ReaderPalette, WARM_DARK_PAGE, WARM_LIGHT_PAGE};
pub use preferences::{
    MAX_MEASURE_CHARS, MIN_MEASURE_CHARS, Measure, READING_BODY_PX, READING_SECTION,
    READING_SERIF_FAMILY, ReadingFont, ReadingPreferences,
};
pub use view::{
    HIGHLIGHT_COLORS, Mark, Passage, ReaderEvent, ReaderGeometry, ReaderView, ReadingPlace,
    average_char_width, min_margin, reader_geometry,
};
