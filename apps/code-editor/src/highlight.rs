//! Syntax highlighting for the editor view.
//!
//! Tokenizing is the toolkit's grammar-backed highlighter
//! ([`ui::widget::code_editor::SyntaxHighlighter`]) and colours come from the
//! theme's shared syntax palette (`theme.tokens.syntax`); this module keeps
//! what is specific to the editor view: the incremental per-line cache that
//! re-parses only the edited region.

#[cfg(test)]
use ui::widget::code_editor::TokenKind;
use ui::widget::code_editor::{SyntaxHighlighter, SyntaxState};

use crate::model::Language;

pub use ui::widget::code_editor::Span;

fn highlighter(language: Language) -> SyntaxHighlighter {
    SyntaxHighlighter::for_token_or_plain(language.grammar_token())
}

/// Tokenize one line, counting the parse in tests so cache reuse is provable.
fn highlight_line(state: &mut SyntaxState, line: &str) -> Vec<Span> {
    #[cfg(test)]
    PARSED_LINES.with(|count| count.set(count.get().saturating_add(1)));
    state.highlight_line(line)
}

/// Outgoing tokenizer state for each cached line. Cache admission includes both
/// parse and scope stacks: identical text alone cannot close a multiline edit.
#[derive(Default)]
pub(crate) struct IncrementalHighlights {
    language: Option<Language>,
    states: Vec<SyntaxState>,
}

impl IncrementalHighlights {
    /// Update spans without changing the tokenizer or its newline convention.
    /// Returns the number of lines actually tokenized (not scanned for equality).
    pub(crate) fn update(
        &mut self,
        language: Language,
        old_lines: &[String],
        lines: &[String],
        spans: &mut Vec<Vec<Span>>,
    ) -> usize {
        let reusable = self.language == Some(language)
            && self.states.len() == old_lines.len()
            && spans.len() == old_lines.len();
        let prefix = if reusable {
            old_lines
                .iter()
                .zip(lines)
                .take_while(|(a, b)| a == b)
                .count()
        } else {
            0
        };
        if reusable && prefix == old_lines.len() && prefix == lines.len() {
            return 0;
        }
        let suffix = if reusable {
            old_lines
                .iter()
                .skip(prefix)
                .rev()
                .zip(lines.iter().skip(prefix).rev())
                .take_while(|(a, b)| a == b)
                .count()
        } else {
            0
        };
        let old_suffix = old_lines.len().saturating_sub(suffix);
        let new_suffix = lines.len().saturating_sub(suffix);
        let highlighter = highlighter(language);
        let mut old_states = std::mem::take(&mut self.states);
        let mut old_spans = std::mem::take(spans);
        // Move cached prefix/suffix allocations rather than cloning every span
        // or ParseState after an otherwise constant-work single-line edit.
        self.states.extend(old_states.drain(..prefix));
        spans.extend(old_spans.drain(..prefix));
        let prefix_state = self
            .states
            .last()
            .cloned()
            .unwrap_or_else(|| highlighter.start());
        let mut state = prefix_state.clone();
        let mut parsed = 0;
        for (index, line) in lines.iter().enumerate().skip(prefix) {
            if suffix != 0 && index >= new_suffix {
                let old_index = old_suffix + (index - new_suffix);
                let offset = old_index - prefix;
                let incoming = offset
                    .checked_sub(1)
                    .and_then(|previous| old_states.get(previous))
                    .unwrap_or(&prefix_state);
                if state == *incoming {
                    self.states.extend(old_states.into_iter().skip(offset));
                    spans.extend(old_spans.into_iter().skip(offset));
                    self.language = Some(language);
                    return parsed;
                }
            }
            spans.push(highlight_line(&mut state, line));
            self.states.push(state.clone());
            parsed += 1;
        }
        self.language = Some(language);
        parsed
    }
}

#[cfg(test)]
#[path = "highlight_incremental_tests.rs"]
mod incremental_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_resolve_to_grammars_that_classify_tokens() {
        // Every language resolves to its grammar.
        for language in [
            Language::Rust,
            Language::Toml,
            Language::Markdown,
            Language::Json,
            Language::Yaml,
            Language::Python,
            Language::JavaScript,
            Language::TypeScript,
            Language::Html,
            Language::Css,
            Language::Shell,
            Language::C,
            Language::Cpp,
            Language::Go,
        ] {
            assert_ne!(
                highlighter(language).name(),
                "Plain Text",
                "{language:?} has no grammar"
            );
        }
        assert_eq!(highlighter(Language::PlainText).name(), "Plain Text");

        // Rust: keyword and string are classified.
        let mut state = highlighter(Language::Rust).start();
        let spans = state.highlight_line("let s = \"hello\";");
        let first = &spans[0];
        assert_eq!(first.range.start, 0, "rust keyword start");
        assert_eq!(first.kind, TokenKind::Keyword, "rust keyword kind");
        assert!(
            spans.iter().any(|s| s.kind == TokenKind::StringLit),
            "rust string literal"
        );

        // Plain text is one text run.
        let mut state = highlighter(Language::PlainText).start();
        let spans = state.highlight_line("just words here");
        assert!(
            spans.iter().all(|s| s.kind == TokenKind::Text),
            "plain text is all Text"
        );
    }
}

#[cfg(test)]
thread_local! {
    static PARSED_LINES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn take_parsed_line_count() -> usize {
    PARSED_LINES.with(|count| count.replace(0))
}
