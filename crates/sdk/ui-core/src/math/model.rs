//! Structured model for the math editor.
//!
//! A formula is a [`Row`] — an ordered list of [`Node`] atoms. Atoms are
//! either *leaves* (a single character: a digit, an operator, a letter of an
//! identifier) or *structures* that own one or more child rows (a fraction
//! owns a numerator row and a denominator row; a power owns an exponent row;
//! a radical owns a radicand row and an optional index row). This is a real
//! tree, not a flat string — which is what makes a true 2D caret, structural
//! backspace, and stacked rendering possible.
//!
//! The model serializes to a compact linear expression and parses that form
//! back into a tree. Applications may feed the serialization to an evaluator,
//! persist it, or use the tree directly for read-only mathematical content.

/// One element of a [`Row`].
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// A single character placed directly in a row: a digit `0-9`, a `.`, a
    /// letter (identifiers are stored as runs of `Char` letters so the caret
    /// can sit between letters), or an operator glyph (`+ - * / · ^` etc.).
    Char(char),
    /// A fraction: numerator over denominator with a centered bar.
    Frac { num: Row, den: Row },
    /// A power: the base is whatever precedes this node in the row; the
    /// exponent is the raised, smaller child row.
    Sup { exp: Row },
    /// A lowered, smaller child row attached to the preceding base.
    Sub { sub: Row },
    /// Paired lower and upper scripts sharing one horizontal script column.
    SubSup { sub: Row, sup: Row },
    /// A radical: `√` with an overbar spanning `radicand`, optional `index`
    /// (the small number in the crook, e.g. cube root).
    Sqrt { index: Option<Row>, radicand: Row },
    /// A parenthesized group `( body )`.
    Paren { body: Row },
    /// A group with explicit scalable delimiters. `None` represents TeX's
    /// invisible `.` delimiter, used by cases and one-sided constructions.
    Delimited {
        open: Option<char>,
        close: Option<char>,
        body: Row,
    },
    /// A function call `name( arg )` — `name` is upright (sin, cos, ln, …).
    Func { name: String, arg: Row },
    /// Upright mathematical text, such as a unit or an `<mtext>` label.
    Text(String),
    /// A close-fitting accent constructed over or under its base.
    Accent { base: Row, mark: char, over: bool },
    /// A base with optional limits stacked directly below and above it.
    UnderOver {
        base: Row,
        under: Option<Row>,
        over: Option<Row>,
    },
    /// A rectangular table of mathematical cells (matrices, cases, arrays).
    Table { rows: Vec<Vec<Row>> },
}

/// An ordered list of atoms. The editable unit.
pub type Row = Vec<Node>;

/// Which child row of a structure a path step descends into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Num,
    Den,
    Exp,
    Sub,
    Sup,
    Radicand,
    Index,
    Body,
    Arg,
    Base,
    Under,
    Over,
    Cell(u16, u16),
}

/// A caret location: a chain of (node index in the current row, which child
/// slot) descents, terminated by an `offset` insertion gap in the deepest
/// row. An empty `steps` with `offset = k` means "gap `k` in the root row".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path {
    pub steps: Vec<(usize, Slot)>,
    pub offset: usize,
}

impl Path {
    pub fn root(offset: usize) -> Self {
        Path {
            steps: Vec::new(),
            offset,
        }
    }
}

/// The whole editable document for one field.
#[derive(Clone, Debug, PartialEq)]
pub struct MathExpr {
    pub root: Row,
}

impl MathExpr {
    pub fn new() -> Self {
        MathExpr { root: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_empty()
    }

    /// Borrow the row addressed by `steps` (ignores `offset`).
    pub fn row_at<'a>(&'a self, steps: &[(usize, Slot)]) -> Option<&'a Row> {
        let mut row = &self.root;
        for (idx, slot) in steps {
            let node = row.get(*idx)?;
            row = child_row(node, *slot)?;
        }
        Some(row)
    }

    /// Mutably borrow the row addressed by `steps`.
    pub fn row_at_mut<'a>(&'a mut self, steps: &[(usize, Slot)]) -> Option<&'a mut Row> {
        let mut row = &mut self.root;
        for (idx, slot) in steps {
            let node = row.get_mut(*idx)?;
            row = child_row_mut(node, *slot)?;
        }
        Some(row)
    }
}

