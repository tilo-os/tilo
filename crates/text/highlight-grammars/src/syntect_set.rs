//! The full grammar set: two-face's extended syntax set (the `bat`
//! project's curation: the defaults plus TOML, TypeScript, Dockerfile and
//! more) run through `syntect`.

use std::any::Any;
use std::sync::OnceLock;

use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use tilo_highlight::{Grammar, Grammars, LineState, Span, TokenKind};

/// Grammars match lines without their trailing `\n`, which is how every
/// caller already splits its text.
fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_no_newlines)
}

pub(crate) static GRAMMARS: SyntectGrammars = SyntectGrammars;

pub(crate) struct SyntectGrammars;

impl SyntectGrammars {
    fn syntax(grammar: Grammar) -> &'static SyntaxReference {
        let set = syntax_set();
        set.syntaxes()
            .get(grammar.0)
            .unwrap_or_else(|| set.find_syntax_plain_text())
    }

    fn grammar_of(syntax: &SyntaxReference) -> Option<Grammar> {
        syntax_set()
            .syntaxes()
            .iter()
            .position(|known| std::ptr::eq(known, syntax))
            .map(Grammar)
    }
}

impl Grammars for SyntectGrammars {
    fn find(&self, token: &str) -> Option<Grammar> {
        let set = syntax_set();
        let syntax = set
            .find_syntax_by_token(token)
            .or_else(|| set.find_syntax_by_extension(token))?;
        Self::grammar_of(syntax)
    }

    fn plain_text(&self) -> Grammar {
        Self::grammar_of(syntax_set().find_syntax_plain_text()).unwrap_or(Grammar(0))
    }

    fn name(&self, grammar: Grammar) -> &'static str {
        &Self::syntax(grammar).name
    }

    fn start(&self, grammar: Grammar) -> Box<dyn LineState> {
        Box::new(SyntaxState {
            parse: ParseState::new(Self::syntax(grammar)),
            scopes: ScopeStack::new(),
        })
    }
}

/// Map a scope stack to the kind of its innermost classified scope, or `None`
/// for body text.
fn scope_kind(scopes: &ScopeStack) -> Option<TokenKind> {
    scopes.as_slice().iter().rev().find_map(|scope| {
        let name = scope.build_string();
        let is = |prefix: &str| name.starts_with(prefix);
        if is("comment") {
            Some(TokenKind::Comment)
        } else if is("string") {
            Some(TokenKind::StringLit)
        } else if is("constant.character") {
            Some(TokenKind::CharLit)
        } else if is("constant.numeric") {
            Some(TokenKind::Number)
        } else if is("constant.language") {
            Some(TokenKind::Constant)
        } else if is("entity.name.function")
            || is("support.function")
            || is("meta.function-call")
            || is("variable.function")
        {
            Some(TokenKind::Function)
        } else if is("entity.name.type")
            || is("entity.name.class")
            || is("entity.name.struct")
            || is("entity.name.enum")
            || is("entity.name.trait")
            || is("support.type")
            || is("support.class")
        {
            Some(TokenKind::Type)
        } else if is("keyword.operator") || is("punctuation.accessor") {
            Some(TokenKind::Operator)
        } else if is("keyword") || is("storage.type") || is("storage.modifier") {
            // `storage.type` names storage keywords (`let`, `fn`, `struct`) in
            // most grammars; genuine type names carry the scopes above.
            Some(TokenKind::Keyword)
        } else if is("entity.name.tag") || is("punctuation.definition.tag") {
            Some(TokenKind::Tag)
        } else if is("entity.name.macro")
            || is("support.macro")
            || is("meta.attribute")
            || is("meta.annotation")
            || is("entity.other.attribute-name")
            || is("punctuation.definition.annotation")
        {
            Some(TokenKind::Attribute)
        } else {
            None
        }
    })
}

/// A grammar's parse position between two lines.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SyntaxState {
    parse: ParseState,
    scopes: ScopeStack,
}

impl LineState for SyntaxState {
    fn highlight_line(&mut self, line: &str) -> Vec<Span> {
        let mut spans = Vec::new();
        let Ok(ops) = self.parse.parse_line(line, syntax_set()) else {
            if !line.is_empty() {
                spans.push(Span::new(0..line.len(), TokenKind::Text));
            }
            return spans;
        };
        let mut start = 0usize;
        let mut kind = scope_kind(&self.scopes).unwrap_or(TokenKind::Text);
        for (offset, op) in ops {
            let offset = offset.min(line.len());
            push_run(
                &mut spans,
                start,
                offset,
                word_operator(kind, line, start, offset),
            );
            start = start.max(offset);
            if self.scopes.apply(&op).is_err() {
                continue;
            }
            kind = scope_kind(&self.scopes).unwrap_or(TokenKind::Text);
        }
        push_run(
            &mut spans,
            start,
            line.len(),
            word_operator(kind, line, start, line.len()),
        );
        spans
    }

    fn boxed_clone(&self) -> Box<dyn LineState> {
        Box::new(self.clone())
    }

    fn same(&self, other: &dyn LineState) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| other == self)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An operator spelled as a word (Dockerfile's `AS`, SQL's `AND`, Python's
/// `not`) reads as a keyword, and is coloured as one.
fn word_operator(kind: TokenKind, line: &str, start: usize, end: usize) -> TokenKind {
    if kind != TokenKind::Operator {
        return kind;
    }
    let word = line.get(start..end).unwrap_or("").trim();
    if !word.is_empty() && word.chars().all(|c| c.is_alphanumeric() || c == '_') {
        TokenKind::Keyword
    } else {
        kind
    }
}

fn push_run(spans: &mut Vec<Span>, start: usize, end: usize, kind: TokenKind) {
    if start >= end {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.kind == kind
        && last.range.end == start
    {
        last.range.end = end;
        return;
    }
    spans.push(Span::new(start..end, kind));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammars_are_found_named_and_compared_by_position() {
        let grammars = &SyntectGrammars;
        let plain = grammars.plain_text();
        assert_eq!(grammars.name(plain), "Plain Text");
        assert!(grammars.find("no-such-language").is_none());
        assert!(grammars.find("rs").is_some());
        let start = grammars.start(plain);
        let mut moved = start.boxed_clone();
        assert!(start.same(moved.as_ref()));
        assert_eq!(
            moved.highlight_line("words"),
            [Span::new(0..5, TokenKind::Text)]
        );
    }
}
