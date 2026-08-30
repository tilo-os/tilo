//! Syntax highlighting.
//!
//! We use [`syntect`] strictly as a *tokenizer*: its Sublime-syntax grammars
//! classify every span of a line into a scope stack (e.g.
//! `source.rust → meta.function → entity.name.function`). We do **not** use
//! syntect's own colour themes — colours in qOS are OS-controlled, so we map
//! each scope to one of a small set of semantic [`TokenRole`]s, and the role
//! is resolved to a concrete colour against the active [`ui::theme::Theme`] at
//! draw time. That keeps highlighting robust (real grammars) while letting the
//! OS theme own every pixel's colour.
//!
//! The highlighter is line-oriented: [`Highlighter::highlight_line`] returns
//! the spans for a single line given the parse state carried over from the
//! previous line, which is exactly what a viewport-culled editor needs (only
//! visible lines are re-tokenised).

use std::sync::OnceLock;

use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use ui::theme::Theme;
use vello::peniko::Color;

use crate::model::Language;

/// Semantic colour roles. A grammar scope maps to exactly one of these; the
/// role then resolves to a Theme colour. Deliberately small — this is the
/// vocabulary of "kinds of token", not a literal palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenRole {
    /// Default body text (identifiers, punctuation, operators).
    Text,
    /// Language keywords: `fn`, `let`, `if`, `return`, `impl`, ...
    Keyword,
    /// Type names, structs, enums, traits, classes.
    Type,
    /// Function / method names at definition or call site.
    Function,
    /// String and char literals.
    String,
    /// Numeric literals, booleans, language constants.
    Number,
    /// Comments and doc comments.
    Comment,
    /// Macros, attributes, annotations, decorators, preprocessor.
    Macro,
    /// Operators and structural punctuation that deserve a tint.
    Operator,
}

impl TokenRole {
    /// Resolve to a concrete colour against the active theme. Every colour is
    /// derived from theme tokens — nothing is hard-coded — so a theme swap
    /// recolours the whole editor coherently.
    pub fn color(self, theme: &Theme) -> Color {
        let c = &theme.tokens.colors;
        match self {
            TokenRole::Text => c.text_primary,
            // Keywords lean on the accent ramp (indigo by default).
            TokenRole::Keyword => c.accent[6],
            // Types: a lighter accent so they read distinct from keywords.
            TokenRole::Type => c.accent[7],
            // Functions: the info/blue semantic colour.
            TokenRole::Function => c.semantic.info,
            // Strings: success/green.
            TokenRole::String => c.semantic.success,
            // Numbers / constants: warning/amber.
            TokenRole::Number => c.semantic.warning,
            // Comments: dimmed text.
            TokenRole::Comment => c.text_dim,
            // Macros / attributes: danger/red for visual pop.
            TokenRole::Macro => c.semantic.danger,
            // Operators: a mid neutral so they recede slightly.
            TokenRole::Operator => c.neutral[7],
        }
    }
}

/// Map a syntect scope-stack to a [`TokenRole`]. syntect scopes are
/// dot-delimited and hierarchical; we test for the most specific meaningful
/// prefix first. This covers the common TextMate/Sublime scope vocabulary
/// shared across grammars, so a single mapping serves every language.
fn role_for_scope(scope_str: &str) -> TokenRole {
    // The scope stack renders as space-separated scopes, innermost last; we
    // scan from the most-specific (rightmost) scope outward.
    for scope in scope_str.split_whitespace().rev() {
        if scope.starts_with("comment") {
            return TokenRole::Comment;
        }
        if scope.starts_with("string") || scope.starts_with("constant.character") {
            return TokenRole::String;
        }
        if scope.starts_with("constant.numeric") || scope.starts_with("constant.language") {
            return TokenRole::Number;
        }
        if scope.starts_with("entity.name.function")
            || scope.starts_with("support.function")
            || scope.starts_with("meta.function-call")
            || scope.starts_with("variable.function")
        {
            return TokenRole::Function;
        }
        if scope.starts_with("entity.name.type")
            || scope.starts_with("entity.name.class")
            || scope.starts_with("entity.name.struct")
            || scope.starts_with("entity.name.enum")
            || scope.starts_with("entity.name.trait")
            || scope.starts_with("support.type")
            || scope.starts_with("support.class")
        {
            return TokenRole::Type;
        }
        // `storage.type` covers BOTH concrete type names in some grammars and
        // storage keywords like `let`/`const`/`fn`/`struct` in others. We bias
        // it toward Keyword: the dedicated `entity.name.type` / `support.type`
        // scopes above already catch genuine type *names*, so what's left under
        // `storage.type` reads as a keyword to the user.
        if scope.starts_with("keyword")
            || scope.starts_with("storage.type")
            || scope.starts_with("storage.modifier")
        {
            return TokenRole::Keyword;
        }
        if scope.starts_with("entity.name.macro")
            || scope.starts_with("support.macro")
            || scope.starts_with("meta.attribute")
            || scope.starts_with("meta.annotation")
            || scope.starts_with("entity.name.tag")
            || scope.starts_with("punctuation.definition.annotation")
        {
            return TokenRole::Macro;
        }
        if scope.starts_with("keyword.operator") || scope.starts_with("punctuation.accessor") {
            return TokenRole::Operator;
        }
    }
    TokenRole::Text
}

