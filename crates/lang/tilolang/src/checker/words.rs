//! Words a view shows (i18n.md §2): a string literal a component shows is a message of the
//! package's catalogue, an attribute of the view's own message keyed by a slug of its words,
//! resolved in the reader's language where it is drawn, and the text as written when no
//! catalogue has it.
//!
//! Only the literal a component shows is a message: the value given to a prop that shows words
//! (not one the toolkit marks as data), or what such a value takes from a branch, an option or a
//! list. A literal compared, passed to a function or used as a key stays text.
//!
//! A view's text parameter is words it shows, so the literal a caller gives it is the caller's
//! message too ([`super::template::Checker::expand`]).

use super::{Checker, Ex};
use crate::abi::Imp;
use crate::ast::{self, ExprKind, StmtKind, StrPart};
use crate::diag::Span;
use crate::ir::{Ek, Stmt};
use crate::meta::WordMeta;
use crate::types::Ty;

/// The most words of a literal its key keeps.
const SLUG_WORDS: usize = 6;

impl Checker<'_> {
    /// Marks the literals `value` shows as its result: the value itself, a branch's, an
    /// option's or a list's.
    pub(super) fn mark_shown(&mut self, value: &ast::Expr) {
        match &value.kind {
            ExprKind::Str(_) => {
                self.st.shown.insert(value.span);
            }
            ExprKind::If { then, els, .. } | ExprKind::IfLet { then, els, .. } => {
                self.mark_shown_block(then);
                if let Some(els) = els {
                    self.mark_shown(els);
                }
            }
            ExprKind::Block(block) => self.mark_shown_block(block),
            ExprKind::Match { arms, .. } => {
                for arm in arms {
                    self.mark_shown(&arm.body);
                }
            }
            ExprKind::Variant {
                name,
                args: Some(args),
            } if name.name == "some" => {
                for arg in args {
                    self.mark_shown(&arg.value);
                }
            }
            ExprKind::List(items) => {
                for item in items {
                    self.mark_shown(item);
                }
            }
            _ => {}
        }
    }

    fn mark_shown_block(&mut self, block: &ast::Block) {
        if let Some(ast::Stmt {
            kind: StmtKind::Expr(tail),
            ..
        }) = block.stmts.last()
        {
            self.mark_shown(tail);
        }
    }

    /// The declaration whose message the words being checked are attributes of: the view or
    /// action being checked, or the one [`super::FnState::words_of`] names.
    fn words_owner(&self) -> String {
        self.st
            .words_of
            .clone()
            .unwrap_or_else(|| self.st.view_id.clone())
    }

    /// `std.say("Morning: {n} done")`: words the code writes for a person to read, such as the
    /// title of a record it makes, as a message of the declaration it is written in. They read in
    /// the language of whoever runs the code, with each interpolation its argument. It is the
    /// library's word, not a bare name, so a package's own action can be called `say`.
    pub(super) fn builtin_say(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        match args {
            [ast::Arg { name: None, value }] if matches!(value.kind, ExprKind::Str(_)) => {
                self.mark_shown(value);
                self.expr_as(value, &Ty::Text)
            }
            _ => {
                self.err(
                    "E022",
                    span,
                    "`std.say` takes the words in quotes, as a person reads them: `std.say(\"Morning: {n} done\")`",
                );
                for a in args {
                    let _ = self.expr(&a.value, None);
                }
                Ex::error()
            }
        }
    }

    /// Whether the literal at `span` is one a component shows.
    pub(super) fn shows(&self, span: Span) -> bool {
        self.st.shown.contains(&span)
    }

    /// A literal a view shows: its message, with each interpolation its argument, falling back
    /// to the text as written. A literal of no words (`"{n}"`, `"·"`) is text.
    pub(super) fn words(&mut self, parts: &[StrPart]) -> Ex {
        let has_words = parts.iter().any(
            |part| matches!(part, StrPart::Lit(text) if text.chars().any(char::is_alphabetic)),
        );
        if !has_words {
            return self.string(parts);
        }
        // Each interpolation is evaluated once, into a local the message's arguments and the
        // fallback text both read.
        let mut stmts = Vec::new();
        let mut text = String::new();
        let mut args: Vec<(String, Ex)> = Vec::new();
        let mut fallback: Option<Ex> = None;
        for part in parts {
            let piece = match part {
                StrPart::Lit(lit) => {
                    text.push_str(lit);
                    Ex::str(lit)
                }
                StrPart::Interp(x) => {
                    let v = self.expr(x, None);
                    let v = self.text_of(v, x.span);
                    let local = self.fresh(Ty::Text);
                    stmts.push(Stmt::Set(local, v));
                    let name = variable(x, &args);
                    text.push('{');
                    text.push_str(&name);
                    text.push('}');
                    args.push((name, Ex::local(local, Ty::Text)));
                    Ex::local(local, Ty::Text)
                }
            };
            fallback = Some(match fallback {
                None => piece,
                Some(acc) => Ex::host(Imp::StrConcat, vec![acc, piece], Ty::Text),
            });
        }
        let key = self.word_key(&text);
        let message = format!("{}.{key}", crate::messages::key_of(&self.words_owner()));
        let list = self.text_pairs(args);
        let call = Ex::host(
            Imp::Word,
            vec![
                Ex::str(&message),
                list,
                fallback.unwrap_or_else(|| Ex::str("")),
            ],
            Ty::Text,
        );
        Ex::new(Ek::Seq(stmts, Box::new(call)), Ty::Text)
    }

    /// The attribute naming `text` under the view's message: a slug of its words, numbered
    /// when another text of the view has the same slug.
    fn word_key(&mut self, text: &str) -> String {
        let view = self.words_owner();
        let base = format!("w-{}", slug(text));
        let mut key = base.clone();
        let mut n = 1;
        loop {
            match self.words.iter().find(|w| w.view == view && w.key == key) {
                Some(w) if w.text == text => return key,
                Some(_) => {
                    n += 1;
                    key = format!("{base}-{n}");
                }
                None => break,
            }
        }
        self.words.push(WordMeta {
            view,
            key: key.clone(),
            text: text.to_string(),
        });
        key
    }
}

