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
//! The model serializes to a flat ASCII expression the existing
//! [`crate::engine`] parses, and parses such a string back into a tree, so
//! graphing keeps working unchanged and paste round-trips.

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
    /// A radical: `√` with an overbar spanning `radicand`, optional `index`
    /// (the small number in the crook, e.g. cube root).
    Sqrt { index: Option<Row>, radicand: Row },
    /// A parenthesized group `( body )`.
    Paren { body: Row },
    /// A function call `name( arg )` — `name` is upright (sin, cos, ln, …).
    Func { name: String, arg: Row },
}

/// An ordered list of atoms. The editable unit.
pub type Row = Vec<Node>;

/// Which child row of a structure a path step descends into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Num,
    Den,
    Exp,
    Radicand,
    Index,
    Body,
    Arg,
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
        (Node::Sqrt { radicand, .. }, Slot::Radicand) => Some(radicand),
        (Node::Sqrt { index: Some(i), .. }, Slot::Index) => Some(i),
        (Node::Paren { body }, Slot::Body) => Some(body),
        (Node::Func { arg, .. }, Slot::Arg) => Some(arg),
        _ => None,
    }
}

pub fn child_row_mut(node: &mut Node, slot: Slot) -> Option<&mut Row> {
    match (node, slot) {
        (Node::Frac { num, .. }, Slot::Num) => Some(num),
        (Node::Frac { den, .. }, Slot::Den) => Some(den),
        (Node::Sup { exp }, Slot::Exp) => Some(exp),
        (Node::Sqrt { radicand, .. }, Slot::Radicand) => Some(radicand),
        (Node::Sqrt { index: Some(i), .. }, Slot::Index) => Some(i),
        (Node::Paren { body }, Slot::Body) => Some(body),
        (Node::Func { arg, .. }, Slot::Arg) => Some(arg),
        _ => None,
    }
}

/// Slots a node exposes, in caret Up/Down navigation order (top → bottom).
pub fn node_slots(node: &Node) -> Vec<Slot> {
    match node {
        Node::Frac { .. } => vec![Slot::Num, Slot::Den],
        Node::Sup { .. } => vec![Slot::Exp],
        Node::Sqrt { index: Some(_), .. } => vec![Slot::Index, Slot::Radicand],
        Node::Sqrt { index: None, .. } => vec![Slot::Radicand],
        Node::Paren { .. } => vec![Slot::Body],
        Node::Func { .. } => vec![Slot::Arg],
        Node::Char(_) => vec![],
    }
}

// ---------------------------------------------------------------------------
// Serialization: model → linear ASCII the engine parses.
// ---------------------------------------------------------------------------

/// Serialize the whole expression to a flat ASCII string understood by
/// [`crate::engine`]. Round-trips with [`parse`].
pub fn to_linear(expr: &MathExpr) -> String {
    let mut out = String::new();
    write_row(&expr.root, &mut out);
    out
}

fn write_row(row: &Row, out: &mut String) {
    let mut i = 0;
    while i < row.len() {
        match &row[i] {
            Node::Char(c) => out.push(*c),
            Node::Sup { exp } => {
                out.push('^');
                out.push('(');
                write_row(exp, out);
                out.push(')');
            }
            Node::Frac { num, den } => {
                out.push('(');
                write_row(num, out);
                out.push(')');
                out.push('/');
                out.push('(');
                write_row(den, out);
                out.push(')');
            }
            Node::Sqrt { index, radicand } => {
                if let Some(idx) = index {
                    out.push_str("root(");
                    write_row(idx, out);
                    out.push(',');
                    write_row(radicand, out);
                    out.push(')');
                } else {
                    out.push_str("sqrt(");
                    write_row(radicand, out);
                    out.push(')');
                }
            }
            Node::Paren { body } => {
                out.push('(');
                write_row(body, out);
                out.push(')');
            }
            Node::Func { name, arg } => {
                out.push_str(name);
                out.push('(');
                write_row(arg, out);
                out.push(')');
            }
        }
        i += 1;
    }
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
                    self.pos += 1;
                    let exp = self.parse_superscript();
                    row.push(Node::Sup { exp });
                }
                '/' => {
                    self.pos += 1;
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
                    self.pos += 1;
                    let body = self.parse_row(&[')']);
                    self.expect(')');
                    row.push(Node::Paren { body });
                }
                c if c.is_ascii_alphabetic() => {
                    let name = self.read_ident();
                    if self.peek() == Some('(') {
                        self.pos += 1;
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
                                self.pos += 1;
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
                    self.pos += 1;
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
            self.pos += 1;
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
                self.pos += 1;
                let body = self.parse_row(&[')']);
                self.expect(')');
                vec![Node::Paren { body }]
            }
            Some('-') => {
                self.pos += 1;
                let mut r = vec![Node::Char('-')];
                r.extend(self.parse_operand_row());
                r
            }
            Some(c) if c.is_ascii_digit() || c == '.' => {
                let mut r = Vec::new();
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() || c == '.' {
                        r.push(Node::Char(c));
                        self.pos += 1;
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
                    let consumed = sub.pos.saturating_sub(name.chars().count() + 1);
                    self.pos += consumed;
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
        self.chars[self.pos..].iter().copied()
    }

    fn read_ident(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        self.chars[start..self.pos].iter().collect()
    }

    fn expect(&mut self, c: char) {
        if self.peek() == Some(c) {
            self.pos += 1;
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
    fn char_run_round_trips() {
        assert_eq!(lin("x+1"), "x+1");
        assert_eq!(lin("2*x-3"), "2*x-3");
    }

    #[test]
    fn power_builds_sup() {
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

    #[test]
    fn fraction_builds_frac() {
        let e = parse("(x+1)/(x-3)");
        match &e.root[0] {
            Node::Frac { num, den } => {
                assert_eq!(to_linear(&MathExpr { root: num.clone() }), "x+1");
                assert_eq!(to_linear(&MathExpr { root: den.clone() }), "x-3");
            }
            other => panic!("expected frac, got {other:?}"),
        }
    }

    #[test]
    fn sqrt_builds_radical() {
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

    #[test]
    fn function_call_builds_func() {
        let e = parse("sin(x)");
        match &e.root[0] {
            Node::Func { name, arg } => {
                assert_eq!(name, "sin");
                assert_eq!(to_linear(&MathExpr { root: arg.clone() }), "x");
            }
            other => panic!("expected func, got {other:?}"),
        }
    }

    #[test]
    fn complex_formula_parses_to_engine_compatible() {
        // (x^2 + 1) / (x - 3)
        let e = parse("(x^2+1)/(x-3)");
        let linear = to_linear(&e);
        // engine must accept it
        assert!(
            crate::engine::compile(&linear, false, false).is_ok(),
            "linear={linear}"
        );
    }

    #[test]
    fn nested_root_index() {
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

    #[test]
    fn seeded_funcs_round_trip_through_engine() {
        for s in [
            "sin(x)",
            "0.25*x^2-2",
            "1/x",
            "(x^2+1)/(x-3)",
            "sqrt(x^2+1)",
        ] {
            let linear = to_linear(&parse(s));
            assert!(
                crate::engine::compile(&linear, false, false).is_ok(),
                "{s} -> {linear} did not compile"
            );
        }
    }
}