impl Default for MathExpr {
    fn default() -> Self {
        Self::new()
    }
}

pub fn child_row(node: &Node, slot: Slot) -> Option<&Row> {
    match (node, slot) {
        (Node::Frac { num, .. }, Slot::Num) => Some(num),
        (Node::Frac { den, .. }, Slot::Den) => Some(den),
        (Node::Sup { exp }, Slot::Exp) => Some(exp),
        (Node::Sub { sub } | Node::SubSup { sub, .. }, Slot::Sub) => Some(sub),
        (Node::SubSup { sup, .. }, Slot::Sup) => Some(sup),
        (Node::Sqrt { radicand, .. }, Slot::Radicand) => Some(radicand),
        (Node::Sqrt { index: Some(i), .. }, Slot::Index) => Some(i),
        (Node::Paren { body } | Node::Delimited { body, .. }, Slot::Body) => Some(body),
        (Node::Func { arg, .. }, Slot::Arg) => Some(arg),
        (Node::Accent { base, .. } | Node::UnderOver { base, .. }, Slot::Base) => Some(base),
        (
            Node::UnderOver {
                under: Some(row), ..
            },
            Slot::Under,
        )
        | (
            Node::UnderOver {
                over: Some(row), ..
            },
            Slot::Over,
        ) => Some(row),
        (Node::Table { rows }, Slot::Cell(row, column)) => rows
            .get(usize::from(row))
            .and_then(|cells| cells.get(usize::from(column))),
        _ => None,
    }
}

pub fn child_row_mut(node: &mut Node, slot: Slot) -> Option<&mut Row> {
    match (node, slot) {
        (Node::Frac { num, .. }, Slot::Num) => Some(num),
        (Node::Frac { den, .. }, Slot::Den) => Some(den),
        (Node::Sup { exp }, Slot::Exp) => Some(exp),
        (Node::Sub { sub } | Node::SubSup { sub, .. }, Slot::Sub) => Some(sub),
        (Node::SubSup { sup, .. }, Slot::Sup) => Some(sup),
        (Node::Sqrt { radicand, .. }, Slot::Radicand) => Some(radicand),
        (Node::Sqrt { index: Some(i), .. }, Slot::Index) => Some(i),
        (Node::Paren { body } | Node::Delimited { body, .. }, Slot::Body) => Some(body),
        (Node::Func { arg, .. }, Slot::Arg) => Some(arg),
        (Node::Accent { base, .. } | Node::UnderOver { base, .. }, Slot::Base) => Some(base),
        (
            Node::UnderOver {
                under: Some(row), ..
            },
            Slot::Under,
        )
        | (
            Node::UnderOver {
                over: Some(row), ..
            },
            Slot::Over,
        ) => Some(row),
        (Node::Table { rows }, Slot::Cell(row, column)) => rows
            .get_mut(usize::from(row))
            .and_then(|cells| cells.get_mut(usize::from(column))),
        _ => None,
    }
}

/// Slots a node exposes, in caret Up/Down navigation order (top → bottom).
pub fn node_slots(node: &Node) -> Vec<Slot> {
    match node {
        Node::Frac { .. } => vec![Slot::Num, Slot::Den],
        Node::Sup { .. } => vec![Slot::Exp],
        Node::Sub { .. } => vec![Slot::Sub],
        Node::SubSup { .. } => vec![Slot::Sup, Slot::Sub],
        Node::Sqrt { index: Some(_), .. } => vec![Slot::Index, Slot::Radicand],
        Node::Sqrt { index: None, .. } => vec![Slot::Radicand],
        Node::Paren { .. } | Node::Delimited { .. } => vec![Slot::Body],
        Node::Func { .. } => vec![Slot::Arg],
        Node::Accent { .. } => vec![Slot::Base],
        Node::UnderOver { under, over, .. } => {
            let mut slots = Vec::new();
            if over.is_some() {
                slots.push(Slot::Over);
            }
            slots.push(Slot::Base);
            if under.is_some() {
                slots.push(Slot::Under);
            }
            slots
        }
        Node::Table { rows } => rows
            .iter()
            .enumerate()
            .flat_map(|(row, cells)| {
                cells.iter().enumerate().filter_map(move |(column, _)| {
                    Some(Slot::Cell(
                        u16::try_from(row).ok()?,
                        u16::try_from(column).ok()?,
                    ))
                })
            })
            .collect(),
        Node::Char(_) | Node::Text(_) => vec![],
    }
}

