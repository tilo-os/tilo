//! The toolkit's grammar-backed [`Highlighter`]: a [`tilo_highlight`]
//! grammar set (syntect over two-face's grammars), each span's scope mapped
//! to one [`TokenKind`]; colours stay with the renderer and the OS
//! [`Theme`], never with a syntect colour theme.
//!
//! The grammar set is not part of the shared runtime every app maps: with
//! `lazy-highlight` (the runtime's build, turned on by the engine library's
//! own build) it comes from `libtilo_highlight_engine.so`, opened the first
//! time code is highlighted; otherwise it is linked
//! (`tilo-highlight-grammars`). If the library does not open, code
//! highlights as plain text.
//!
//! [`Theme`]: crate::theme::Theme

use std::cell::RefCell;

use tilo_highlight::{Grammar, Grammars, LineState};

use super::{Highlighter, HlState, LineSpans, Span, TokenKind};

/// The grammar set: linked.
#[cfg(not(feature = "lazy-highlight"))]
fn grammars() -> Option<&'static dyn Grammars> {
    Some(tilo_highlight_grammars::grammars())
}

/// The grammar set, from its engine library: opened the first time, kept.
#[cfg(feature = "lazy-highlight")]
fn grammars() -> Option<&'static dyn Grammars> {
    static GRAMMARS: std::sync::OnceLock<Option<&'static dyn Grammars>> =
        std::sync::OnceLock::new();
    *GRAMMARS.get_or_init(|| {
        // SAFETY: the engine's entry is checked against `GrammarsEntry`
        // where it is defined (crates/runtime/engines/highlight), in the
        // same build.
        let entry = unsafe {
            tilo_engine_loader::entry::<tilo_highlight::GrammarsEntry>(
                tilo_highlight::ENGINE_LIBRARY,
                tilo_highlight::ENGINE_ENTRY,
            )
        };
        match entry {
            Ok(entry) => Some(entry()),
            Err(error) => {
                tracing::error!(%error, "code highlights as plain text");
                None
            }
        }
    })
}

/// A grammar's parse position between two lines: everything multi-line
/// constructs (block comments, strings, heredocs) need to continue.
#[derive(Debug)]
pub struct SyntaxState(Option<Box<dyn LineState>>);

impl Clone for SyntaxState {
    fn clone(&self) -> Self {
        Self(self.0.as_ref().map(|state| state.boxed_clone()))
    }
}

impl PartialEq for SyntaxState {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(state), Some(other)) => state.same(other.as_ref()),
            (None, None) => true,
            _ => false,
        }
    }
}

impl Eq for SyntaxState {}

impl SyntaxState {
    /// Tokenize one line (no `\n`) and advance to the next. The returned spans
    /// cover the whole line in order; unclassified runs are
    /// [`TokenKind::Text`] and adjacent runs of one kind are merged.
    pub fn highlight_line(&mut self, line: &str) -> Vec<Span> {
        match &mut self.0 {
            Some(state) => state.highlight_line(line),
            None if line.is_empty() => Vec::new(),
            None => vec![Span::new(0..line.len(), TokenKind::Text)],
        }
    }
}

/// A grammar-backed [`Highlighter`] for one language.
///
/// As a [`Highlighter`], a pass starts at carry `0` and each line's carry is
/// only meaningful to the line after it within that pass: carries index the
/// parse states recorded since the last carry-`0` call. Callers keeping their
/// own caches use [`SyntaxHighlighter::start`] and [`SyntaxState`] directly.
pub struct SyntaxHighlighter {
    /// `None` when no grammar set is available: plain text.
    grammar: Option<Grammar>,
    pass: RefCell<Vec<SyntaxState>>,
}

impl SyntaxHighlighter {
    /// Look a grammar up by file extension (`rs`, `toml`) or by
    /// case-insensitive name (`Rust`, `Dockerfile`), the way a Markdown fence
    /// tag names its language. `None` when no grammar matches.
    pub fn for_token(token: &str) -> Option<Self> {
        let grammar = grammars()?.find(token)?;
        Some(Self::with_grammar(Some(grammar)))
    }

    /// Plain text: every line is a single [`TokenKind::Text`] run.
    pub fn plain_text() -> Self {
        Self::with_grammar(grammars().map(|grammars| grammars.plain_text()))
    }

    /// [`SyntaxHighlighter::for_token`], falling back to plain text.
    pub fn for_token_or_plain(token: &str) -> Self {
        Self::for_token(token).unwrap_or_else(Self::plain_text)
    }

    fn with_grammar(grammar: Option<Grammar>) -> Self {
        Self {
            grammar,
            pass: RefCell::new(Vec::new()),
        }
    }

    /// The grammar's display name (`Rust`, `Plain Text`).
    pub fn name(&self) -> &'static str {
        match (grammars(), self.grammar) {
            (Some(grammars), Some(grammar)) => grammars.name(grammar),
            _ => "Plain Text",
        }
    }

    /// The parse state at the start of a document.
    pub fn start(&self) -> SyntaxState {
        SyntaxState(match (grammars(), self.grammar) {
            (Some(grammars), Some(grammar)) => Some(grammars.start(grammar)),
            _ => None,
        })
    }
}

