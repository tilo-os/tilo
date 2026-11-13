//! Tilo's grammar set: Sublime syntax definitions run through `syntect`, used purely
//! as a tokenizer. Each span's scope stack (`source.rust → meta.function →
//! entity.name.function`) maps to one [`TokenKind`]; colours stay with the renderer
//! and the OS theme.
//!
//! One process-wide syntax set backs every caller: the extended set curated by the
//! `bat` project. It is the packed set Typst loads for raw blocks, taken from the
//! highlight engine, so a process holding both maps those bytes once.
//!
//! Without `syntax-definitions` (the browser toolkit core) no grammars run: syntect
//! and its regex engines are not linked, and every language is plain text.
//!
//! The toolkit reaches it as a [`tilo_highlight::Grammars`] through [`grammars`].
//!
//! [`TokenKind`]: tilo_highlight::TokenKind

use tilo_highlight::Grammars;

#[cfg(feature = "syntax-definitions")]
mod syntect_set;

#[must_use]
pub fn grammars() -> &'static dyn Grammars {
    #[cfg(feature = "syntax-definitions")]
    return &syntect_set::GRAMMARS;
    #[cfg(not(feature = "syntax-definitions"))]
    return &plain::PlainText;
}

#[cfg(any(test, not(feature = "syntax-definitions")))]
mod plain {
    use std::any::Any;

    use tilo_highlight::{Grammar, Grammars, LineState, Span, TokenKind};

    pub(crate) struct PlainText;

    impl Grammars for PlainText {
        fn find(&self, token: &str) -> Option<Grammar> {
            (token.eq_ignore_ascii_case("txt") || token.eq_ignore_ascii_case("plain text"))
                .then_some(Grammar(0))
        }

        fn plain_text(&self) -> Grammar {
            Grammar(0)
        }

        fn name(&self, _grammar: Grammar) -> &'static str {
            "Plain Text"
        }

        fn start(&self, _grammar: Grammar) -> Box<dyn LineState> {
            Box::new(PlainState)
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct PlainState;

    impl LineState for PlainState {
        fn highlight_line(&mut self, line: &str) -> Vec<Span> {
            if line.is_empty() {
                Vec::new()
            } else {
                vec![Span::new(0..line.len(), TokenKind::Text)]
            }
        }

        fn boxed_clone(&self) -> Box<dyn LineState> {
            Box::new(*self)
        }

        fn same(&self, other: &dyn LineState) -> bool {
            other.as_any().is::<Self>()
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn plain_text_is_one_run_per_line() {
            let grammars = &PlainText;
            let plain = grammars.plain_text();
            assert_eq!(grammars.name(plain), "Plain Text");
            assert_eq!(grammars.find("TXT"), Some(plain));
            assert!(grammars.find("rs").is_none());
            let start = grammars.start(plain);
            let mut moved = start.boxed_clone();
            assert!(start.same(moved.as_ref()));
            assert_eq!(
                moved.highlight_line("words"),
                [Span::new(0..5, TokenKind::Text)]
            );
            assert!(moved.highlight_line("").is_empty());
        }
    }
}