// ---------------------------------------------------------------------------
// Serialization: model → linear ASCII the engine parses.
// ---------------------------------------------------------------------------

/// Serialize the whole expression to a flat ASCII string. Round-trips with
/// [`parse`].
pub fn to_linear(expr: &MathExpr) -> String {
    let mut out = String::new();
    write_row(&expr.root, &mut out);
    out
}

fn write_row(row: &Row, out: &mut String) {
    for node in row {
        write_node(node, out);
    }
}

fn write_node(node: &Node, out: &mut String) {
    match node {
        Node::Char(c) => out.push(*c),
        Node::Sup { exp } => write_bracketed('^', exp, out),
        Node::Sub { sub } => write_bracketed('_', sub, out),
        Node::SubSup { sub, sup } => {
            write_bracketed('_', sub, out);
            write_bracketed('^', sup, out);
        }
        Node::Frac { num, den } => {
            write_paren(num, out);
            out.push('/');
            write_paren(den, out);
        }
        Node::Sqrt { index, radicand } => {
            if let Some(idx) = index {
                out.push_str("root(");
                write_row(idx, out);
                out.push(',');
            } else {
                out.push_str("sqrt(");
            }
            write_row(radicand, out);
            out.push(')');
        }
        Node::Paren { body } => write_paren(body, out),
        Node::Delimited { open, close, body } => {
            if let Some(open) = open {
                out.push(*open);
            }
            write_row(body, out);
            if let Some(close) = close {
                out.push(*close);
            }
        }
        Node::Func { name, arg } => {
            out.push_str(name);
            write_paren(arg, out);
        }
        Node::Text(text) => out.push_str(text),
        Node::Accent { base, .. } => write_row(base, out),
        Node::UnderOver { base, under, over } => {
            write_row(base, out);
            if let Some(under) = under {
                write_bracketed('_', under, out);
            }
            if let Some(over) = over {
                write_bracketed('^', over, out);
            }
        }
        Node::Table { rows } => write_table(rows, out),
    }
}

/// Emits `prefix(row)`, e.g. `^(x+1)` for a superscript.
fn write_bracketed(prefix: char, row: &Row, out: &mut String) {
    out.push(prefix);
    write_paren(row, out);
}

fn write_paren(row: &Row, out: &mut String) {
    out.push('(');
    write_row(row, out);
    out.push(')');
}

fn write_table(rows: &[Vec<Row>], out: &mut String) {
    out.push('[');
    for (row_index, row) in rows.iter().enumerate() {
        if row_index > 0 {
            out.push(';');
        }
        for (column, cell) in row.iter().enumerate() {
            if column > 0 {
                out.push(',');
            }
            write_row(cell, out);
        }
    }
    out.push(']');
}

/// Names rendered upright as function calls when reading a linear string.
pub const FUNCS: &[&str] = &[
    "sin", "cos", "tan", "asin", "acos", "atan", "sinh", "cosh", "tanh", "asinh", "acosh", "atanh",
    "exp", "ln", "log10", "log2", "log", "sqrt", "cbrt", "abs", "sign", "sgn", "floor", "ceil",
    "round", "trunc", "fract", "gamma", "fact", "mod", "atan2", "hypot", "max", "min", "root",
    "pow", "rad", "deg",
];

// ---------------------------------------------------------------------------
// Parsing: linear ASCII → model tree.
// ---------------------------------------------------------------------------

/// Parse a flat ASCII expression into a structured tree. Best-effort and
/// total: anything it cannot classify is kept as literal `Char` atoms so the
/// field never loses content (the engine reports semantic errors at plot
/// time). Recognizes `name(...)` calls, `(...)` groups, `^` powers,
/// `sqrt(...)`/`root(i,x)` radicals, and `a/b` fractions where `a` and `b`
/// are simple immediate operands.
pub fn parse(input: &str) -> MathExpr {
    let chars: Vec<char> = input.chars().collect();
    let mut p = LinParser { chars, pos: 0 };
    let root = p.parse_row(&[]);
    MathExpr { root }
}