/// The variable an interpolation is in the message: the last name it reads (`{t.count}` is
/// `count`), numbered when the literal already has one of that name.
fn variable(x: &ast::Expr, taken: &[(String, Ex)]) -> String {
    let base = match &x.kind {
        ExprKind::Ident(name) => name.clone(),
        ExprKind::Field { name, .. } | ExprKind::OptChain { name, .. } => name.name.clone(),
        ExprKind::Call { callee, .. } => match &callee.kind {
            ExprKind::Ident(name) => name.clone(),
            ExprKind::Field { name, .. } => name.name.clone(),
            _ => "value".to_string(),
        },
        _ => "value".to_string(),
    };
    let mut name = base.clone();
    let mut n = 1;
    while taken.iter().any(|(t, _)| *t == name) {
        n += 1;
        name = format!("{base}{n}");
    }
    name
}

/// A literal's words as a key: lower case, joined by `-`, at most [`SLUG_WORDS`] of them. A
/// Fluent name is ASCII, so other letters are left out (the number after a clash keeps keys
/// apart).
fn slug(text: &str) -> String {
    let mut words = Vec::new();
    let mut in_var = false;
    let mut word = String::new();
    for c in text.chars() {
        match c {
            '{' => in_var = true,
            '}' => in_var = false,
            c if !in_var && c.is_ascii_alphanumeric() => word.push(c.to_ascii_lowercase()),
            _ => {}
        }
        if !c.is_ascii_alphanumeric() && !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words.truncate(SLUG_WORDS);
    let slug = words.join("-");
    if slug.is_empty() {
        "text".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::slug;

    #[test]
    fn a_slug_is_the_first_words_without_variables() {
        assert_eq!(slug("{count} new messages"), "new-messages");
        assert_eq!(
            slug("No conversation selected."),
            "no-conversation-selected"
        );
        assert_eq!(
            slug("Über uns, und mehr als sechs Wörter"),
            "ber-uns-und-mehr-als-sechs"
        );
        assert_eq!(slug("·"), "text");
    }
}
