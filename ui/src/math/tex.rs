//! Best-effort TeX math parser.
//!
//! This deliberately parses the mathematical language, not TeX's macro and
//! page-building language. Unknown commands are preserved as upright names and
//! reported, so a note never loses source merely because its preview cannot
//! implement a package-specific macro.

use super::model::{MathExpr, Node, Row};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseDiagnostic {
    pub offset: usize,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParseOutput {
    pub expr: MathExpr,
    pub diagnostics: Vec<ParseDiagnostic>,
}

pub fn parse_tex(source: &str) -> ParseOutput {
    let source = strip_math_wrapper(source);
    let mut parser = TexParser::new(source);
    let root = parser.parse_row(None, false);
    ParseOutput {
        expr: MathExpr { root },
        diagnostics: parser.diagnostics,
    }
}

/// Canonical TeX emitted after a user edits the parsed tree visually.
/// Source-mode formulas retain their original bytes; this serializer is only
/// used once the structured editor has made a semantic change.
pub fn serialize_tex(expr: &MathExpr) -> String {
    fn row(nodes: &Row, out: &mut String) {
        for node in nodes {
            match node {
                Node::Char(ch) => out.push(*ch),
                Node::Frac { num, den } => {
                    out.push_str(r"\frac{");
                    row(num, out);
                    out.push_str("}{");
                    row(den, out);
                    out.push('}');
                }
                Node::Sup { exp } => {
                    out.push_str("^{");
                    row(exp, out);
                    out.push('}');
                }
                Node::Sub { sub } => {
                    out.push_str("_{");
                    row(sub, out);
                    out.push('}');
                }
                Node::SubSup { sub, sup } => {
                    out.push_str("_{");
                    row(sub, out);
                    out.push_str("}^{");
                    row(sup, out);
                    out.push('}');
                }
                Node::Sqrt { index, radicand } => {
                    out.push_str(r"\sqrt");
                    if let Some(index) = index {
                        out.push('[');
                        row(index, out);
                        out.push(']');
                    }
                    out.push('{');
                    row(radicand, out);
                    out.push('}');
                }
                Node::Paren { body } => {
                    out.push_str(r"\left(");
                    row(body, out);
                    out.push_str(r"\right)");
                }
                Node::Delimited { open, close, body } => {
                    out.push_str(r"\left");
                    write_delimiter(*open, out);
                    row(body, out);
                    out.push_str(r"\right");
                    write_delimiter(*close, out);
                }
                Node::Func { name, arg } => {
                    if matches!(name.as_str(), "sin" | "cos" | "tan" | "log" | "ln" | "exp") {
                        out.push('\\');
                        out.push_str(name);
                    } else {
                        out.push_str(r"\operatorname{");
                        out.push_str(name);
                        out.push('}');
                    }
                    out.push_str(r"\left(");
                    row(arg, out);
                    out.push_str(r"\right)");
                }
                Node::Text(text) => {
                    out.push_str(r"\text{");
                    out.push_str(
                        &text
                            .replace('\\', r"\textbackslash ")
                            .replace('{', r"\{")
                            .replace('}', r"\}"),
                    );
                    out.push('}');
                }
                Node::Accent { base, mark, over } => {
                    let command = match (*mark, *over) {
                        ('ˆ' | '^', true) => "hat",
                        ('˜' | '~', true) => "widetilde",
                        ('→', true) => "overrightarrow",
                        ('←', true) => "overleftarrow",
                        ('˙', true) => "dot",
                        ('¨', true) => "ddot",
                        ('_', false) => "underline",
                        _ => "overline",
                    };
                    out.push('\\');
                    out.push_str(command);
                    out.push('{');
                    row(base, out);
                    out.push('}');
                }
                Node::UnderOver { base, under, over } => {
                    row(base, out);
                    if let Some(under) = under {
                        out.push_str("_{");
                        row(under, out);
                        out.push('}');
                    }
                    if let Some(over) = over {
                        out.push_str("^{");
                        row(over, out);
                        out.push('}');
                    }
                }
                Node::Table { rows } => {
                    out.push_str(r"\begin{matrix}");
                    for (row_index, cells) in rows.iter().enumerate() {
                        if row_index > 0 {
                            out.push_str(r"\\");
                        }
                        for (cell_index, cell) in cells.iter().enumerate() {
                            if cell_index > 0 {
                                out.push('&');
                            }
                            row(cell, out);
                        }
                    }
                    out.push_str(r"\end{matrix}");
                }
            }
        }
    }

    fn write_delimiter(delimiter: Option<char>, out: &mut String) {
        match delimiter {
            None => out.push('.'),
            Some('{') => out.push_str(r"\{"),
            Some('}') => out.push_str(r"\}"),
            Some('⟨') => out.push_str(r"\langle"),
            Some('⟩') => out.push_str(r"\rangle"),
            Some('⌊') => out.push_str(r"\lfloor"),
            Some('⌋') => out.push_str(r"\rfloor"),
            Some('⌈') => out.push_str(r"\lceil"),
            Some('⌉') => out.push_str(r"\rceil"),
            Some('‖') => out.push_str(r"\Vert"),
            Some(ch) => out.push(ch),
        }
    }

    let mut out = String::new();
    row(&expr.root, &mut out);
    out
}

struct TexParser {
    chars: Vec<char>,
    pos: usize,
    diagnostics: Vec<ParseDiagnostic>,
    limit_mode: Option<bool>,
}

impl TexParser {
    fn new(source: &str) -> Self {
        Self {
            chars: source.chars().collect(),
            pos: 0,
            diagnostics: Vec::new(),
            limit_mode: None,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn parse_row(&mut self, end: Option<char>, stop_on_right: bool) -> Row {
        let mut row = Vec::new();
        while let Some(ch) = self.peek() {
            if Some(ch) == end || (stop_on_right && self.command_ahead("right")) {
                break;
            }
            if ch.is_whitespace() {
                self.pos += 1;
                continue;
            }
            if ch == '}' || ch == ']' {
                self.error("unmatched closing delimiter");
                self.pos += 1;
                continue;
            }
            if ch == '^' || ch == '_' {
                self.error("script has no base");
                self.pos += 1;
                let _ = self.parse_argument();
                continue;
            }
            let mut atoms = self.parse_atom();
            row.append(&mut atoms);
            self.attach_scripts(&mut row);
        }
        row
    }

    fn parse_atom(&mut self) -> Row {
        match self.peek() {
            Some('{') => self.parse_group(),
            Some('\\') => self.parse_command(),
            Some('~') => {
                self.pos += 1;
                vec![Node::Char('\u{00a0}')]
            }
            Some(ch) => {
                self.pos += 1;
                vec![Node::Char(normalize_ascii_operator(ch))]
            }
            None => Vec::new(),
        }
    }

    fn parse_group(&mut self) -> Row {
        if self.peek() != Some('{') {
            return self.parse_atom();
        }
        self.pos += 1;
        let row = self.parse_row(Some('}'), false);
        if self.peek() == Some('}') {
            self.pos += 1;
        } else {
            self.error("unclosed group");
        }
        row
    }

    fn parse_optional_group(&mut self) -> Option<Row> {
        self.skip_space();
        if self.peek() != Some('[') {
            return None;
        }
        self.pos += 1;
        let row = self.parse_row(Some(']'), false);
        if self.peek() == Some(']') {
            self.pos += 1;
        } else {
            self.error("unclosed optional argument");
        }
        Some(row)
    }

    fn parse_argument(&mut self) -> Row {
        self.skip_space();
        if self.peek() == Some('{') {
            self.parse_group()
        } else {
            self.parse_atom()
        }
    }

    fn parse_command(&mut self) -> Row {
        let command_start = self.pos;
        self.pos += 1;
        if self.peek() == Some('\\') {
            self.pos += 1;
            return Vec::new();
        }
        let name = self.read_command_name();
        if name.is_empty() {
            return self.peek().map_or_else(Vec::new, |ch| {
                self.pos += 1;
                vec![Node::Char(ch)]
            });
        }
        match name.as_str() {
            "frac" | "dfrac" | "tfrac" => vec![Node::Frac {
                num: self.parse_argument(),
                den: self.parse_argument(),
            }],
            "sqrt" => {
                let index = self.parse_optional_group();
                vec![Node::Sqrt {
                    index,
                    radicand: self.parse_argument(),
                }]
            }
            "binom" | "dbinom" | "tbinom" => {
                let upper = self.parse_argument();
                let lower = self.parse_argument();
                vec![Node::Delimited {
                    open: Some('('),
                    close: Some(')'),
                    body: vec![Node::Table {
                        rows: vec![vec![upper], vec![lower]],
                    }],
                }]
            }
            "left" => self.parse_left_right(),
            "text" | "textrm" | "textnormal" | "operatorname" => {
                vec![Node::Text(self.read_text_argument())]
            }
            "mathrm" => vec![Node::Text(row_plain(&self.parse_argument()))],
            "mathbf" | "boldsymbol" | "bm" => {
                map_row_chars(self.parse_argument(), MathAlphabet::Bold)
            }
            "mathbb" => map_row_chars(self.parse_argument(), MathAlphabet::Blackboard),
            "mathcal" | "mathscr" => map_row_chars(self.parse_argument(), MathAlphabet::Script),
            "mathfrak" => map_row_chars(self.parse_argument(), MathAlphabet::Fraktur),
            "mathtt" => map_row_chars(self.parse_argument(), MathAlphabet::Monospace),
            "mathit" => self.parse_argument(),
            "overline" | "bar" => self.accent('¯', true),
            "underline" => self.accent('_', false),
            "hat" | "widehat" => self.accent('ˆ', true),
            "tilde" | "widetilde" => self.accent('˜', true),
            "vec" | "overrightarrow" => self.accent('→', true),
            "overleftarrow" => self.accent('←', true),
            "dot" => self.accent('˙', true),
            "ddot" => self.accent('¨', true),
            "overset" | "stackrel" => {
                let over = self.parse_argument();
                let base = self.parse_argument();
                vec![Node::UnderOver {
                    base,
                    under: None,
                    over: Some(over),
                }]
            }
            "underset" => {
                let under = self.parse_argument();
                let base = self.parse_argument();
                vec![Node::UnderOver {
                    base,
                    under: Some(under),
                    over: None,
                }]
            }
            "begin" => {
                let environment = self.read_raw_group();
                if environment == "array" {
                    // Column alignment/preamble affects table alignment, not
                    // mathematical cell content. Consume it rather than
                    // rendering `cc`, `rcl`, etc. as the first cell.
                    let _ = self.read_raw_group();
                }
                self.parse_environment(&environment)
            }
            "limits" => {
                self.limit_mode = Some(true);
                Vec::new()
            }
            "nolimits" => {
                self.limit_mode = Some(false);
                Vec::new()
            }
            "mod" | "bmod" | "pmod" => {
                let argument = if name == "pmod" {
                    let row = self.parse_argument();
                    format!("(mod {})", row_plain(&row))
                } else {
                    "mod".to_string()
                };
                vec![Node::Text(argument)]
            }
            "sum" | "prod" | "coprod" | "bigcup" | "bigcap" | "bigvee" | "bigwedge" | "lim"
            | "limsup" | "liminf" | "max" | "min" | "sup" | "inf" => {
                let base = operator_row(&name);
                vec![Node::UnderOver {
                    base,
                    under: None,
                    over: None,
                }]
            }
            "sin" | "cos" | "tan" | "cot" | "sec" | "csc" | "arcsin" | "arccos" | "arctan"
            | "sinh" | "cosh" | "tanh" | "log" | "ln" | "exp" | "det" | "gcd" | "ker" | "Pr"
            | "hom" => vec![Node::Text(name)],
            "quad" => vec![Node::Char('\u{2003}')],
            "qquad" => vec![Node::Char('\u{2003}'), Node::Char('\u{2003}')],
            "," | ":" | ";" | "!" | " " => Vec::new(),
            "not" => vec![Node::Char('̸')],
            "right" | "end" => {
                self.pos = command_start;
                Vec::new()
            }
            _ => {
                if let Some(symbol) = tex_symbol(&name) {
                    vec![Node::Char(symbol)]
                } else {
                    self.diagnostics.push(ParseDiagnostic {
                        offset: command_start,
                        message: format!("unsupported TeX command \\{name}"),
                    });
                    vec![Node::Text(name)]
                }
            }
        }
    }

    fn accent(&mut self, accent: char, over: bool) -> Row {
        let base = self.parse_argument();
        vec![Node::Accent {
            base,
            mark: accent,
            over,
        }]
    }

    fn parse_left_right(&mut self) -> Row {
        self.skip_space();
        let open = self.read_delimiter();
        let body = self.parse_row(None, true);
        let close = if self.command_ahead("right") {
            self.pos += 1;
            let _ = self.read_command_name();
            self.skip_space();
            self.read_delimiter()
        } else {
            self.error("\\left has no matching \\right");
            None
        };
        vec![Node::Delimited { open, close, body }]
    }

    fn read_delimiter(&mut self) -> Option<char> {
        if self.peek() == Some('\\') {
            self.pos += 1;
            let name = self.read_command_name();
            return match name.as_str() {
                "langle" => Some('⟨'),
                "rangle" => Some('⟩'),
                "lbrace" | "{" => Some('{'),
                "rbrace" | "}" => Some('}'),
                "lvert" | "rvert" | "vert" => Some('|'),
                "lVert" | "rVert" | "Vert" => Some('‖'),
                "lfloor" => Some('⌊'),
                "rfloor" => Some('⌋'),
                "lceil" => Some('⌈'),
                "rceil" => Some('⌉'),
                _ => name.chars().next(),
            };
        }
        let ch = self.peek()?;
        self.pos += 1;
        (ch != '.').then_some(ch)
    }

    fn attach_scripts(&mut self, row: &mut Row) {
        let mut sub = None;
        let mut sup = None;
        let mut primes = 0usize;
        loop {
            self.skip_space();
            match self.peek() {
                Some('_') if sub.is_none() => {
                    self.pos += 1;
                    sub = Some(self.parse_argument());
                }
                Some('^') if sup.is_none() => {
                    self.pos += 1;
                    sup = Some(self.parse_argument());
                }
                Some('\'') => {
                    self.pos += 1;
                    primes += 1;
                }
                _ => break,
            }
        }
        if primes > 0 {
            let prime_row = (0..primes).map(|_| Node::Char('′'));
            sup.get_or_insert_with(Vec::new).extend(prime_row);
        }
        if sub.is_none() && sup.is_none() {
            return;
        }
        let limit_mode = self.limit_mode.take();
        if limit_mode != Some(false)
            && let Some(Node::UnderOver { under, over, .. }) = row.last_mut()
        {
            *under = sub;
            *over = sup;
            return;
        }
        if limit_mode == Some(true)
            && let Some(base) = row.pop()
        {
            row.push(Node::UnderOver {
                base: vec![base],
                under: sub,
                over: sup,
            });
            return;
        }
        if limit_mode == Some(false)
            && let Some(Node::UnderOver { base, .. }) = row.pop()
        {
            row.extend(base);
        }
        row.push(match (sub, sup) {
            (Some(sub), Some(sup)) => Node::SubSup { sub, sup },
            (Some(sub), None) => Node::Sub { sub },
            (None, Some(exp)) => Node::Sup { exp },
            (None, None) => unreachable!(),
        });
    }

    fn parse_environment(&mut self, name: &str) -> Row {
        let end_marker: Vec<char> = format!("\\end{{{name}}}").chars().collect();
        let start = self.pos;
        let mut depth = 0i32;
        while self.pos < self.chars.len() {
            if depth == 0 && self.chars[self.pos..].starts_with(&end_marker) {
                break;
            }
            match self.chars[self.pos] {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            self.pos += 1;
        }
        let content: String = self.chars[start..self.pos].iter().collect();
        if self.chars[self.pos..].starts_with(&end_marker) {
            self.pos += end_marker.len();
        } else {
            self.error(&format!("unclosed {name} environment"));
        }
        let cells = split_environment(&content);
        let mut rows = Vec::new();
        let mut nested_diagnostics = Vec::new();
        for source_row in cells {
            let mut row = Vec::new();
            for cell in source_row {
                let parsed = parse_tex(cell.trim());
                nested_diagnostics.extend(parsed.diagnostics);
                row.push(parsed.expr.root);
            }
            rows.push(row);
        }
        self.diagnostics.extend(nested_diagnostics);
        let table = vec![Node::Table { rows }];
        let (open, close) = match name {
            "pmatrix" => (Some('('), Some(')')),
            "bmatrix" => (Some('['), Some(']')),
            "Bmatrix" => (Some('{'), Some('}')),
            "vmatrix" => (Some('|'), Some('|')),
            "Vmatrix" => (Some('‖'), Some('‖')),
            "cases" => (Some('{'), None),
            "matrix" | "smallmatrix" | "array" | "aligned" | "align" | "gathered" => (None, None),
            _ => {
                self.error(&format!("unsupported TeX environment {name}"));
                (None, None)
            }
        };
        if open.is_some() || close.is_some() {
            vec![Node::Delimited {
                open,
                close,
                body: table,
            }]
        } else {
            table
        }
    }

    fn read_text_argument(&mut self) -> String {
        self.skip_space();
        if self.peek() != Some('{') {
            return row_plain(&self.parse_argument());
        }
        self.read_raw_group()
    }

    fn read_raw_group(&mut self) -> String {
        self.skip_space();
        if self.peek() != Some('{') {
            self.error("expected braced argument");
            return String::new();
        }
        self.pos += 1;
        let start = self.pos;
        let mut depth = 1usize;
        while self.pos < self.chars.len() && depth > 0 {
            match self.chars[self.pos] {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            self.pos += 1;
        }
        let end = if depth == 0 { self.pos - 1 } else { self.pos };
        if depth != 0 {
            self.error("unclosed braced argument");
        }
        self.chars[start..end].iter().collect()
    }

    fn command_ahead(&self, wanted: &str) -> bool {
        if self.peek() != Some('\\') {
            return false;
        }
        let rest: String = self.chars[self.pos + 1..]
            .iter()
            .take(wanted.len())
            .collect();
        rest == wanted
            && self
                .chars
                .get(self.pos + 1 + wanted.chars().count())
                .is_none_or(|ch| !ch.is_ascii_alphabetic())
    }

    fn read_command_name(&mut self) -> String {
        let start = self.pos;
        if self.peek().is_some_and(|ch| ch.is_ascii_alphabetic()) {
            while self.peek().is_some_and(|ch| ch.is_ascii_alphabetic()) {
                self.pos += 1;
            }
        } else if self.peek().is_some() {
            self.pos += 1;
        }
        self.chars[start..self.pos].iter().collect()
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    fn error(&mut self, message: &str) {
        self.diagnostics.push(ParseDiagnostic {
            offset: self.pos,
            message: message.to_string(),
        });
    }
}

fn strip_math_wrapper(source: &str) -> &str {
    let source = source.trim();
    if source.len() >= 2 && source.starts_with('$') && source.ends_with('$') {
        return source[1..source.len() - 1].trim();
    }
    if source.len() >= 4
        && ((source.starts_with(r"\(") && source.ends_with(r"\)"))
            || (source.starts_with(r"\[") && source.ends_with(r"\]")))
    {
        return source[2..source.len() - 2].trim();
    }
    source
}

fn split_environment(source: &str) -> Vec<Vec<String>> {
    let chars: Vec<char> = source.chars().collect();
    let mut rows = vec![vec![String::new()]];
    let mut depth = 0i32;
    let mut i = 0usize;
    while i < chars.len() {
        match chars[i] {
            '{' => depth += 1,
            '}' => depth -= 1,
            '&' if depth == 0 => rows.last_mut().unwrap().push(String::new()),
            '\\' if depth == 0 && chars.get(i + 1) == Some(&'\\') => {
                rows.push(vec![String::new()]);
                i += 1;
            }
            ch => rows.last_mut().unwrap().last_mut().unwrap().push(ch),
        }
        i += 1;
    }
    rows.retain(|row| row.iter().any(|cell| !cell.trim().is_empty()));
    rows
}

fn operator_row(name: &str) -> Row {
    match name {
        "sum" => vec![Node::Char('∑')],
        "prod" => vec![Node::Char('∏')],
        "coprod" => vec![Node::Char('∐')],
        "bigcup" => vec![Node::Char('⋃')],
        "bigcap" => vec![Node::Char('⋂')],
        "bigvee" => vec![Node::Char('⋁')],
        "bigwedge" => vec![Node::Char('⋀')],
        _ => vec![Node::Text(name.to_string())],
    }
}

fn row_plain(row: &Row) -> String {
    super::model::to_linear(&MathExpr { root: row.clone() })
}

#[derive(Clone, Copy)]
enum MathAlphabet {
    Bold,
    Blackboard,
    Script,
    Fraktur,
    Monospace,
}

fn map_row_chars(mut row: Row, alphabet: MathAlphabet) -> Row {
    fn visit(node: &mut Node, alphabet: MathAlphabet) {
        match node {
            Node::Char(ch) => *ch = styled_char(*ch, alphabet).unwrap_or(*ch),
            Node::Frac { num, den } => {
                *num = map_row_chars(std::mem::take(num), alphabet);
                *den = map_row_chars(std::mem::take(den), alphabet);
            }
            Node::Sup { exp } => *exp = map_row_chars(std::mem::take(exp), alphabet),
            Node::Sub { sub } => *sub = map_row_chars(std::mem::take(sub), alphabet),
            Node::SubSup { sub, sup } => {
                *sub = map_row_chars(std::mem::take(sub), alphabet);
                *sup = map_row_chars(std::mem::take(sup), alphabet);
            }
            Node::Sqrt { index, radicand } => {
                if let Some(index) = index {
                    *index = map_row_chars(std::mem::take(index), alphabet);
                }
                *radicand = map_row_chars(std::mem::take(radicand), alphabet);
            }
            Node::Paren { body } | Node::Delimited { body, .. } => {
                *body = map_row_chars(std::mem::take(body), alphabet)
            }
            Node::Func { arg, .. } => *arg = map_row_chars(std::mem::take(arg), alphabet),
            Node::Accent { base, .. } => *base = map_row_chars(std::mem::take(base), alphabet),
            Node::UnderOver { base, under, over } => {
                *base = map_row_chars(std::mem::take(base), alphabet);
                if let Some(under) = under {
                    *under = map_row_chars(std::mem::take(under), alphabet);
                }
                if let Some(over) = over {
                    *over = map_row_chars(std::mem::take(over), alphabet);
                }
            }
            Node::Table { rows } => {
                for row in rows {
                    for cell in row {
                        *cell = map_row_chars(std::mem::take(cell), alphabet);
                    }
                }
            }
            Node::Text(_) => {}
        }
    }
    for node in &mut row {
        visit(node, alphabet);
    }
    row
}

fn styled_char(ch: char, alphabet: MathAlphabet) -> Option<char> {
    let (upper, lower, digit) = match alphabet {
        MathAlphabet::Bold => (0x1D400, 0x1D41A, Some(0x1D7CE)),
        MathAlphabet::Blackboard => (0x1D538, 0x1D552, Some(0x1D7D8)),
        MathAlphabet::Script => (0x1D49C, 0x1D4B6, None),
        MathAlphabet::Fraktur => (0x1D504, 0x1D51E, None),
        MathAlphabet::Monospace => (0x1D670, 0x1D68A, Some(0x1D7F6)),
    };
    let code = match ch {
        'A'..='Z' => upper + (ch as u32 - 'A' as u32),
        'a'..='z' => lower + (ch as u32 - 'a' as u32),
        '0'..='9' => digit? + (ch as u32 - '0' as u32),
        _ => return None,
    };
    char::from_u32(code)
}

fn normalize_ascii_operator(ch: char) -> char {
    match ch {
        '-' => '−',
        '*' => '·',
        _ => ch,
    }
}

fn tex_symbol(name: &str) -> Option<char> {
    Some(match name {
        "alpha" => 'α',
        "beta" => 'β',
        "gamma" => 'γ',
        "delta" => 'δ',
        "epsilon" => 'ϵ',
        "varepsilon" => 'ε',
        "zeta" => 'ζ',
        "eta" => 'η',
        "theta" => 'θ',
        "vartheta" => 'ϑ',
        "iota" => 'ι',
        "kappa" => 'κ',
        "lambda" => 'λ',
        "mu" => 'μ',
        "nu" => 'ν',
        "xi" => 'ξ',
        "pi" => 'π',
        "varpi" => 'ϖ',
        "rho" => 'ρ',
        "varrho" => 'ϱ',
        "sigma" => 'σ',
        "varsigma" => 'ς',
        "tau" => 'τ',
        "upsilon" => 'υ',
        "phi" => 'ϕ',
        "varphi" => 'φ',
        "chi" => 'χ',
        "psi" => 'ψ',
        "omega" => 'ω',
        "Gamma" => 'Γ',
        "Delta" => 'Δ',
        "Theta" => 'Θ',
        "Lambda" => 'Λ',
        "Xi" => 'Ξ',
        "Pi" => 'Π',
        "Sigma" => 'Σ',
        "Upsilon" => 'Υ',
        "Phi" => 'Φ',
        "Psi" => 'Ψ',
        "Omega" => 'Ω',
        "int" => '∫',
        "iint" => '∬',
        "iiint" => '∭',
        "oint" => '∮',
        "infty" => '∞',
        "partial" => '∂',
        "nabla" => '∇',
        "ell" => 'ℓ',
        "hbar" => 'ℏ',
        "imath" => 'ı',
        "jmath" => 'ȷ',
        "Re" => 'ℜ',
        "Im" => 'ℑ',
        "forall" => '∀',
        "exists" => '∃',
        "nexists" => '∄',
        "neg" => '¬',
        "in" => '∈',
        "notin" => '∉',
        "ni" => '∋',
        "subset" => '⊂',
        "supset" => '⊃',
        "subseteq" => '⊆',
        "supseteq" => '⊇',
        "emptyset" => '∅',
        "cup" => '∪',
        "cap" => '∩',
        "setminus" => '∖',
        "times" => '×',
        "div" => '÷',
        "cdot" => '·',
        "ast" => '∗',
        "star" => '⋆',
        "circ" => '∘',
        "bullet" => '•',
        "pm" => '±',
        "mp" => '∓',
        "oplus" => '⊕',
        "otimes" => '⊗',
        "le" | "leq" => '≤',
        "ge" | "geq" => '≥',
        "ne" | "neq" => '≠',
        "approx" => '≈',
        "sim" => '∼',
        "simeq" => '≃',
        "equiv" => '≡',
        "propto" => '∝',
        "perp" => '⊥',
        "parallel" => '∥',
        "mid" => '∣',
        "to" | "rightarrow" => '→',
        "leftarrow" => '←',
        "leftrightarrow" => '↔',
        "Rightarrow" => '⇒',
        "Leftarrow" => '⇐',
        "Leftrightarrow" => '⇔',
        "mapsto" => '↦',
        "uparrow" => '↑',
        "downarrow" => '↓',
        "ldots" => '…',
        "cdots" => '⋯',
        "vdots" => '⋮',
        "ddots" => '⋱',
        "angle" => '∠',
        "triangle" => '△',
        "square" => '□',
        "Box" => '□',
        "prime" => '′',
        "degree" => '°',
        "aleph" => 'ℵ',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fraction_root_scripts_and_greek() {
        let parsed = parse_tex(r"\frac{\alpha_1 + x^2}{\sqrt[3]{1-x}}");
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let Node::Frac { num, den } = &parsed.expr.root[0] else {
            panic!()
        };
        assert!(matches!(num[0], Node::Char('α')));
        assert!(matches!(den[0], Node::Sqrt { index: Some(_), .. }));
    }

    #[test]
    fn accepts_common_math_wrappers_and_prime_scripts() {
        for source in [r"$f'(x)$", r"\(f''(x)\)", r"\[f'''(x)\]"] {
            let parsed = parse_tex(source);
            assert!(
                parsed.diagnostics.is_empty(),
                "{source}: {:?}",
                parsed.diagnostics
            );
            assert!(matches!(parsed.expr.root[1], Node::Sup { .. }));
        }
    }

    #[test]
    fn parses_left_right_and_matrix_environments() {
        let parsed = parse_tex(r"\left[\begin{matrix}a&b\\c&d\end{matrix}\right]");
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let Node::Delimited { open, close, body } = &parsed.expr.root[0] else {
            panic!()
        };
        assert_eq!((*open, *close), (Some('['), Some(']')));
        let Node::Table { rows } = &body[0] else {
            panic!()
        };
        assert_eq!((rows.len(), rows[0].len()), (2, 2));
    }

    #[test]
    fn parses_large_operator_limits_and_cases() {
        let parsed = parse_tex(r"\sum_{i=1}^{n} x_i + \begin{cases}x^2&x>0\\0&x\le0\end{cases}");
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert!(matches!(
            parsed.expr.root[0],
            Node::UnderOver {
                under: Some(_),
                over: Some(_),
                ..
            }
        ));
        assert!(parsed.expr.root.iter().any(|node| matches!(
            node,
            Node::Delimited {
                open: Some('{'),
                close: None,
                ..
            }
        )));
    }

    #[test]
    fn unknown_commands_survive_and_report_diagnostic() {
        let parsed = parse_tex(r"x + \packageMacro{y}");
        assert_eq!(parsed.diagnostics.len(), 1);
        assert!(
            parsed
                .expr
                .root
                .iter()
                .any(|node| matches!(node, Node::Text(name) if name == "packageMacro"))
        );
    }

    #[test]
    fn practical_tex_formula_corpus_parses_without_loss() {
        let formulas = [
            r"x=\frac{-b\pm\sqrt{b^2-4ac}}{2a}",
            r"e^{i\pi}+1=0",
            r"\int_0^\infty e^{-x^2}\,dx=\frac{\sqrt\pi}{2}",
            r"\lim_{n\to\infty}\left(1+\frac1n\right)^n=e",
            r"\begin{pmatrix}a&b\\c&d\end{pmatrix}^{-1}",
            r"f(x)=\begin{cases}x^2&x\ge0\\-x&x<0\end{cases}",
            r"\mathbb{R}^n\subseteq\mathbb{C}^n",
            r"\hat x+\vec v+\overline{AB}+\dot y",
            r"\binom nk=\frac{n!}{k!(n-k)!}",
            r"\begin{array}{rcl}a&=&b+c\\d&=&e-f\end{array}",
        ];
        for source in formulas {
            let parsed = parse_tex(source);
            assert!(
                parsed.diagnostics.is_empty(),
                "{source}: {:?}",
                parsed.diagnostics
            );
            assert!(!parsed.expr.root.is_empty(), "{source}");
        }
    }

    #[test]
    fn visually_edited_tree_serializes_back_to_parseable_tex() {
        let parsed = parse_tex(r"\left[\frac{\hat{x}}{y}\right]+");
        let source = serialize_tex(&parsed.expr);
        let reparsed = parse_tex(&source);
        assert!(
            reparsed.diagnostics.is_empty(),
            "{source}: {:?}",
            reparsed.diagnostics
        );
        assert_eq!(
            super::row_plain(&reparsed.expr.root),
            super::row_plain(&parsed.expr.root)
        );
    }
}
