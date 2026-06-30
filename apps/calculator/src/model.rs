//! In-memory model for the calculator. This is the render/interaction
//! mirror of the CRDT document (see `doc.rs`); state is authored here and
//! flushed into Automerge so undo/redo and live collaboration work.

use crate::engine::{self, Ctx};

/// Top-level app mode (segmented control in the header).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Calc,
    Graph2d,
    Graph3d,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Calc => "Calculator",
            Mode::Graph2d => "Graph 2D",
            Mode::Graph3d => "Graph 3D",
        }
    }
    pub fn all() -> [Mode; 3] {
        [Mode::Calc, Mode::Graph2d, Mode::Graph3d]
    }
    pub fn index(self) -> usize {
        match self {
            Mode::Calc => 0,
            Mode::Graph2d => 1,
            Mode::Graph3d => 2,
        }
    }
    pub fn from_index(i: usize) -> Mode {
        match i {
            1 => Mode::Graph2d,
            2 => Mode::Graph3d,
            _ => Mode::Calc,
        }
    }
}

/// One evaluated entry in the calculator history.
#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub expr: String,
    pub result: String,
}

/// A `y = f(x)` curve for the 2D plotter.
#[derive(Clone, Debug)]
pub struct PlotFn {
    pub expr: String,
    pub visible: bool,
}

/// 2D viewport: world-space center + pixels-per-unit zoom.
#[derive(Clone, Copy, Debug)]
pub struct View2d {
    pub cx: f64,
    pub cy: f64,
    pub scale: f64,
}

impl Default for View2d {
    fn default() -> Self {
        Self { cx: 0.0, cy: 0.0, scale: 64.0 }
    }
}

/// 3D viewport: orbit angles + zoom.
#[derive(Clone, Copy, Debug)]
pub struct View3d {
    pub yaw: f64,
    pub pitch: f64,
    pub zoom: f64,
}

impl Default for View3d {
    fn default() -> Self {
        Self { yaw: 0.62, pitch: 0.52, zoom: 1.0 }
    }
}

/// The whole calculator state.
#[derive(Clone, Debug)]
pub struct CalcModel {
    pub mode: Mode,
    pub deg: bool,
    pub expr: String,
    /// After `=`, the next value-key starts fresh; an operator continues.
    pub after_equals: bool,
    pub ans: f64,
    pub memory: f64,
    pub history: Vec<HistoryEntry>,
    pub funcs: Vec<PlotFn>,
    pub active_fn: usize,
    pub surface: String,
    pub view2d: View2d,
    pub view3d: View3d,
}

impl Default for CalcModel {
    fn default() -> Self {
        Self {
            mode: Mode::Calc,
            deg: false,
            expr: String::new(),
            after_equals: false,
            ans: 0.0,
            memory: 0.0,
            history: Vec::new(),
            funcs: Vec::new(),
            active_fn: 0,
            surface: "sin(sqrt(x^2+y^2))".to_string(),
            view2d: View2d::default(),
            view3d: View3d::default(),
        }
    }
}

impl CalcModel {
    /// Rich starting content so a fresh launch (and the `--shots` frame)
    /// shows a populated, lived-in calculator + graphs.
    pub fn seeded() -> Self {
        let mut m = CalcModel::default();
        m.mode = Mode::Graph2d;
        m.expr = "sin(0.5)+cos(0.5)".to_string();
        m.ans = 1.0;
        m.history = vec![
            HistoryEntry { expr: "12*(4+3)".into(), result: "84".into() },
            HistoryEntry { expr: "sqrt(2)".into(), result: "1.4142135624".into() },
            HistoryEntry { expr: "5!".into(), result: "120".into() },
            HistoryEntry { expr: "log(2,1024)".into(), result: "10".into() },
            HistoryEntry { expr: "2^10".into(), result: "1024".into() },
        ];
        m.funcs = vec![
            PlotFn { expr: "(x^2+1)/(x-3)".into(), visible: true },
            PlotFn { expr: "0.25*x^2-2".into(), visible: true },
            PlotFn { expr: "sqrt(x+6)".into(), visible: true },
            PlotFn { expr: "sin(x)".into(), visible: true },
        ];
        m.active_fn = 0;
        m.memory = 42.0;
        m
    }

    /// Evaluation context derived from the current model.
    pub fn ctx(&self) -> Ctx {
        Ctx { deg: self.deg, ans: self.ans, mem: self.memory, ..Default::default() }
    }

    /// Live preview of the current expression (grey under the input).
    pub fn preview(&self) -> Option<String> {
        if self.expr.trim().is_empty() {
            return None;
        }
        engine::evaluate(&self.expr, &self.ctx())
            .ok()
            .map(engine::format_number)
    }