struct LinParser {
    chars: Vec<char>,
    pos: usize,
}

impl LinParser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    /// Parse a sequence of atoms until one of `stop` chars (consumed by the
    /// caller) or end of input. Builds `^` and `/` structures as it goes.
    fn parse_row(&mut self, stop: &[char]) -> Row {
        let mut row: Row = Vec::new();
        while let Some(c) = self.peek() {
            if stop.contains(&c) {
                break;
            }
            match c {
                '^' => {
                    self.pos = self.pos.saturating_add(1);
                    let exp = self.parse_superscript();
                    row.push(Node::Sup { exp });
                }
                '/' => {
                    self.pos = self.pos.saturating_add(1);
                    // Denominator is the next immediate operand; numerator is
                    // the operand already emitted (last node, unwrapping a
                    // redundant paren).
                    let den_row = self.parse_operand_row();
                    let num_node = row.pop();
                    let num = match num_node {
                        Some(Node::Paren { body }) => body,
                        Some(n) => vec![n],
                        None => Vec::new(),
                    };
                    let den = match den_row.len() {
                        1 => match den_row.into_iter().next() {
                            Some(Node::Paren { body }) => body,
                            Some(n) => vec![n],
                            None => Vec::new(),
                        },
                        _ => den_row,
                    };
                    row.push(Node::Frac { num, den });
                }
                '(' => {
                    self.pos = self.pos.saturating_add(1);
                    let body = self.parse_row(&[')']);
                    self.expect(')');
                    row.push(Node::Paren { body });
                }
                c if c.is_ascii_alphabetic() => {
                    let name = self.read_ident();
                    if self.peek() == Some('(') {
                        self.pos = self.pos.saturating_add(1);
                        if name == "sqrt" {
                            let radicand = self.parse_row(&[')']);
                            self.expect(')');
                            row.push(Node::Sqrt {
                                index: None,
                                radicand,
                            });
                        } else if name == "root" {
                            let index = self.parse_row(&[',', ')']);
                            if self.peek() == Some(',') {
                                self.pos = self.pos.saturating_add(1);
                                let radicand = self.parse_row(&[')']);
                                self.expect(')');
                                row.push(Node::Sqrt {
                                    index: Some(index),
                                    radicand,
                                });
                            } else {
                                self.expect(')');
                                row.push(Node::Sqrt {
                                    index: None,
                                    radicand: index,
                                });
                            }
                        } else if FUNCS.contains(&name.as_str()) {
                            let arg = self.parse_row(&[')']);
                            self.expect(')');
                            row.push(Node::Func { name, arg });
                        } else {
                            // Unknown name + paren: keep letters, then group.
                            for ch in name.chars() {
                                row.push(Node::Char(ch));
                            }
                            let body = self.parse_row(&[')']);
                            self.expect(')');
                            row.push(Node::Paren { body });
                        }
                    } else {
                        for ch in name.chars() {
                            row.push(Node::Char(ch));
                        }
                    }
                }
                _ => {
                    self.pos = self.pos.saturating_add(1);
                    row.push(Node::Char(c));
                }
            }
        }
        row
    }

    /// Parse the exponent that follows a `^`. `^(...)` takes the group; a
    /// bare `^2` / `^x` / `^sin(x)` takes a single operand.
    fn parse_superscript(&mut self) -> Row {
        if self.peek() == Some('(') {
            self.pos = self.pos.saturating_add(1);
            let r = self.parse_row(&[')']);
            self.expect(')');
            r
        } else {
            self.parse_operand_row()
        }
    }

    /// Parse one "operand": a number, an identifier (optionally a call), or a
    /// parenthesized group. Used for `^` exponents and `/` denominators.
    fn parse_operand_row(&mut self) -> Row {
        match self.peek() {
            Some('(') => {
                self.pos = self.pos.saturating_add(1);
                let body = self.parse_row(&[')']);
                self.expect(')');
                vec![Node::Paren { body }]
            }
            Some('-') => {
                self.pos = self.pos.saturating_add(1);
                let mut r = vec![Node::Char('-')];
                r.extend(self.parse_operand_row());
                r
            }
            Some(c) if c.is_ascii_digit() || c == '.' => {
                let mut r = Vec::new();
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() || c == '.' {
                        r.push(Node::Char(c));
                        self.pos = self.pos.saturating_add(1);
                    } else {
                        break;
                    }
                }
                r
            }
            Some(c) if c.is_ascii_alphabetic() => {
                let name = self.read_ident();
                if self.peek() == Some('(') {
                    // Re-enter the row parser to classify the call uniformly.
                    let mut sub = LinParser {
                        chars: format!("{name}(").chars().chain(self.tail()).collect(),
                        pos: 0,
                    };
                    let nodes = sub.parse_row(&[]);
                    // advance self by what sub consumed past `name(`
                    let consumed = sub
                        .pos
                        .saturating_sub(name.chars().count().saturating_add(1));
                    self.pos = self.pos.saturating_add(consumed);
                    // Keep exactly the single produced call node. `next()`
                    // already yields only the first; the truncate that used to
                    // stand here computed 1.max(len.min(1)), which is always 1.
                    if let Some(first) = nodes.into_iter().next() {
                        vec![first]
                    } else {
                        vec![]
                    }
                } else {
                    name.chars().map(Node::Char).collect()
                }
            }
            _ => Vec::new(),
        }
    }

    fn tail(&self) -> impl Iterator<Item = char> + '_ {
        self.chars.get(self.pos..).unwrap_or(&[]).iter().copied()
    }

    fn read_ident(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                self.pos = self.pos.saturating_add(1);
            } else {
                break;
            }
        }
        self.chars
            .get(start..self.pos)
            .unwrap_or(&[])
            .iter()
            .collect()
    }

    fn expect(&mut self, c: char) {
        if self.peek() == Some(c) {
            self.pos = self.pos.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lin(s: &str) -> String {
        to_linear(&parse(s))
    }

    #[test]
    fn linear_math_parses_into_structured_nodes_and_back() {
        // phase: char_run_round_trips
        {
            assert_eq!(lin("x+1"), "x+1");
            assert_eq!(lin("2*x-3"), "2*x-3");
        }
        // phase: power_builds_sup
        {
            let e = parse("x^2");
            assert_eq!(
                e.root,
                vec![
                    Node::Char('x'),
                    Node::Sup {
                        exp: vec![Node::Char('2')]
                    }
                ]
            );
            assert_eq!(to_linear(&e), "x^(2)");
        }
        // phase: fraction_builds_frac
        {
            let e = parse("(x+1)/(x-3)");
            match &e.root[0] {
                Node::Frac { num, den } => {
                    assert_eq!(to_linear(&MathExpr { root: num.clone() }), "x+1");
                    assert_eq!(to_linear(&MathExpr { root: den.clone() }), "x-3");
                }
                other => panic!("expected frac, got {other:?}"),
            }
        }
        // phase: sqrt_builds_radical
        {
            let e = parse("sqrt(x+1)");
            match &e.root[0] {
                Node::Sqrt {
                    index: None,
                    radicand,
                } => {
                    assert_eq!(
                        to_linear(&MathExpr {
                            root: radicand.clone()
                        }),
                        "x+1"
                    );
                }
                other => panic!("expected sqrt, got {other:?}"),
            }
        }
        // phase: function_call_builds_func
        {
            let e = parse("sin(x)");
            match &e.root[0] {
                Node::Func { name, arg } => {
                    assert_eq!(name, "sin");
                    assert_eq!(to_linear(&MathExpr { root: arg.clone() }), "x");
                }
                other => panic!("expected func, got {other:?}"),
            }
        }
        // phase: nested_root_index
        {
            let e = parse("root(3,x)");
            match &e.root[0] {
                Node::Sqrt {
                    index: Some(i),
                    radicand,
                } => {
                    assert_eq!(to_linear(&MathExpr { root: i.clone() }), "3");
                    assert_eq!(
                        to_linear(&MathExpr {
                            root: radicand.clone()
                        }),
                        "x"
                    );
                }
                other => panic!("expected indexed sqrt, got {other:?}"),
            }
        }
    }
}
