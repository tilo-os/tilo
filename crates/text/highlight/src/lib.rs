//! The vocabulary of syntax highlighting.
//!
//! The toolkit's code editor colours [`Span`]s by [`TokenKind`]; a
//! [`Grammars`] set turns a line of a language into those spans. The set
//! Tilo ships (syntect over two-face's grammars, `tilo-highlight-grammars`)
//! is large, and most apps never highlight code, so the shared runtime does
//! not carry it: the toolkit opens it from `libtilo_highlight_engine.so` the
//! first time a line is highlighted ([`ENGINE_LIBRARY`], [`ENGINE_ENTRY`]).
//! This crate is what both sides share, and the only part of highlighting
//! every app maps.

use std::any::Any;
use std::fmt;
use std::ops::Range;

/// The syntactic category a run of source falls into. The editor never
/// hard-codes a colour for these — the theme's syntax palette resolves each
/// to a concrete colour, so syntax tracks the active theme in light, dark
/// and high contrast.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    Keyword,
    /// Type name (`String`, `u32`, a capitalised identifier).
    Type,
    Function,
    Number,
    StringLit,
    CharLit,
    Comment,
    Operator,
    Punctuation,
    /// `#[derive(..)]`, an HTML attribute name.
    Attribute,
    /// A markup tag name (`div` in `<div>`).
    Tag,
    /// `#include`, `#!`.
    Preprocessor,
    /// Named constant / literal keyword (`true`, `null`, `None`).
    Constant,
    Variable,
    /// Markup heading (Typst `=`, Markdown `#`).
    Heading,
    Emphasis,
    Strong,
    /// Raw / inline-code / fenced markup run.
    Raw,
    Math,
    /// Uncategorised body text (renders in the default text colour).
    Text,
}

/// A coloured run within one logical line: a byte `range` (into that line)
/// and its [`TokenKind`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub kind: TokenKind,
}

impl Span {
    #[must_use]
    pub fn new(range: Range<usize>, kind: TokenKind) -> Self {
        Self { range, kind }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Grammar(pub usize);

/// A grammar's parse position between two lines: everything multi-line
/// constructs (block comments, strings, heredocs) need to continue.
pub trait LineState: Send + Sync + fmt::Debug {
    /// Tokenize one line (no `\n`) and advance to the next. The spans cover
    /// the whole line in order; unclassified runs are [`TokenKind::Text`] and
    /// adjacent runs of one kind are merged.
    fn highlight_line(&mut self, line: &str) -> Vec<Span>;

    /// This position, to continue from separately.
    fn boxed_clone(&self) -> Box<dyn LineState>;

    /// Whether `other` is the same position: a line continues identically
    /// from either.
    fn same(&self, other: &dyn LineState) -> bool;

    /// For [`LineState::same`] to compare like with like.
    fn as_any(&self) -> &dyn Any;
}

/// A set of grammars, looked up by the names code is labelled with.
pub trait Grammars: Send + Sync {
    /// The grammar for a file extension (`rs`, `toml`) or a case-insensitive
    /// name (`Rust`, `Dockerfile`), the way a Markdown fence names its
    /// language.
    fn find(&self, token: &str) -> Option<Grammar>;

    /// Plain text: every line is one [`TokenKind::Text`] run.
    fn plain_text(&self) -> Grammar;

    /// The grammar's display name (`Rust`, `Plain Text`).
    fn name(&self, grammar: Grammar) -> &'static str;

    /// The parse position at the start of a document.
    fn start(&self, grammar: Grammar) -> Box<dyn LineState>;
}

/// The entry `libtilo_highlight_engine.so` exports: its grammar set.
pub type GrammarsEntry = fn() -> &'static dyn Grammars;

/// The engine library holding Tilo's grammar set.
pub const ENGINE_LIBRARY: &str = "libtilo_highlight_engine.so";
/// Its [`GrammarsEntry`].
pub const ENGINE_ENTRY: &str = "tilo_highlight_engine_entry";