impl Highlighter for SyntaxHighlighter {
    fn spans(&self, line: &str, carry: HlState) -> LineSpans {
        let mut pass = self.pass.borrow_mut();
        if carry == 0 {
            pass.clear();
        }
        let incoming = usize::try_from(carry)
            .ok()
            .and_then(|carry| carry.checked_sub(1))
            .and_then(|index| pass.get(index));
        let mut state = incoming.cloned().unwrap_or_else(|| self.start());
        let spans = state
            .highlight_line(line)
            .into_iter()
            .filter(|span| span.kind != TokenKind::Text)
            .collect();
        // Most lines leave the parse where it was; reuse that entry rather
        // than recording one state per line.
        let carry = if incoming.is_some_and(|incoming| *incoming == state) {
            carry
        } else {
            pass.push(state);
            HlState::try_from(pass.len()).unwrap_or(0)
        };
        LineSpans { spans, carry }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(line: &str, kind: TokenKind, spans: &[Span]) -> Vec<String> {
        spans
            .iter()
            .filter(|span| span.kind == kind)
            .filter_map(|span| line.get(span.range.clone()))
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn grammars_cover_app_languages_and_spans_tile_each_line() {
        fn every_language_the_apps_name_has_a_grammar() {
            for token in [
                "rs",
                "sql",
                "toml",
                "md",
                "json",
                "yaml",
                "py",
                "js",
                "ts",
                "html",
                "css",
                "sh",
                "c",
                "cpp",
                "go",
                "Dockerfile",
                "diff",
            ] {
                let hl = SyntaxHighlighter::for_token(token);
                assert!(hl.is_some(), "no grammar for {token}");
            }
            assert!(SyntaxHighlighter::for_token("no-such-language").is_none());
        }

        fn spans_cover_the_whole_line_in_order() {
            let hl = SyntaxHighlighter::for_token_or_plain("rs");
            let line = "let s = \"hi\"; // note";
            let spans = hl.start().highlight_line(line);
            let mut end = 0;
            for span in &spans {
                assert_eq!(span.range.start, end);
                end = span.range.end;
            }
            assert_eq!(end, line.len());
        }

        fn plain_text_has_no_classified_spans() {
            let hl = SyntaxHighlighter::plain_text();
            assert!(hl.spans("fn main() {}", 0).spans.is_empty());
            assert_eq!(
                hl.start().highlight_line("fn main() {}"),
                [Span::new(0..12, TokenKind::Text)]
            );
        }

        fn unchanged_parse_state_reuses_its_carry() {
            let hl = SyntaxHighlighter::for_token_or_plain("rs");
            let first = hl.spans("let a = 1;", 0);
            let second = hl.spans("let b = 2;", first.carry);
            assert_eq!(first.carry, second.carry);
            assert_eq!(hl.pass.borrow().len(), 1);
        }

        every_language_the_apps_name_has_a_grammar();
        spans_cover_the_whole_line_in_order();
        plain_text_has_no_classified_spans();
        unchanged_parse_state_reuses_its_carry();
    }

    #[test]
    fn language_classifiers_categorize_tokens_and_carry_comments() {
        fn rust_categories() {
            let hl = SyntaxHighlighter::for_token_or_plain("rust");
            let line = "fn main() { let x = 42; let s = \"hi\"; } // done";
            let spans = hl.spans(line, 0).spans;
            assert!(text_of(line, TokenKind::Keyword, &spans).contains(&"fn".to_owned()));
            assert!(text_of(line, TokenKind::Keyword, &spans).contains(&"let".to_owned()));
            assert_eq!(text_of(line, TokenKind::Function, &spans), ["main"]);
            assert_eq!(text_of(line, TokenKind::Number, &spans), ["42"]);
            assert_eq!(text_of(line, TokenKind::StringLit, &spans), ["\"hi\""]);
            assert_eq!(text_of(line, TokenKind::Comment, &spans), ["// done"]);
            assert!(text_of(line, TokenKind::Operator, &spans).contains(&"=".to_owned()));
        }

        fn block_comment_carries_across_lines() {
            let hl = SyntaxHighlighter::for_token_or_plain("rs");
            let first = hl.spans("code(); /* open", 0);
            assert_ne!(first.carry, 0);
            let second = hl.spans("still comment", first.carry);
            assert!(second.spans.iter().all(|s| s.kind == TokenKind::Comment));
            let third = hl.spans("*/ done();", second.carry);
            assert_eq!(
                third.spans.first().map(|s| s.kind),
                Some(TokenKind::Comment)
            );
            assert!(third.spans.iter().any(|s| s.kind == TokenKind::Function));
            // A fresh pass starts over rather than inheriting the comment.
            let restart = hl.spans("still code", 0);
            assert!(restart.spans.iter().all(|s| s.kind != TokenKind::Comment));
        }

        fn sql_keywords_are_case_insensitive() {
            let hl = SyntaxHighlighter::for_token_or_plain("sql");
            let line = "select name from users where id = 'it''s'";
            let spans = hl.spans(line, 0).spans;
            let keywords = text_of(line, TokenKind::Keyword, &spans);
            for keyword in ["select", "from", "where"] {
                assert!(keywords.contains(&keyword.to_owned()), "{keywords:?}");
            }
            assert!(spans.iter().any(|s| s.kind == TokenKind::StringLit));
        }

        fn toml_and_dockerfile_are_classified() {
            let toml = SyntaxHighlighter::for_token_or_plain("toml");
            let line = "name = \"tilo\" # comment";
            let spans = toml.spans(line, 0).spans;
            assert_eq!(text_of(line, TokenKind::StringLit, &spans), ["\"tilo\""]);
            assert_eq!(text_of(line, TokenKind::Comment, &spans), ["# comment"]);

            let docker = SyntaxHighlighter::for_token_or_plain("Dockerfile");
            let line = "FROM fedora:44";
            let spans = docker.spans(line, 0).spans;
            assert_eq!(text_of(line, TokenKind::Keyword, &spans), ["FROM"]);
        }

        rust_categories();
        block_comment_carries_across_lines();
        sql_keywords_are_case_insensitive();
        toml_and_dockerfile_are_classified();
    }
}
