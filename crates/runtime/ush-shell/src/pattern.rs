//! Shell patterns: `*`, `?` and bracket expressions, as `case`, `[[ == ]]`,
//! `${x#pat}` and pathname expansion read them. A quoted character is
//! always itself.

/// One character of a pattern's text, and whether it was quoted.
pub type PatChar = (char, bool);

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClassItem {
    Char(char),
    Range(char, char),
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Lit(char),
    Any,
    Star,
    Class {
        negated: bool,
        items: Vec<ClassItem>,
    },
}

/// A compiled pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    toks: Vec<Tok>,
}

impl Pattern {
    /// Compile `text`. Backslash escapes the next unquoted character.
    pub fn new(text: &[PatChar]) -> Self {
        let mut toks = Vec::new();
        let mut i = 0_usize;
        while let Some(&(c, quoted)) = text.get(i) {
            i = i.saturating_add(1);
            if quoted {
                toks.push(Tok::Lit(c));
                continue;
            }
            match c {
                '\\' => {
                    if let Some(&(next, _)) = text.get(i) {
                        toks.push(Tok::Lit(next));
                        i = i.saturating_add(1);
                    } else {
                        toks.push(Tok::Lit('\\'));
                    }
                }
                '*' => {
                    if toks.last() != Some(&Tok::Star) {
                        toks.push(Tok::Star);
                    }
                }
                '?' => toks.push(Tok::Any),
                '[' => match parse_class(text, i) {
                    Some((tok, next)) => {
                        toks.push(tok);
                        i = next;
                    }
                    None => toks.push(Tok::Lit('[')),
                },
                other => toks.push(Tok::Lit(other)),
            }
        }
        Self { toks }
    }

    /// A pattern of unquoted text.
    #[cfg(test)]
    pub fn parse(text: &str) -> Self {
        let chars: Vec<PatChar> = text.chars().map(|c| (c, false)).collect();
        Self::new(&chars)
    }

    /// Whether the pattern has anything but literal characters.
    pub fn is_literal(&self) -> bool {
        self.toks.iter().all(|t| matches!(t, Tok::Lit(_)))
    }

    /// The literal text, when [`Self::is_literal`].
    pub fn literal(&self) -> String {
        self.toks
            .iter()
            .filter_map(|t| match t {
                Tok::Lit(c) => Some(*c),
                _ => None,
            })
            .collect()
    }

    /// Whether the pattern starts with a literal `.`, which a leading dot
    /// in a file name must be matched by.
    pub fn starts_with_dot(&self) -> bool {
        self.toks.first() == Some(&Tok::Lit('.'))
    }

    /// Whether all of `s` matches.
    pub fn matches(&self, s: &str) -> bool {
        let chars: Vec<char> = s.chars().collect();
        matches_at(&self.toks, &chars)
    }

    /// Whether all of `chars` matches.
    pub fn matches_chars(&self, chars: &[char]) -> bool {
        matches_at(&self.toks, chars)
    }
}

fn parse_class(text: &[PatChar], start: usize) -> Option<(Tok, usize)> {
    let mut i = start;
    let mut negated = false;
    if let Some(&(c, false)) = text.get(i)
        && (c == '!' || c == '^')
    {
        negated = true;
        i = i.saturating_add(1);
    }
    let mut items = Vec::new();
    let mut first = true;
    loop {
        let &(c, quoted) = text.get(i)?;
        if c == ']' && !quoted && !first {
            return Some((Tok::Class { negated, items }, i.saturating_add(1)));
        }
        first = false;
        if c == '[' && !quoted && text.get(i.saturating_add(1)).map(|p| p.0) == Some(':') {
            let rest: String = text.iter().skip(i.saturating_add(2)).map(|p| p.0).collect();
            if let Some(end) = rest.find(":]") {
                let name = rest.chars().take(end).collect::<String>();
                let len = name.chars().count();
                items.push(ClassItem::Named(name));
                i = i.saturating_add(len).saturating_add(4);
                continue;
            }
        }
        let c = if c == '\\' && !quoted {
            i = i.saturating_add(1);
            text.get(i)?.0
        } else {
            c
        };
        i = i.saturating_add(1);
        if text.get(i) == Some(&('-', false))
            && let Some(&(end, _)) = text.get(i.saturating_add(1))
            && end != ']'
        {
            items.push(ClassItem::Range(c, end));
            i = i.saturating_add(2);
        } else {
            items.push(ClassItem::Char(c));
        }
    }
}

fn class_has(items: &[ClassItem], c: char) -> bool {
    items.iter().any(|item| match item {
        ClassItem::Char(x) => *x == c,
        ClassItem::Range(a, b) => *a <= c && c <= *b,
        ClassItem::Named(name) => match name.as_str() {
            "alpha" => c.is_alphabetic(),
            "digit" => c.is_ascii_digit(),
            "alnum" => c.is_alphanumeric(),
            "upper" => c.is_uppercase(),
            "lower" => c.is_lowercase(),
            "space" => c.is_whitespace(),
            "blank" => c == ' ' || c == '\t',
            "punct" => c.is_ascii_punctuation(),
            "xdigit" => c.is_ascii_hexdigit(),
            "cntrl" => c.is_control(),
            "print" => !c.is_control(),
            "graph" => !c.is_control() && c != ' ',
            "word" => c.is_alphanumeric() || c == '_',
            _ => false,
        },
    })
}

fn matches_at(toks: &[Tok], s: &[char]) -> bool {
    // Iterative matching with one backtrack point per star: linear in
    // practice, and never exponential.
    let (mut ti, mut at) = (0_usize, 0_usize);
    let mut star: Option<(usize, usize)> = None;
    loop {
        if let Some(tok) = toks.get(ti) {
            let here = s.get(at);
            let step = match (tok, here) {
                (Tok::Star, _) => {
                    star = Some((ti, at));
                    ti = ti.saturating_add(1);
                    continue;
                }
                (Tok::Lit(c), Some(h)) => c == h,
                (Tok::Any, Some(_)) => true,
                (Tok::Class { negated, items }, Some(h)) => class_has(items, *h) != *negated,
                (_, None) => false,
            };
            if step {
                ti = ti.saturating_add(1);
                at = at.saturating_add(1);
                continue;
            }
        } else if at == s.len() {
            return true;
        }
        match star {
            Some((st, si)) if si < s.len() => {
                star = Some((st, si.saturating_add(1)));
                ti = st.saturating_add(1);
                at = si.saturating_add(1);
            }
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stars_questions_and_classes() {
        assert!(Pattern::parse("*.rs").matches("main.rs"));
        assert!(!Pattern::parse("*.rs").matches("main.rc"));
        assert!(Pattern::parse("a?c").matches("abc"));
        assert!(Pattern::parse("[a-c]x").matches("bx"));
        assert!(Pattern::parse("[!a-c]x").matches("dx"));
        assert!(Pattern::parse("[[:digit:]]*").matches("9lives"));
        assert!(Pattern::parse("*a*b*").matches("xxaxxbxx"));
        assert!(Pattern::parse("\\*").matches("*"));
        assert!(!Pattern::parse("\\*").matches("x"));
        assert!(Pattern::parse("[]x]").matches("]"));
    }

    #[test]
    fn a_quoted_star_is_a_star() {
        let quoted = Pattern::new(&[('*', true)]);
        assert!(quoted.matches("*"));
        assert!(!quoted.matches("anything"));
    }
}