    /// Commit `=`: evaluate, push history, update Ans.
    pub fn commit_equals(&mut self) {
        let trimmed = self.expr.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        match engine::evaluate(&trimmed, &self.ctx()) {
            Ok(v) => {
                let result = engine::format_number(v);
                self.history.insert(
                    0,
                    HistoryEntry { expr: trimmed, result: result.clone() },
                );
                self.history.truncate(64);
                self.ans = v;
                self.expr = result;
                self.after_equals = true;
            }
            Err(_) => {
                // Leave the expression intact; the result line shows the error.
                self.after_equals = false;
            }
        }
    }

    /// The currently-active 2D function expression, if any.
    pub fn active_expr(&self) -> Option<&str> {
        self.funcs.get(self.active_fn).map(|f| f.expr.as_str())
    }
}

/// A pressable key on the calculator pad.
#[derive(Clone, Debug)]
pub enum Key {
    Digit(char),
    Dot,
    DoubleZero,
    /// Insert a raw ASCII fragment at the caret (operators, `(`, `sin(`…).
    Ins(&'static str),
    /// Apply a unary wrap to the whole current expression.
    Wrap(&'static str, &'static str),
    Percent,
    ToggleSign,
    Equals,
    Clear,
    Backspace,
    Ans,
    MemAdd,
    MemSub,
    MemRecall,
    MemClear,
}

impl Key {
    /// Glyph shown on the keycap.
    pub fn label(&self) -> &'static str {
        match self {
            Key::Digit('0') => "0",
            Key::Digit('1') => "1",
            Key::Digit('2') => "2",
            Key::Digit('3') => "3",
            Key::Digit('4') => "4",
            Key::Digit('5') => "5",
            Key::Digit('6') => "6",
            Key::Digit('7') => "7",
            Key::Digit('8') => "8",
            Key::Digit('9') => "9",
            Key::Digit(_) => "?",
            Key::Dot => ".",
            Key::DoubleZero => "00",
            Key::Percent => "%",
            Key::ToggleSign => "±",
            Key::Equals => "=",
            Key::Clear => "AC",
            Key::Backspace => "⌫",
            Key::Ans => "ans",
            Key::MemAdd => "M+",
            Key::MemSub => "M−",
            Key::MemRecall => "MR",
            Key::MemClear => "MC",
            Key::Wrap(_, _) => "x²",
            Key::Ins(s) => match *s {
                "+" => "+",
                "-" => "−",
                "*" => "×",
                "/" => "÷",
                "^" => "xʸ",
                "(" => "(",
                ")" => ")",
                "," => ",",
                "sin(" => "sin",
                "cos(" => "cos",
                "tan(" => "tan",
                "asin(" => "asin",
                "acos(" => "acos",
                "atan(" => "atan",
                "ln(" => "ln",
                "log10(" => "log",
                "sqrt(" => "√",
                "exp(" => "eˣ",
                "1/(" => "1/x",
                "pi" => "π",
                "e" => "e",
                "mod(" => "mod",
                _ => s,
            },
        }
    }

    /// Visual category → keycap tint family.
    pub fn role(&self) -> KeyRole {
        match self {
            Key::Digit(_) | Key::Dot | Key::DoubleZero => KeyRole::Digit,
            Key::Equals => KeyRole::Accent,
            Key::Clear | Key::Backspace => KeyRole::Warm,
            Key::Ins("+") | Key::Ins("-") | Key::Ins("*") | Key::Ins("/") => {
                KeyRole::Operator
            }
            _ => KeyRole::Function,
        }
    }
}

/// Tint family for a keycap, resolved against the theme at draw time.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum KeyRole {
    Digit,
    Operator,
    Function,
    Accent,
    Warm,
}

/// The 6-column scientific keypad. Row-major; `None` marks an empty cell.
pub const KEYPAD_COLS: usize = 6;

pub fn keypad() -> Vec<Key> {
    use Key::*;
    vec![
        // Row 1
        Clear, Backspace, Ins("("), Ins(")"), Ins("mod("), Ins("!"),
        // Row 2
        Ins("sin("), Ins("cos("), Ins("tan("), Ins("pi"), Ins("e"), Percent,
        // Row 3
        Ins("asin("), Ins("acos("), Ins("atan("), Ins("^"), Ins("sqrt("), Ins("/"),
        // Row 4
        Ins("ln("), Ins("log10("), Digit('7'), Digit('8'), Digit('9'), Ins("*"),
        // Row 5
        Ins("exp("), Ins("1/("), Digit('4'), Digit('5'), Digit('6'), Ins("-"),
        // Row 6
        Wrap("(", ")^2"), ToggleSign, Digit('1'), Digit('2'), Digit('3'), Ins("+"),
        // Row 7
        DoubleZero, Ans, Digit('0'), Dot, Ins(","), Equals,
    ]
}