/// A contiguous run of characters that share one role. Offsets are byte
/// offsets into the source line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub role: TokenRole,
}

/// Process-wide syntax set, loaded once. The bundled `default_newlines` set
/// covers every grammar we declare in [`Language`].
fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// A line-oriented highlighter bound to one language. Cheap to construct; the
/// heavy syntax set is shared via [`syntax_set`].
pub struct Highlighter {
    syntax: &'static SyntaxReference,
}

impl Highlighter {
    /// Build a highlighter for `language`, falling back to plain text if the
    /// grammar can't be found.
    pub fn for_language(language: Language) -> Self {
        let set = syntax_set();
        let syntax = set
            .find_syntax_by_extension(language.syntect_extension())
            .unwrap_or_else(|| set.find_syntax_plain_text());
        Highlighter { syntax }
    }

    /// A fresh parse state for the start of a file.
    pub fn fresh_state(&self) -> LineState {
        LineState {
            parse: ParseState::new(self.syntax),
            scopes: ScopeStack::new(),
        }
    }

    /// Tokenise one line (which must include its trailing `\n` for grammars
    /// that anchor on it; callers pass the raw line + newline). Advances
    /// `state` so the next line continues the same parse (multi-line strings,
    /// block comments, etc. carry over correctly). Returns role-tagged spans
    /// covering the whole line.
    pub fn highlight_line(&self, line: &str, state: &mut LineState) -> Vec<Span> {
        let set = syntax_set();
        // `parse_line` yields (byte_offset, ScopeStackOp) deltas. We replay
        // them against a running ScopeStack to know the active scope at each
        // boundary, then coalesce equal-role runs.
        let ops = match state.parse.parse_line(line, set) {
            Ok(ops) => ops,
            Err(_) => return vec![],
        };

        let mut spans: Vec<Span> = Vec::new();
        let mut last_off = 0usize;
        let mut cur_role = role_for_scope(&scope_string(&state.scopes));

        for (off, op) in ops {
            if off > last_off {
                push_run(&mut spans, last_off, off, cur_role);
                last_off = off;
            }
            // Apply the op to advance the scope stack, then recompute role.
            let _ = state.scopes.apply(&op);
            cur_role = role_for_scope(&scope_string(&state.scopes));
        }
        // Tail of the line after the last op.
        let line_len = line.trim_end_matches('\n').len();
        if line_len > last_off {
            push_run(&mut spans, last_off, line_len, cur_role);
        }
        spans
    }
}

/// Coalescing push: extend the previous span if its role matches, else open a
/// new one. Keeps the span list compact for the renderer.
fn push_run(spans: &mut Vec<Span>, start: usize, end: usize, role: TokenRole) {
    if start >= end {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.role == role
        && last.end == start
    {
        last.end = end;
        return;
    }
    spans.push(Span { start, end, role });
}

/// Render a [`ScopeStack`] to its dotted-scope string for role matching.
fn scope_string(stack: &ScopeStack) -> String {
    let mut s = String::new();
    for scope in stack.as_slice() {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(&scope.build_string());
    }
    s
}

/// Carry-over parse state between consecutive lines.
#[derive(Clone)]
pub struct LineState {
    parse: ParseState,
    scopes: ScopeStack,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_keyword_and_string_classified() {
        let hl = Highlighter::for_language(Language::Rust);
        let mut st = hl.fresh_state();
        let spans = hl.highlight_line("let s = \"hello\";\n", &mut st);
        assert!(!spans.is_empty(), "should produce spans");
        // The leading `let` is a keyword.
        let first = &spans[0];
        assert_eq!(first.start, 0);
        assert_eq!(first.role, TokenRole::Keyword);
        // Somewhere in the line there's a string span.
        assert!(spans.iter().any(|s| s.role == TokenRole::String));
    }

    #[test]
    fn comment_classified() {
        let hl = Highlighter::for_language(Language::Rust);
        let mut st = hl.fresh_state();
        let spans = hl.highlight_line("// a comment\n", &mut st);
        assert!(spans.iter().all(|s| s.role == TokenRole::Comment));
    }

    #[test]
    fn block_comment_carries_across_lines() {
        let hl = Highlighter::for_language(Language::Rust);
        let mut st = hl.fresh_state();
        let _ = hl.highlight_line("/* open\n", &mut st);
        let spans = hl.highlight_line("still comment\n", &mut st);
        assert!(
            spans.iter().any(|s| s.role == TokenRole::Comment),
            "second line of a block comment should remain a comment"
        );
    }

    #[test]
    fn plain_text_falls_back_to_text_role() {
        let hl = Highlighter::for_language(Language::PlainText);
        let mut st = hl.fresh_state();
        let spans = hl.highlight_line("just words here\n", &mut st);
        assert!(spans.iter().all(|s| s.role == TokenRole::Text));
    }
}
