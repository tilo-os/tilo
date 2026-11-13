//! What a tilolang program's source is read as: the tokens it is made of, the spans they cover,
//! the diagnostics reported where a line is not the language, and the literals a token holds.
//!
//! The compiler (`tilolang`) parses these into a syntax tree and checks it. An editor colours a
//! tilolang program with this same lexer, so the colours a person reads never disagree with what
//! the language reads.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::doc_markdown,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::indexing_slicing,
    clippy::many_single_char_names,
    clippy::module_name_repetitions,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::too_many_lines,
    clippy::format_push_string,
    reason = "a lexer over source text: offsets are bounded by the text's length, every step \
              walks one byte, and a line that is not the language is a diagnostic"
)]

pub mod calendar;
pub mod diag;
pub mod duration;
pub mod lexer;

pub use diag::{Diagnostic, Span};
pub use lexer::RefLit;
