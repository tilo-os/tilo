//! Typst math syntax lowered into the shared mathematical expression tree.
//!
//! The parser targets Typst's math language (identifiers, function calls,
//! scripts, fractions and matrix/cases constructors), not general Typst markup.

use super::model::{MathExpr, Node, Row};
use super::tex::{ParseDiagnostic, ParseOutput};

pub fn parse_typst(source: &str) -> ParseOutput {
    let source = strip_math_wrapper(source);
    let mut parser = TypstParser {
        chars: source.chars().collect(),
        pos: 0,
        diagnostics: Vec::new(),
    };
    let root = parser.parse_row(&[]);
    ParseOutput {
        expr: MathExpr { root },
        diagnostics: parser.diagnostics,
    }
}

/// Canonical Typst math emitted after an actual structured edit.
pub fn serialize_typst(expr: &MathExpr) -> String {
    let mut out = String::new();
    serialize_row(&expr.root, &mut out);
    out
}

fn serialize_row(nodes: &Row, out: &mut String) {
    for node in nodes {
        serialize_node(node, out);
    }
}

fn serialize_node(node: &Node, out: &mut String) {
    match node {
        Node::Char(ch) => out.push(*ch),
        Node::Frac { num, den } => {
            out.push_str("frac(");
            serialize_row(num, out);
            out.push_str(", ");
            serialize_row(den, out);
            out.push(')');
        }
        Node::Sup { exp } => {
            out.push_str("^(");
            serialize_row(exp, out);
            out.push(')');
        }
        Node::Sub { sub } => {
            out.push_str("_(");
            serialize_row(sub, out);
            out.push(')');
        }
        Node::SubSup { sub, sup } => {
            out.push_str("_(");
            serialize_row(sub, out);
            out.push_str(")^(");
            serialize_row(sup, out);
            out.push(')');
        }
        Node::Sqrt { index, radicand } => {
            if let Some(index) = index {
                out.push_str("root(");
                serialize_row(index, out);
                out.push_str(", ");
            } else {
                out.push_str("sqrt(");
            }
            serialize_row(radicand, out);
            out.push(')');
        }
        Node::Paren { body } => {
            out.push('(');
            serialize_row(body, out);
            out.push(')');
        }
        Node::Delimited { open, close, body } => serialize_delimited(*open, *close, body, out),
        Node::Func { name, arg } => {
            out.push_str(name);
            out.push('(');
            serialize_row(arg, out);
            out.push(')');
        }
        Node::Text(text) => {
            out.push('"');
            out.push_str(&text.replace('\\', r"\\").replace('"', r#"\""#));
            out.push('"');
        }
        Node::Accent { base, mark, .. } => {
            let function = match mark {
                'ˆ' | '^' => "hat",
                '˜' | '~' => "tilde",
                '→' | '←' => "arrow",
                '˙' => "dot",
                '¨' => "ddot",
                _ => "bar",
            };
            out.push_str(function);
            out.push('(');
            serialize_row(base, out);
            out.push(')');
        }
        Node::UnderOver { base, under, over } => {
            serialize_row(base, out);
            if let Some(under) = under {
                out.push_str("_(");
                serialize_row(under, out);
                out.push(')');
            }
            if let Some(over) = over {
                out.push_str("^(");
                serialize_row(over, out);
                out.push(')');
            }
        }
        Node::Table { rows } => {
            out.push_str("mat(");
            serialize_table_rows(rows, "; ", ", ", out);
            out.push(')');
        }
    }
}

/// A `Delimited` node whose sole child is a `Table` is one of Typst's
/// bracketed constructors (`vec`, `cases`, `mat`) rather than a literal
/// delimiter pair; everything else falls back to matching open/close glyphs
/// or a named function (`abs`, `norm`, `floor`, `ceil`).
fn serialize_delimited(open: Option<char>, close: Option<char>, body: &Row, out: &mut String) {
    if let [Node::Table { rows }] = body.as_slice() {
        let is_vector = open == Some('(')
            && close == Some(')')
            && rows.len() > 1
            && rows.iter().all(|cells| cells.len() == 1);
        let is_cases = open == Some('{') && close.is_none();
        out.push_str(if is_vector {
            "vec("
        } else if is_cases {
            "cases("
        } else {
            "mat("
        });
        let row_sep = if is_cases || is_vector { ", " } else { "; " };
        let cell_sep = if is_cases { " & " } else { ", " };
        serialize_table_rows(rows, row_sep, cell_sep, out);
        out.push(')');
        return;
    }
    let function = match (open, close) {
        (Some('|'), Some('|')) => Some("abs"),
        (Some('‖'), Some('‖')) => Some("norm"),
        (Some('⌊'), Some('⌋')) => Some("floor"),
        (Some('⌈'), Some('⌉')) => Some("ceil"),
        _ => None,
    };
    if let Some(function) = function {
        out.push_str(function);
        out.push('(');
        serialize_row(body, out);
        out.push(')');
    } else {
        if let Some(open) = open {
            out.push(open);
        }
        serialize_row(body, out);
        if let Some(close) = close {
            out.push(close);
        }
    }
}

fn serialize_table_rows(rows: &[Vec<Row>], row_sep: &str, cell_sep: &str, out: &mut String) {
    for (row_index, cells) in rows.iter().enumerate() {
        if row_index > 0 {
            out.push_str(row_sep);
        }
        for (cell_index, cell) in cells.iter().enumerate() {
            if cell_index > 0 {
                out.push_str(cell_sep);
            }
            serialize_row(cell, out);
        }
    }
}

struct TypstParser {
    chars: Vec<char>,
    pos: usize,
    diagnostics: Vec<ParseDiagnostic>,
}

impl TypstParser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn parse_row(&mut self, stop: &[char]) -> Row {
        let mut row = Vec::new();
        while let Some(ch) = self.peek() {
            if stop.contains(&ch) {
                break;
            }
            if ch.is_whitespace() {
                self.pos = self.pos.saturating_add(1);
                continue;
            }
            if ch == '/' {
                self.pos = self.pos.saturating_add(1);
                let den = self.parse_operand();
                let num = row.pop().map_or_else(Vec::new, |node| match node {
                    Node::Paren { body } | Node::Delimited { body, .. } => body,
                    other => vec![other],
                });
                row.push(Node::Frac { num, den });
                continue;
            }
            if ch == '^' || ch == '_' {
                self.error("script has no base");
                self.pos = self.pos.saturating_add(1);
                let _ = self.parse_operand();
                continue;
            }
            let mut atoms = self.parse_atom();
            row.append(&mut atoms);
            self.attach_scripts(&mut row);
        }
        row
    }

    fn parse_atom(&mut self) -> Row {
        if let Some(symbol) = self.parse_shorthand() {
            return vec![Node::Char(symbol)];
        }
        match self.peek() {
            Some('(') => {
                self.pos = self.pos.saturating_add(1);
                let body = self.parse_row(&[')']);
                self.consume(')');
                vec![Node::Paren { body }]
            }
            Some('[') => {
                self.pos = self.pos.saturating_add(1);
                let body = self.parse_row(&[']']);
                self.consume(']');
                vec![Node::Delimited {
                    open: Some('['),
                    close: Some(']'),
                    body,
                }]
            }
            Some('"') => vec![Node::Text(self.read_string())],
            Some(ch) if is_ident_start(ch) => {
                let name = self.read_ident();
                self.skip_space();
                if self.peek() == Some('(') {
                    self.pos = self.pos.saturating_add(1);
                    self.parse_call(&name)
                } else if let Some(symbol) = typst_symbol(&name) {
                    if is_large_operator(&name) {
                        vec![Node::UnderOver {
                            base: vec![Node::Char(symbol)],
                            under: None,
                            over: None,
                        }]
                    } else {
                        vec![Node::Char(symbol)]
                    }
                } else if is_upright_name(&name) {
                    vec![Node::Text(name)]
                } else {
                    name.chars().map(Node::Char).collect()
                }
            }
            Some(ch) => {
                self.pos = self.pos.saturating_add(1);
                vec![Node::Char(match ch {
                    '-' => '−',
                    '*' => '·',
                    _ => ch,
                })]
            }
            None => Vec::new(),
        }
    }

    fn parse_call(&mut self, name: &str) -> Row {
        match name {
            "frac" => {
                let num = self.parse_row(&[',', ')']);
                self.consume(',');
                let den = self.parse_row(&[')']);
                self.consume(')');
                vec![Node::Frac { num, den }]
            }
            "sqrt" => {
                let radicand = self.parse_row(&[')']);
                self.consume(')');
                vec![Node::Sqrt {
                    index: None,
                    radicand,
                }]
            }
            "root" => {
                let index = self.parse_row(&[',', ')']);
                self.consume(',');
                let radicand = self.parse_row(&[')']);
                self.consume(')');
                vec![Node::Sqrt {
                    index: Some(index),
                    radicand,
                }]
            }
            "binom" => {
                let upper = self.parse_row(&[',', ')']);
                self.consume(',');
                let lower = self.parse_row(&[')']);
                self.consume(')');
                vec![Node::Delimited {
                    open: Some('('),
                    close: Some(')'),
                    body: vec![Node::Table {
                        rows: vec![vec![upper], vec![lower]],
                    }],
                }]
            }
            "mat" | "matrix" => self.parse_table(Some('('), Some(')')),
            "vec" | "vector" => self.parse_branches(Some('('), Some(')'), false),
            "cases" => self.parse_branches(Some('{'), None, true),
            "abs" => self.parse_delimited_call(Some('|'), Some('|')),
            "norm" => self.parse_delimited_call(Some('‖'), Some('‖')),
            "floor" => self.parse_delimited_call(Some('⌊'), Some('⌋')),
            "ceil" => self.parse_delimited_call(Some('⌈'), Some('⌉')),
            "round" | "lr" => self.parse_delimited_call(Some('('), Some(')')),
            "text" | "upright" | "op" => {
                let body = self.parse_row(&[')']);
                self.consume(')');
                vec![Node::Text(super::model::to_linear(&MathExpr {
                    root: body,
                }))]
            }
            "hat" | "tilde" | "bar" | "dot" | "ddot" | "arrow" => {
                let body = self.parse_row(&[')']);
                self.consume(')');
                let accent = match name {
                    "hat" => 'ˆ',
                    "tilde" => '˜',
                    "bar" => '¯',
                    "dot" => '˙',
                    "ddot" => '¨',
                    _ => '→',
                };
                vec![Node::Accent {
                    base: body,
                    mark: accent,
                    over: true,
                }]
            }
            "underbrace" | "overbrace" => {
                let body = self.parse_row(&[')']);
                self.consume(')');
                vec![Node::UnderOver {
                    base: body,
                    under: (name == "underbrace").then(|| vec![Node::Char('⏟')]),
                    over: (name == "overbrace").then(|| vec![Node::Char('⏞')]),
                }]
            }
            name if is_upright_name(name) => {
                let arg = self.parse_row(&[')']);
                self.consume(')');
                vec![Node::Func {
                    name: name.to_string(),
                    arg,
                }]
            }
            _ => {
                let arg = self.parse_row(&[')']);
                self.consume(')');
                // Typst math calls are extensible: an unrecognized name may
                // be a user-defined function, so preserve it as an ordinary
                // italic mathematical call rather than reporting a false
                // unsupported-feature diagnostic.
                let mut row: Row = name.chars().map(Node::Char).collect();
                row.push(Node::Paren { body: arg });
                row
            }
        }
    }

    fn parse_table(&mut self, open: Option<char>, close: Option<char>) -> Row {
        let mut rows = vec![vec![Vec::new()]];
        loop {
            let cell = self.parse_row(&[',', ';', ')']);
            if let Some(cell_slot) = rows.last_mut().and_then(|r| r.last_mut()) {
                *cell_slot = cell;
            }
            match self.peek() {
                Some(',') => {
                    self.pos = self.pos.saturating_add(1);
                    if let Some(last_row) = rows.last_mut() {
                        last_row.push(Vec::new());
                    }
                }
                Some(';') => {
                    self.pos = self.pos.saturating_add(1);
                    rows.push(vec![Vec::new()]);
                }
                Some(')') => {
                    self.pos = self.pos.saturating_add(1);
                    break;
                }
                None => {
                    self.error("unclosed table constructor");
                    break;
                }
                _ => break,
            }
        }
        vec![Node::Delimited {
            open,
            close,
            body: vec![Node::Table { rows }],
        }]
    }

    /// Typst vectors and cases are lists of vertical branches, unlike `mat`
    /// where commas separate columns. Cases additionally use `&` to align
    /// content within each branch.
    fn parse_branches(&mut self, open: Option<char>, close: Option<char>, aligned: bool) -> Row {
        let mut rows = Vec::new();
        loop {
            self.skip_space();
            if self.peek() == Some(')') {
                self.pos = self.pos.saturating_add(1);
                break;
            }
            if self.peek().is_none() {
                self.error("unclosed branch constructor");
                break;
            }
            let mut row = vec![self.parse_row(if aligned {
                &['&', ',', ';', ')']
            } else {
                &[',', ';', ')']
            })];
            while aligned && self.peek() == Some('&') {
                self.pos = self.pos.saturating_add(1);
                row.push(self.parse_row(&['&', ',', ';', ')']));
            }
            rows.push(row);
            match self.peek() {
                Some(',' | ';') => self.pos = self.pos.saturating_add(1),
                Some(')') => {
                    self.pos = self.pos.saturating_add(1);
                    break;
                }
                None => {
                    self.error("unclosed branch constructor");
                    break;
                }
                _ => break,
            }
        }
        vec![Node::Delimited {
            open,
            close,
            body: vec![Node::Table { rows }],
        }]
    }

    fn parse_delimited_call(&mut self, open: Option<char>, close: Option<char>) -> Row {
        let body = self.parse_row(&[')']);
        self.consume(')');
        vec![Node::Delimited { open, close, body }]
    }

    fn parse_operand(&mut self) -> Row {
        self.skip_space();
        match self.peek() {
            Some('(') => self.parse_atom(),
            Some(ch) if is_ident_start(ch) => self.parse_atom(),
            Some(ch) if ch.is_ascii_digit() => {
                let mut row = Vec::new();
                while let Some(ch) = self.peek() {
                    if !(ch.is_ascii_digit() || ch == '.') {
                        break;
                    }
                    row.push(Node::Char(ch));
                    self.pos = self.pos.saturating_add(1);
                }
                row
            }
            _ => self.parse_atom(),
        }
    }

    fn attach_scripts(&mut self, row: &mut Row) {
        let mut sub = None;
        let mut sup = None;
        let mut primes = 0usize;
        loop {
            self.skip_space();
            match self.peek() {
                Some('_') if sub.is_none() => {
                    self.pos = self.pos.saturating_add(1);
                    sub = Some(self.parse_operand());
                }
                Some('^') if sup.is_none() => {
                    self.pos = self.pos.saturating_add(1);
                    sup = Some(self.parse_operand());
                }
                Some('\'') => {
                    self.pos = self.pos.saturating_add(1);
                    primes = primes.saturating_add(1);
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
        if let Some(Node::UnderOver { under, over, .. }) = row.last_mut() {
            *under = sub;
            *over = sup;
        } else {
            row.push(match (sub, sup) {
                (Some(sub), Some(sup)) => Node::SubSup { sub, sup },
                (Some(sub), None) => Node::Sub { sub },
                (None, Some(exp)) => Node::Sup { exp },
                (None, None) => return,
            });
        }
    }

    fn read_ident(&mut self) -> String {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|ch| ch.is_alphanumeric() || ch == '.' || ch == '-')
        {
            self.pos = self.pos.saturating_add(1);
        }
        self.chars
            .get(start..self.pos)
            .unwrap_or(&[])
            .iter()
            .collect()
    }

    fn parse_shorthand(&mut self) -> Option<char> {
        let rest = self.chars.get(self.pos..)?;
        for (source, symbol) in [
            ("<=>", '⇔'),
            ("=>", '⇒'),
            ("<=", '≤'),
            (">=", '≥'),
            ("!=", '≠'),
            ("->", '→'),
            ("<-", '←'),
            ("+-", '±'),
        ] {
            let chars: Vec<char> = source.chars().collect();
            if rest.starts_with(&chars) {
                self.pos = self.pos.saturating_add(chars.len());
                return Some(symbol);
            }
        }
        None
    }

    fn read_string(&mut self) -> String {
        self.pos = self.pos.saturating_add(1);
        let mut out = String::new();
        while let Some(ch) = self.peek() {
            self.pos = self.pos.saturating_add(1);
            if ch == '"' {
                break;
            }
            if ch == '\\' {
                if let Some(escaped) = self.peek() {
                    out.push(escaped);
                    self.pos = self.pos.saturating_add(1);
                }
            } else {
                out.push(ch);
            }
        }
        out
    }

    fn consume(&mut self, expected: char) {
        self.skip_space();
        if self.peek() == Some(expected) {
            self.pos = self.pos.saturating_add(1);
        } else {
            self.error(&format!("expected {expected}"));
        }
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos = self.pos.saturating_add(1);
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
        source
            .get(1..source.len().saturating_sub(1))
            .unwrap_or(source)
            .trim()
    } else {
        source
    }
}

fn is_ident_start(ch: char) -> bool {
    ch.is_alphabetic()
}

fn is_upright_name(name: &str) -> bool {
    matches!(
        name,
        "sin"
            | "cos"
            | "tan"
            | "cot"
            | "arcsin"
            | "arccos"
            | "arctan"
            | "sinh"
            | "cosh"
            | "tanh"
            | "log"
            | "ln"
            | "exp"
            | "lim"
            | "min"
            | "max"
            | "det"
    )
}

fn is_large_operator(name: &str) -> bool {
    matches!(name, "sum" | "product" | "union.big" | "inter.big")
}

fn typst_symbol(name: &str) -> Option<char> {
    Some(match name {
        "alpha" => 'α',
        "beta" => 'β',
        "gamma" => 'γ',
        "delta" => 'δ',
        "epsilon" => 'ε',
        "zeta" => 'ζ',
        "eta" => 'η',
        "theta" => 'θ',
        "lambda" => 'λ',
        "mu" => 'μ',
        "nu" => 'ν',
        "xi" => 'ξ',
        "pi" => 'π',
        "rho" => 'ρ',
        "sigma" => 'σ',
        "tau" => 'τ',
        "phi" => 'φ',
        "chi" => 'χ',
        "psi" => 'ψ',
        "omega" => 'ω',
        "infinity" | "oo" => '∞',
        "integral" | "integral.cont" => '∫',
        "integral.double" => '∬',
        "sum" => '∑',
        "product" => '∏',
        "union.big" => '⋃',
        "inter.big" => '⋂',
        "partial" => '∂',
        "nabla" => '∇',
        "forall" => '∀',
        "exists" => '∃',
        "in" => '∈',
        "in.not" => '∉',
        "subset" => '⊂',
        "subset.eq" => '⊆',
        "union" => '∪',
        "inter" => '∩',
        "times" => '×',
        "dot" => '·',
        "plus.minus" => '±',
        "minus.plus" => '∓',
        "eq.not" => '≠',
        "lt.eq" => '≤',
        "gt.eq" => '≥',
        "approx" => '≈',
        "equiv" => '≡',
        "arrow.r" => '→',
        "arrow.l" => '←',
        "arrow.l.r" => '↔',
        "arrow.r.double" => '⇒',
        "dots.h" => '…',
        "dots.v" => '⋮',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typst_parses_fractions_matrices_vectors_calls_and_operators() {
        // phase: parses_typst_fraction_root_and_scripts
        {
            let parsed = parse_typst("frac(alpha_1 + x^2, root(3, 1-x))");
            assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
            let Node::Frac { num, den } = &parsed.expr.root[0] else {
                panic!()
            };
            assert!(matches!(num[0], Node::Char('α')));
            assert!(matches!(den[0], Node::Sqrt { index: Some(_), .. }));
        }
        // phase: parses_typst_matrix_and_vector_constructors
        {
            let parsed = parse_typst("mat(a, b; c, d) + vec(x, y, z)");
            assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
            let Node::Delimited { body, .. } = &parsed.expr.root[0] else {
                panic!()
            };
            let Node::Table { rows } = &body[0] else {
                panic!()
            };
            assert_eq!((rows.len(), rows[0].len()), (2, 2));
            assert!(parsed.expr.root.iter().any(|node| matches!(node, Node::Delimited { body, .. } if matches!(&body[0], Node::Table { rows } if rows.len() == 3))));
        }
        // phase: vector_components_and_case_branches_are_vertical
        {
            let vector = parse_typst("$ vec(x, y, z) $");
            let Node::Delimited { body, .. } = &vector.expr.root[0] else {
                panic!()
            };
            assert!(
                matches!(&body[0], Node::Table { rows } if rows.len() == 3 && rows.iter().all(|row| row.len() == 1))
            );

            let cases = parse_typst(r#"cases(x^2 & "if" x >= 0, -x & "otherwise")"#);
            let Node::Delimited { body, .. } = &cases.expr.root[0] else {
                panic!()
            };
            assert!(
                matches!(&body[0], Node::Table { rows } if rows.len() == 2 && rows.iter().all(|row| row.len() == 2))
            );
            assert!(cases.diagnostics.is_empty(), "{:?}", cases.diagnostics);
        }
        // phase: user_defined_function_calls_are_preserved_without_false_diagnostics
        {
            let parsed = parse_typst("f(x) + energy(t)");
            assert!(parsed.diagnostics.is_empty());
            assert!(
                parsed
                    .expr
                    .root
                    .iter()
                    .any(|node| matches!(node, Node::Paren { .. }))
            );
        }
        // phase: parses_typst_large_operator_limits
        {
            let parsed = parse_typst("sum_(i=1)^n x_i");
            assert!(parsed.diagnostics.is_empty());
            assert!(matches!(
                parsed.expr.root[0],
                Node::UnderOver {
                    under: Some(_),
                    over: Some(_),
                    ..
                }
            ));
        }
    }

    #[test]
    fn typst_round_trips_a_practical_corpus_and_visual_constructs() {
        // phase: practical_typst_formula_corpus_parses_without_loss
        {
            let formulas = [
                "x = frac(-b plus.minus sqrt(b^2-4a c), 2a)",
                "integral_0^infinity exp(-x^2) dot x",
                "sum_(i=1)^n x_i",
                "mat(a, b; c, d)",
                "cases(x^2, x gt.eq 0; -x, x < 0)",
                "hat(x) + arrow(v) + norm(x)",
                "binom(n, k) = frac(n!, k! (n-k)!)",
            ];
            for source in formulas {
                let parsed = parse_typst(source);
                assert!(
                    parsed.diagnostics.is_empty(),
                    "{source}: {:?}",
                    parsed.diagnostics
                );
                assert!(!parsed.expr.root.is_empty(), "{source}");
            }
        }
        // phase: visual_vector_and_cases_serialize_to_native_typst_constructs
        {
            for source in [
                "vec(alpha, beta, gamma)",
                r#"cases(x^2 & "if" x >= 0, -x & "otherwise")"#,
            ] {
                let parsed = parse_typst(source);
                let canonical = serialize_typst(&parsed.expr);
                assert!(
                    canonical.starts_with(if source.starts_with("vec") {
                        "vec("
                    } else {
                        "cases("
                    }),
                    "{canonical}"
                );
                let reparsed = parse_typst(&canonical);
                assert!(
                    reparsed.diagnostics.is_empty(),
                    "{canonical}: {:?}",
                    reparsed.diagnostics
                );
            }
        }
    }
}