/// Apply a key press to the model (calculator mode).
pub fn apply_key(m: &mut CalcModel, key: &Key) {
    match key {
        Key::Clear => {
            m.expr.clear();
            m.after_equals = false;
        }
        Key::Backspace => {
            m.after_equals = false;
            // Pop one token-ish chunk: a trailing `name(` deletes whole.
            pop_token(&mut m.expr);
        }
        Key::Equals => m.commit_equals(),
        Key::ToggleSign => {
            if m.expr.trim().is_empty() {
                m.expr = "-".into();
            } else if m.expr.starts_with('-') {
                m.expr.remove(0);
            } else {
                m.expr = format!("-({})", m.expr);
            }
            m.after_equals = false;
        }
        Key::Percent => {
            m.expr.push('%');
            m.after_equals = false;
        }
        Key::Ans => start_value(m, "ans"),
        Key::Digit(c) => {
            let s = c.to_string();
            start_value(m, &s);
        }
        Key::Dot => start_value(m, "."),
        Key::DoubleZero => start_value(m, "00"),
        Key::Ins(s) => {
            // Operators + postfix/closers continue from a committed result;
            // value-starters (`(`, `sin(`, `pi`, …) begin fresh after `=`.
            let continues = matches!(*s, "+" | "-" | "*" | "/" | "^" | "!" | ")" | ",");
            if continues {
                m.after_equals = false;
                m.expr.push_str(s);
            } else {
                start_value(m, s);
            }
        }
        Key::Wrap(pre, post) => {
            if m.expr.trim().is_empty() {
                // Nothing to wrap.
            } else {
                m.expr = format!("{pre}{}{post}", m.expr);
            }
            m.after_equals = false;
        }
        Key::MemAdd => {
            if let Ok(v) = engine::evaluate(&m.expr, &m.ctx()) {
                m.memory += v;
            }
        }
        Key::MemSub => {
            if let Ok(v) = engine::evaluate(&m.expr, &m.ctx()) {
                m.memory -= v;
            }
        }
        Key::MemRecall => start_value(m, &engine::format_number(m.memory)),
        Key::MemClear => m.memory = 0.0,
    }
}

/// Route a typed character through the calculator (keyboard entry).
pub fn type_char(m: &mut CalcModel, ch: char) {
    match ch {
        '0'..='9' => apply_key(m, &Key::Digit(ch)),
        '.' => apply_key(m, &Key::Dot),
        '+' => apply_key(m, &Key::Ins("+")),
        '-' => apply_key(m, &Key::Ins("-")),
        '*' => apply_key(m, &Key::Ins("*")),
        '/' => apply_key(m, &Key::Ins("/")),
        '^' => apply_key(m, &Key::Ins("^")),
        '(' => apply_key(m, &Key::Ins("(")),
        ')' => apply_key(m, &Key::Ins(")")),
        ',' => apply_key(m, &Key::Ins(",")),
        '!' => apply_key(m, &Key::Ins("!")),
        '%' => apply_key(m, &Key::Percent),
        '=' => apply_key(m, &Key::Equals),
        c if c.is_ascii_alphabetic() => start_value(m, &c.to_ascii_lowercase().to_string()),
        _ => {}
    }
}

/// Insert a "value" fragment, clearing a just-committed result first.
fn start_value(m: &mut CalcModel, s: &str) {
    if m.after_equals {
        m.expr.clear();
        m.after_equals = false;
    }
    m.expr.push_str(s);
}

/// Delete one trailing logical token from `expr`.
fn pop_token(expr: &mut String) {
    if expr.is_empty() {
        return;
    }
    // If it ends with `name(`, drop the whole identifier + paren.
    if expr.ends_with('(') {
        expr.pop();
        while let Some(c) = expr.chars().last() {
            if c.is_ascii_alphanumeric() || c == '_' {
                expr.pop();
            } else {
                break;
            }
        }
        return;
    }
    expr.pop();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equals_pushes_history_and_sets_ans() {
        let mut m = CalcModel::default();
        m.expr = "2+3".into();
        m.commit_equals();
        assert_eq!(m.ans, 5.0);
        assert_eq!(m.history[0].result, "5");
        assert_eq!(m.expr, "5");
        assert!(m.after_equals);
    }

    #[test]
    fn digit_after_equals_starts_fresh() {
        let mut m = CalcModel::default();
        m.expr = "5".into();
        m.after_equals = true;
        apply_key(&mut m, &Key::Digit('7'));
        assert_eq!(m.expr, "7");
    }

    #[test]
    fn operator_after_equals_continues() {
        let mut m = CalcModel::default();
        m.expr = "5".into();
        m.after_equals = true;
        apply_key(&mut m, &Key::Ins("+"));
        assert_eq!(m.expr, "5+");
    }

    #[test]
    fn backspace_drops_function_name() {
        let mut m = CalcModel::default();
        m.expr = "2+sin(".into();
        apply_key(&mut m, &Key::Backspace);
        assert_eq!(m.expr, "2+");
    }

    #[test]
    fn memory_add_accumulates() {
        let mut m = CalcModel::default();
        m.expr = "10".into();
        apply_key(&mut m, &Key::MemAdd);
        apply_key(&mut m, &Key::MemAdd);
        assert_eq!(m.memory, 20.0);
    }

    #[test]
    fn keypad_is_full_grid() {
        assert_eq!(keypad().len() % KEYPAD_COLS, 0);
    }
}
