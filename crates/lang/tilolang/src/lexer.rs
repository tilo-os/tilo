//! The lexer. Newlines are tokens because they separate statements; the lexer drops the ones
//! that cannot end a statement (inside brackets, before a leading `.`).

use crate::ast::RefLit;
use crate::diag::{Diagnostic, Span};

/// Words that cannot be used as names.
pub const KEYWORDS: &[&str] = &[
    "app", "use", "as", "type", "enum", "action", "flow", "view", "fn", "let", "if", "else",
    "match", "for", "in", "return", "and", "or", "not", "true", "false", "try", "merge",
    "proposal",
];

/// Words that are keywords only where a declaration starts: `window` and `lens` at the start
/// of a statement of a view body, `root` and `default` at the start of an item. Elsewhere they
/// are ordinary names.
pub const CONTEXTUAL: &[&str] = &["window", "lens", "root", "default"];

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
}

impl Tok {
    /// How the token reads in an error message.
    pub fn describe(&self) -> String {
        match self {
            Self::Ident(n) => format!("`{n}`"),
            Self::Kw(k) => format!("`{k}`"),
            Self::Int(_) | Self::Float(_) | Self::Length(..) => "a number".into(),
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

/// The plain `//` comments of `src` (not `///` doc comments): what printing the canonical form
/// would drop.
pub fn plain_comments(src: &str) -> Vec<Span> {
    let mut lx = Lexer {
        src,
        bytes: src.as_bytes(),
        pos: 0,
        base: 0,
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
    /// Plain `//` comments, which the canonical form does not keep.
    comments: Vec<Span>,
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
        // A line that starts `.name` continues the previous one as a method call, unless the
        // previous line ended a block or the new line is a match arm (`.name(..) =>`).
        if !matches!(self.toks.last().map(|t| &t.tok), Some(Tok::RBrace))
            && self.next_line_continues()
        {
            return;
        }
        self.push(Tok::Newline, start);
    }

    /// Whether the next significant character is a `.` that starts a method call, so the
    /// previous line continues.
    fn next_line_continues(&self) -> bool {
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
                    return self.bytes.get(i + 1).is_some_and(|b| is_ident_start(*b))
                        && !self.is_match_arm(i + 1);
                }
                _ => return false,
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
            self.comments.push(span);
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

    fn punct(&mut self, b: u8, start: usize) {
        let two = |lx: &Self| lx.at(1);
        let (tok, len) = match (b, two(self)) {
            (b'-', Some(b'>')) => (Tok::Arrow, 2),
            (b'-', Some(b'=')) => (Tok::MinusAssign, 2),
            (b'+', Some(b'=')) => (Tok::PlusAssign, 2),
            (b'=', Some(b'=')) => (Tok::EqEq, 2),
            (b'=', Some(b'>')) => (Tok::FatArrow, 2),
            (b'!', Some(b'=')) => (Tok::NotEq, 2),
            (b'<', Some(b'=')) => (Tok::Le, 2),
            (b'>', Some(b'=')) => (Tok::Ge, 2),
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
        // A resource nested in another is written `doc/field:item`, one step per nesting.
        while self.at(0) == Some(b'/') && self.step_follows() {
            self.pos += 1;
            self.id_chars();
            self.pos += 1;
            self.id_chars();
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

    /// Whether the `/` at the cursor starts a `/field:item` step of a reference id.
    fn step_follows(&self) -> bool {
        let is_id = |b: u8| is_ident_char(b) || b == b'-';
        let mut i = self.pos + 1;
        let field = i;
        while self.bytes.get(i).is_some_and(|b| is_id(*b)) {
            i += 1;
        }
        if i == field || self.bytes.get(i) != Some(&b':') {
            return false;
        }
        self.bytes.get(i + 1).is_some_and(|b| is_id(*b))
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
