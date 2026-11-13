//! What a tilolang program's source is read as: the tokens it is made of, the spans they cover,
//! the diagnostics, and the literals a token holds. An editor colours source with this same lexer,
//! so its colours never disagree with what the language reads.

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
