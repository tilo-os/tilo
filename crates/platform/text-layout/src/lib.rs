//! Setting text into a measure, for every setter alike (the toolkit's text, the word processor's
//! sheets, a reader's pages and PDF export): where a line may break and a word hyphenate
//! ([`breaks`]), a paragraph composed into lines ([`paragraph`]), and lines kept together across
//! pages ([`paging`]). Measuring glyphs stays with the caller.

pub mod breaks;
pub mod paging;
pub mod paragraph;
