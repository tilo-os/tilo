//! The lexer. Newlines are tokens because they separate statements; the lexer drops the ones
//! that cannot end a statement (inside brackets, before a leading `.`).

use crate::calendar::{check_date, check_time, check_wall_clock};
use crate::diag::{Diagnostic, Span};
use crate::duration::UNITS;

/// Words that cannot be used as names.
pub const KEYWORDS: &[&str] = &[
    "app", "use", "as", "type", "enum", "action", "view", "fn", "let", "if", "else", "match",
    "for", "in", "return", "and", "or", "not", "true", "false", "try", "suggest", "merge",
    "proposal",
];

/// Words that are keywords only where a declaration starts: `state` at the start of a
/// statement of a view body; `root`, `default`, `row` (before `view`), `scenario`,
/// `trigger`, `every`, `at`, `primary`, `destructive` and `migration` (before `action`) at the
/// start of an item;
/// `root` before a type in a scenario. Elsewhere they are ordinary names.
pub const CONTEXTUAL: &[&str] = &[
    "state",
    "root",
    "default",
    "row",
    "scenario",
    "trigger",
    "every",
    "at",
    "primary",
    "destructive",
    "migration",
];

/// `@app.Type:id@version`: a reference literal, the one piece of syntax a token carries whole
/// for the parser and everything after it to read.
#[derive(Clone, Debug, PartialEq)]
pub struct RefLit {
    /// The app part as written (an app id or an alias); empty for `@kind:id`.
    pub app: String,
    pub ty: String,
    pub id: String,
    pub version: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StrPiece {
    Lit(String),
    /// A `{ expr }` interpolation: the byte range of the expression inside the braces.
    Interp {
        start: usize,
        end: usize,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Kw(&'static str),
    Int(i64),
    Float(f64),
    /// A number with a unit suffix: `11rem`.
    Length(f64, &'static str),
    /// A whole number of a unit of time: `7d`, `5m` (search.md, "Types, references and paths").
    Duration(i64, &'static str),
    /// A calendar literal (search.md, "Types, references and paths"): a day `2026-06-29`, a
    /// month `2026-08`, an instant `2026-06-29T07:02Z`, or a wall-clock time
    /// `2026-06-29T07:02` of no zone, held in its canonical ISO text
    /// (`2026-06-29T07:02:00Z`, `2026-06-29T07:02:00`).
    Calendar(String),
    Str(Vec<StrPiece>),
    Ref(RefLit),
    Doc(String),
    Newline,
    Eof,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Dot,
    /// `..`, between the ends of a range: `a..b`, `a..`, `..b`.
    DotDot,
    Arrow,
    /// `=>`, between a match pattern and its arm.
    FatArrow,
    Assign,
    PlusAssign,
    MinusAssign,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Hash,
    /// `?.`: a field or method of an Option's value, when it has one.
    QDot,
    /// `??`: an Option's value, or a fallback.
    QQ,
    /// `|`, between the cases of a union type.
    Pipe,
}

impl Tok {
    /// How the token reads in an error message.
    pub fn describe(&self) -> String {
        match self {
            Self::Ident(n) => format!("`{n}`"),
            Self::Kw(k) => format!("`{k}`"),
            Self::Int(_) | Self::Float(_) | Self::Length(..) | Self::Duration(..) => {
                "a number".into()
            }
            Self::Calendar(_) => "a date".into(),
            Self::QDot => "`?.`".into(),
            Self::QQ => "`??`".into(),
            Self::Pipe => "`|`".into(),
            Self::Str(_) => "a string".into(),
            Self::Ref(_) => "a reference".into(),
            Self::Doc(_) => "a doc comment".into(),
            Self::Newline => "the end of the line".into(),
            Self::Eof => "the end of the file".into(),
            Self::LParen => "`(`".into(),
            Self::RParen => "`)`".into(),
            Self::LBrace => "`{`".into(),
            Self::RBrace => "`}`".into(),
            Self::LBracket => "`[`".into(),
            Self::RBracket => "`]`".into(),
            Self::Comma => "`,`".into(),
            Self::Colon => "`:`".into(),
            Self::Dot => "`.`".into(),
            Self::DotDot => "`..`".into(),
            Self::Arrow => "`->`".into(),
            Self::FatArrow => "`=>`".into(),
            Self::Assign => "`=`".into(),
            Self::PlusAssign => "`+=`".into(),
            Self::MinusAssign => "`-=`".into(),
            Self::EqEq => "`==`".into(),
            Self::NotEq => "`!=`".into(),
            Self::Lt => "`<`".into(),
            Self::Le => "`<=`".into(),
            Self::Gt => "`>`".into(),
            Self::Ge => "`>=`".into(),
            Self::Plus => "`+`".into(),
            Self::Minus => "`-`".into(),
            Self::Star => "`*`".into(),
            Self::Slash => "`/`".into(),
            Self::Percent => "`%`".into(),
            Self::Hash => "`#`".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

/// Lexes `src`, whose first byte sits at `base` in the file (non-zero for interpolations).
pub fn lex_at(src: &str, base: usize) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut lx = Lexer {
        src,
        bytes: src.as_bytes(),
        pos: 0,
        base,
        depth: 0,
        toks: Vec::new(),
        diags: Vec::new(),
        comments: Vec::new(),
    };
    lx.run();
    (lx.toks, lx.diags)
}

/// One plain `//` comment (not a `///` doc comment): where it is, and what it says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    pub span: Span,
    /// The text after the `//`, with the one space the canonical form prints taken off and no
    /// trailing space; empty for a line of `//` alone.
    pub text: String,
}

/// The plain `//` comments of `src`, in source order: the canonical form keeps them, above the
/// declaration or statement they were written over.
pub fn plain_comments(src: &str) -> Vec<Comment> {
    plain_comments_at(src, 0)
}

/// The plain `//` comments of `src`, whose spans start at `base` as [`lex_at`] places tokens.
pub fn plain_comments_at(src: &str, base: usize) -> Vec<Comment> {
    let mut lx = Lexer {
        src,
        bytes: src.as_bytes(),
        pos: 0,
        base,
        depth: 0,
        toks: Vec::new(),
        diags: Vec::new(),
        comments: Vec::new(),
    };
    lx.run();
    lx.comments
}

/// Lexes a whole file.
pub fn lex(src: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    lex_at(src, 0)
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    base: usize,
    /// Nesting depth of `(` and `[`; newlines inside are not statement ends.
    depth: usize,
    toks: Vec<Token>,
    diags: Vec<Diagnostic>,
    /// Plain `//` comments, which the canonical form keeps where they were written.
    comments: Vec<Comment>,
}

const fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

const fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

impl Lexer<'_> {
    fn at(&self, off: usize) -> Option<u8> {
        self.bytes.get(self.pos + off).copied()
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.base + start, self.base + end)
    }

    fn push(&mut self, tok: Tok, start: usize) {
        let span = self.span(start, self.pos);
        self.toks.push(Token { tok, span });
    }

    fn error(&mut self, code: &str, start: usize, end: usize, msg: impl Into<String>) {
        let span = self.span(start, end);
        self.diags.push(Diagnostic::new(code, span, msg));
    }

    fn slice(&self, start: usize, end: usize) -> &str {
        self.src.get(start..end).unwrap_or("")
    }

    fn run(&mut self) {
        while let Some(b) = self.at(0) {
            let start = self.pos;
            match b {
                b' ' | b'\t' | b'\r' => self.pos += 1,
                b'\n' => {
                    self.pos += 1;
                    self.newline(start);
                }
                b';' => {
                    self.pos += 1;
                    if self.depth == 0
                        && !matches!(self.toks.last().map(|t| &t.tok), Some(Tok::Newline) | None)
                    {
                        self.push(Tok::Newline, start);
                    }
                }
                b'/' if self.at(1) == Some(b'/') => self.comment(start),
                b'/' if self.operand_expected() => self.regex(start),
                b'"' => self.string(start),
                b'@' => self.reference(start),
                b'0'..=b'9' => self.number(start),
                _ if is_ident_start(b) => self.word(start),
                _ => self.punct(b, start),
            }
        }
        let end = self.bytes.len();
        self.pos = end;
        self.push(Tok::Eof, end);
    }

    fn newline(&mut self, start: usize) {
        if self.depth > 0 || self.toks.is_empty() {
            return;
        }
        if matches!(self.toks.last().map(|t| &t.tok), Some(Tok::Newline)) {
            return;
        }
        // A statement never ends with a comma: the line continues the list it is in, as a long
        // `if let` breaks between its binds.
        if matches!(self.toks.last().map(|t| &t.tok), Some(Tok::Comma)) {
            return;
        }
        // A line that starts `.name` continues the previous one as a method call, unless the
        // previous line ended a block or the new line is a match arm (`.name(..) =>`); a line
        // that starts a binary operator continues it too, however the line before ended.
        let ended_block = matches!(self.toks.last().map(|t| &t.tok), Some(Tok::RBrace));
        if self.next_line_continues(ended_block) {
            return;
        }
        self.push(Tok::Newline, start);
    }

    /// Whether the next line continues the previous one: its first token is a `.` or `?.` that
    /// names a field or method, or a binary operator, which a broken expression leads its
    /// continuation line with.
    fn next_line_continues(&self, ended_block: bool) -> bool {
        let mut i = self.pos;
        loop {
            match self.bytes.get(i).copied() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => i += 1,
                Some(b'/')
                    if self.bytes.get(i + 1) == Some(&b'/')
                        && self.bytes.get(i + 2) != Some(&b'/') =>
                {
                    while self.bytes.get(i).is_some_and(|b| *b != b'\n') {
                        i += 1;
                    }
                }
                Some(b'.') => {
                    return !ended_block
                        && self.bytes.get(i + 1).is_some_and(|b| is_ident_start(*b))
                        && !self.is_match_arm(i + 1);
                }
                Some(b'?') => {
                    return match self.bytes.get(i + 1) {
                        // `?.name`: a field or method of what the line before left.
                        Some(b'.') => {
                            !ended_block
                                && self.bytes.get(i + 2).is_some_and(|b| is_ident_start(*b))
                        }
                        // `??`, the value or the fallback.
                        Some(b'?') => true,
                        _ => false,
                    };
                }
                Some(b) if is_ident_start(b) => {
                    // The operators spelled as words; nothing else starts a statement.
                    let mut end = i;
                    while self.bytes.get(end).is_some_and(|b| is_ident_char(*b)) {
                        end += 1;
                    }
                    return matches!(self.bytes.get(i..end), Some(b"and" | b"or"));
                }
                Some(b) => {
                    // The symbolic operators. `-`, `not` and `/` are left out: each can begin an
                    // expression, a value or a regular expression of its own.
                    return matches!(b, b'+' | b'*' | b'%' | b'=' | b'!' | b'<' | b'>');
                }
                None => return false,
            }
        }
    }

    /// Whether the name starting at `from` is followed (after an optional parenthesised list)
    /// by `=>`: the line is a match arm, not a method call.
    fn is_match_arm(&self, from: usize) -> bool {
        let at = |i: usize| self.bytes.get(i).copied();
        let mut i = from;
        while at(i).is_some_and(is_ident_char) {
            i += 1;
        }
        let skip_ws = |mut i: usize| {
            while matches!(at(i), Some(b' ' | b'\t')) {
                i += 1;
            }
            i
        };
        i = skip_ws(i);
        if at(i) == Some(b'(') {
            let mut depth = 0usize;
            loop {
                match at(i) {
                    Some(b'(') => depth += 1,
                    Some(b')') => {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    Some(b'\n') | None => return false,
                    _ => {}
                }
                i += 1;
            }
            i = skip_ws(i);
        }
        at(i) == Some(b'=') && at(i + 1) == Some(b'>')
    }

    fn comment(&mut self, start: usize) {
        let doc = self.at(2) == Some(b'/') && self.at(3) != Some(b'/');
        while self.at(0).is_some_and(|b| b != b'\n') {
            self.pos += 1;
        }
        if !doc {
            let span = self.span(start, self.pos);
            // As a doc comment: one space after the marker, and none at the end.
            let text = self.slice(start + 2, self.pos);
            let text = text.strip_prefix(' ').unwrap_or(text).trim_end().to_owned();
            self.comments.push(Comment { span, text });
        }
        if doc {
            let text = self.slice(start + 3, self.pos);
            let text = text
                .strip_prefix(' ')
                .unwrap_or(text)
                .trim_end()
                .to_string();
            self.push(Tok::Doc(text), start);
        }
    }

    fn word(&mut self, start: usize) {
        while self.at(0).is_some_and(is_ident_char) {
            self.pos += 1;
        }
        let text = self.slice(start, self.pos).to_string();
        let tok = match KEYWORDS.iter().find(|k| **k == text) {
            Some(k) => Tok::Kw(k),
            None => Tok::Ident(text),
        };
        self.push(tok, start);
    }

    fn number(&mut self, start: usize) {
        if self.calendar(start) {
            return;
        }
        while self.at(0).is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        let mut float = false;
        if self.at(0) == Some(b'.') && self.at(1).is_some_and(|b| b.is_ascii_digit()) {
            float = true;
            self.pos += 1;
            while self.at(0).is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.at(0), Some(b'e' | b'E')) {
            let sign = usize::from(matches!(self.at(1), Some(b'+' | b'-')));
            if self.at(1 + sign).is_some_and(|b| b.is_ascii_digit()) {
                float = true;
                self.pos += 1 + sign;
                while self.at(0).is_some_and(|b| b.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
        }
        let text = self.slice(start, self.pos).to_string();
        let suffix_start = self.pos;
        while self.at(0).is_some_and(is_ident_char) {
            self.pos += 1;
        }
        let unit = ["rem", "dp", "fr"]
            .into_iter()
            .find(|u| *u == self.slice(suffix_start, self.pos));
        if let Some(u) = unit {
            match text.parse::<f64>() {
                Ok(v) if v.is_finite() => self.push(Tok::Length(v, u), start),
                _ => {
                    self.error(
                        "E004",
                        start,
                        self.pos,
                        format!("`{text}{u}` is not a finite length"),
                    );
                    self.push(Tok::Length(0.0, u), start);
                }
            }
            return;
        }
        let duration = UNITS
            .iter()
            .map(|(u, _)| *u)
            .find(|u| *u == self.slice(suffix_start, self.pos));
        if let Some(u) = duration
            && !float
        {
            match text.parse::<i64>() {
                Ok(v) => self.push(Tok::Duration(v, u), start),
                Err(_) => {
                    self.error(
                        "E004",
                        start,
                        self.pos,
                        format!("`{text}{u}` does not fit in an Int (64-bit signed)"),
                    );
                    self.push(Tok::Duration(0, u), start);
                }
            }
            return;
        }
        self.pos = suffix_start;
        if float {
            match text.parse::<f64>() {
                Ok(v) if v.is_finite() => self.push(Tok::Float(v), start),
                _ => {
                    self.error(
                        "E004",
                        start,
                        self.pos,
                        format!("`{text}` is not a finite Float literal"),
                    );
                    self.push(Tok::Float(0.0), start);
                }
            }
        } else {
            match text.parse::<i64>() {
                Ok(v) => self.push(Tok::Int(v), start),
                Err(_) => {
                    self.error(
                        "E004",
                        start,
                        self.pos,
                        format!("`{text}` does not fit in an Int (64-bit signed)"),
                    );
                    self.push(Tok::Int(0), start);
                }
            }
        }
    }

    /// A calendar literal at `start`: four digits, `-` and two digits make a month; `-` and two
    /// more a day; `T`, `HH:MM` and optional `:SS` a wall-clock time, which `Z` on its end makes
    /// an instant. Anything else is a number.
    fn calendar(&mut self, start: usize) -> bool {
        let digits = |lx: &Self, at: usize, n: usize| {
            (0..n).all(|i| lx.at(at + i).is_some_and(|b| b.is_ascii_digit()))
                && !lx.at(at + n).is_some_and(|b| b.is_ascii_digit())
        };
        if !(digits(self, 0, 4) && self.at(4) == Some(b'-') && digits(self, 5, 2)) {
            return false;
        }
        let mut len = 7;
        if self.at(7) == Some(b'-') && digits(self, 8, 2) {
            len = 10;
            if self.at(10) == Some(b'T')
                && digits(self, 11, 2)
                && self.at(13) == Some(b':')
                && digits(self, 14, 2)
            {
                len = 16;
                if self.at(16) == Some(b':') && digits(self, 17, 2) {
                    len = 19;
                }
                // No `Z` says no zone: the time is the person's own time of day, resolved to
                // the instant it names where the value is used.
                if self.at(len) == Some(b'Z') {
                    len += 1;
                }
            }
        }
        if self.at(len).is_some_and(is_ident_char) {
            return false;
        }
        let written = self.slice(start, start + len).to_string();
        self.pos += len;
        // A time of no zone is the person's own time of day, resolved where it runs; its
        // canonical text keeps the seconds, so the one shape reaches the checker.
        let (text, zone) = match len {
            7 => (format!("{written}-01"), true),
            17 => (format!("{}:00Z", written.trim_end_matches('Z')), true),
            16 => (format!("{written}:00"), false),
            19 | 20 => (written.clone(), len == 20),
            _ => (written.clone(), true),
        };
        let checked = if len <= 10 {
            check_date(&text)
        } else if zone {
            check_time(&text)
        } else {
            check_wall_clock(&text)
        };
        if let Err(e) = checked {
            self.error("E004", start, self.pos, e);
            self.push(Tok::Calendar(String::new()), start);
            return true;
        }
        let canonical = if len == 7 { written } else { text };
        self.push(Tok::Calendar(canonical), start);
        true
    }

    fn punct(&mut self, b: u8, start: usize) {
        let two = |lx: &Self| lx.at(1);
        let (tok, len) = match (b, two(self)) {
            (b'-', Some(b'>')) => (Tok::Arrow, 2),
            (b'-', Some(b'=')) => (Tok::MinusAssign, 2),
            (b'+', Some(b'=')) => (Tok::PlusAssign, 2),
            (b'=', Some(b'=')) => (Tok::EqEq, 2),
            (b'=', Some(b'>')) => (Tok::FatArrow, 2),
            (b'.', Some(b'.')) => (Tok::DotDot, 2),
            (b'!', Some(b'=')) => (Tok::NotEq, 2),
            (b'<', Some(b'=')) => (Tok::Le, 2),
            (b'>', Some(b'=')) => (Tok::Ge, 2),
            (b'?', Some(b'.')) => (Tok::QDot, 2),
            (b'?', Some(b'?')) => (Tok::QQ, 2),
            (b'(', _) => (Tok::LParen, 1),
            (b')', _) => (Tok::RParen, 1),
            (b'{', _) => (Tok::LBrace, 1),
            (b'}', _) => (Tok::RBrace, 1),
            (b'[', _) => (Tok::LBracket, 1),
            (b']', _) => (Tok::RBracket, 1),
            (b',', _) => (Tok::Comma, 1),
            (b':', _) => (Tok::Colon, 1),
            (b'.', _) => (Tok::Dot, 1),
            (b'=', _) => (Tok::Assign, 1),
            (b'<', _) => (Tok::Lt, 1),
            (b'>', _) => (Tok::Gt, 1),
            (b'+', _) => (Tok::Plus, 1),
            (b'-', _) => (Tok::Minus, 1),
            (b'*', _) => (Tok::Star, 1),
            (b'/', _) => (Tok::Slash, 1),
            (b'%', _) => (Tok::Percent, 1),
            (b'#', _) => (Tok::Hash, 1),
            (b'|', _) => (Tok::Pipe, 1),
            _ => {
                let ch = self
                    .src
                    .get(self.pos..)
                    .and_then(|s| s.chars().next())
                    .unwrap_or('?');
                self.pos += ch.len_utf8();
                self.error(
                    "E001",
                    start,
                    self.pos,
                    format!("unexpected character `{ch}`"),
                );
                return;
            }
        };
        match tok {
            Tok::LParen | Tok::LBracket => self.depth += 1,
            Tok::RParen | Tok::RBracket => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
        self.pos += len;
        self.push(tok, start);
    }

    /// `@app.id.Type:id@version`
    fn reference(&mut self, start: usize) {
        self.pos += 1;
        let mut segs: Vec<String> = Vec::new();
        loop {
            let s = self.pos;
            if !self.at(0).is_some_and(is_ident_start) {
                break;
            }
            while self.at(0).is_some_and(is_ident_char) {
                self.pos += 1;
            }
            segs.push(self.slice(s, self.pos).to_string());
            if self.at(0) == Some(b'.') && self.at(1).is_some_and(is_ident_start) {
                self.pos += 1;
            } else {
                break;
            }
        }
        let ok = !segs.is_empty() && self.at(0) == Some(b':');
        if !ok {
            self.error(
                "E001",
                start,
                self.pos,
                "a reference is written `@app.Type:id`, for example `@music.Album:01J8ZK4T`",
            );
            return;
        }
        self.pos += 1;
        let id_start = self.pos;
        self.id_chars();
        // An item of a document is written `doc/item`. A reference written before items were
        // named by id alone has a `/field:item` step per nesting; it names its last item.
        while self.at(0) == Some(b'/') && self.step_follows() {
            self.pos += 1;
            self.id_chars();
            if self.at(0) == Some(b':') {
                self.pos += 1;
                self.id_chars();
            }
        }
        let id = self.slice(id_start, self.pos).to_string();
        if id.is_empty() {
            self.error(
                "E001",
                start,
                self.pos,
                "a reference needs an id after the `:`",
            );
            return;
        }
        let mut version = None;
        if self.at(0) == Some(b'@') && self.at(1).is_some_and(is_ident_char) {
            self.pos += 1;
            let v = self.pos;
            while self.at(0).is_some_and(is_ident_char) {
                self.pos += 1;
            }
            version = Some(self.slice(v, self.pos).to_string());
        }
        let ty = segs.pop().unwrap_or_default();
        let lit = RefLit {
            app: segs.join("."),
            ty,
            id,
            version,
        };
        self.push(Tok::Ref(lit), start);
    }

    fn id_chars(&mut self) {
        while self.at(0).is_some_and(|b| is_ident_char(b) || b == b'-') {
            self.pos += 1;
        }
    }

    /// Whether the `/` at the cursor starts a `/item` (or older `/field:item`) step of a
    /// reference id.
    fn step_follows(&self) -> bool {
        let is_id = |b: u8| is_ident_char(b) || b == b'-';
        let mut i = self.pos + 1;
        let field = i;
        while self.bytes.get(i).is_some_and(|b| is_id(*b)) {
            i += 1;
        }
        if i == field {
            return false;
        }
        self.bytes.get(i) != Some(&b':') || self.bytes.get(i + 1).is_some_and(|b| is_id(*b))
    }

    fn string(&mut self, start: usize) {
        self.pos += 1;
        let mut pieces: Vec<StrPiece> = Vec::new();
        let mut cur = String::new();
        loop {
            let Some(b) = self.at(0) else {
                self.error(
                    "E002",
                    start,
                    self.pos,
                    "unterminated string literal: add a closing `\"`",
                );
                break;
            };
            match b {
                b'"' => {
                    self.pos += 1;
                    break;
                }
                b'\n' => {
                    self.error("E002", start, self.pos, "unterminated string literal: strings end on the line they start; write `\\n` for a newline");
                    break;
                }
                b'\\' => self.escape(&mut cur),
                b'{' => {
                    let open = self.pos;
                    match self.interpolation_end(open + 1) {
                        Some(close) => {
                            if !cur.is_empty() {
                                pieces.push(StrPiece::Lit(std::mem::take(&mut cur)));
                            }
                            pieces.push(StrPiece::Interp {
                                start: self.base + open + 1,
                                end: self.base + close,
                            });
                            self.pos = close + 1;
                        }
                        None => {
                            self.error("E002", open, self.bytes.len(), "unterminated `{` in a string: close the interpolation with `}` or write `\\{`");
                            self.pos = self.bytes.len();
                            break;
                        }
                    }
                }
                _ => {
                    let ch = self
                        .src
                        .get(self.pos..)
                        .and_then(|s| s.chars().next())
                        .unwrap_or('?');
                    cur.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        if !cur.is_empty() || pieces.is_empty() {
            pieces.push(StrPiece::Lit(cur));
        }
        self.push(Tok::Str(pieces), start);
    }

    /// Whether the token before this point leaves an operand to come, so a `/` here starts a
    /// regular expression, not a division: the start of a line, an opening bracket, a comma, a
    /// colon, an operator or a keyword.
    fn operand_expected(&self) -> bool {
        !matches!(
            self.toks.last().map(|t| &t.tok),
            Some(
                Tok::Ident(_)
                    | Tok::Int(_)
                    | Tok::Float(_)
                    | Tok::Length(..)
                    | Tok::Duration(..)
                    | Tok::Calendar(_)
                    | Tok::Str(_)
                    | Tok::Ref(_)
                    | Tok::RParen
                    | Tok::RBracket
                    | Tok::RBrace
                    | Tok::Kw("true" | "false")
            )
        )
    }

    /// A regular expression, `/\bTide\b/i` (agents.md, "Querying"): text whose pattern is
    /// what is between the slashes, a `\/` standing for a slash, and whose flags (`i`, `m`, `s`,
    /// `x`) follow the closing slash. It is a `Text` holding the pattern with its flags written
    /// inline (`(?i)\bTide\b`), the form `grep` and `read(matching:)` take.
    fn regex(&mut self, start: usize) {
        self.pos += 1;
        let mut pattern = String::new();
        loop {
            match self.at(0) {
                None | Some(b'\n') => {
                    self.error(
                        "E002",
                        start,
                        self.pos,
                        "unterminated regular expression: close it with `/` on the same line",
                    );
                    break;
                }
                Some(b'/') => {
                    self.pos += 1;
                    break;
                }
                Some(b'\\') if self.at(1) == Some(b'/') => {
                    pattern.push('/');
                    self.pos += 2;
                }
                Some(_) => {
                    let ch = self
                        .src
                        .get(self.pos..)
                        .and_then(|s| s.chars().next())
                        .unwrap_or('?');
                    pattern.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        let mut flags = String::new();
        while let Some(flag) = self.at(0).filter(u8::is_ascii_alphabetic) {
            if !matches!(flag, b'i' | b'm' | b's' | b'x') {
                self.error(
                    "E002",
                    self.pos,
                    self.pos + 1,
                    format!(
                        "`{}` is no flag of a regular expression; its flags are i, m, s and x",
                        char::from(flag)
                    ),
                );
            } else if !flags.contains(char::from(flag)) {
                flags.push(char::from(flag));
            }
            self.pos += 1;
        }
        let text = if flags.is_empty() {
            pattern
        } else {
            format!("(?{flags}){pattern}")
        };
        self.push(Tok::Str(vec![StrPiece::Lit(text)]), start);
    }

    fn escape(&mut self, cur: &mut String) {
        let start = self.pos;
        self.pos += 1;
        let Some(b) = self.at(0) else { return };
        self.pos += 1;
        match b {
            b'n' => cur.push('\n'),
            b't' => cur.push('\t'),
            b'r' => cur.push('\r'),
            b'"' => cur.push('"'),
            b'\\' => cur.push('\\'),
            b'{' => cur.push('{'),
            b'}' => cur.push('}'),
            other => {
                self.error(
                    "E005",
                    start,
                    self.pos,
                    format!(
                        "unknown escape `\\{}`; the escapes are \\n \\t \\r \\\" \\\\ \\{{ \\}}",
                        other as char
                    ),
                );
            }
        }
    }

    /// The offset of the `}` closing an interpolation that starts at `from`.
    fn interpolation_end(&self, from: usize) -> Option<usize> {
        let mut i = from;
        let mut depth = 0usize;
        while let Some(b) = self.bytes.get(i).copied() {
            match b {
                b'{' => depth += 1,
                b'}' if depth == 0 => return Some(i),
                b'}' => depth -= 1,
                b'"' => i = self.string_end(i + 1)?,
                b'\n' => return None,
                _ => {}
            }
            i += 1;
        }
        None
    }

    /// The offset of the closing quote of a string whose body starts at `from`.
    fn string_end(&self, from: usize) -> Option<usize> {
        let mut i = from;
        while let Some(b) = self.bytes.get(i).copied() {
            match b {
                b'"' => return Some(i),
                b'\\' => i += 1,
                b'{' => i = self.interpolation_end(i + 1)?,
                b'\n' => return None,
                _ => {}
            }
            i += 1;
        }
        None
    }
}
