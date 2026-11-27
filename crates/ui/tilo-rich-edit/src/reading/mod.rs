//! Reading a document as a book: a page's blocks and their inline styling, the page broken by
//! measured lines, one page painted, and the component a view places to read one.
//!
//! The layout is the typesetter a reader's page needs: measured words, hyphenation in the
//! book's language where a paragraph is justified, punctuation hanging in the margin, and the
//! shared page-breaking rules (`tilo_text_layout::paging`) deciding where a page ends. It is the same
//! setting a word processor's page and a PDF export read, and nothing here knows what Tilo is.

pub mod component;
pub mod from_rich;
pub mod layout;
pub mod model;
pub mod page;

pub use component::{Reading, ReadingMode, ReadingProps};
pub use model::{Block, CompositionStyle, ReadingStyle, Span};
