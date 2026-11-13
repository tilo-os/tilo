//! The calculator's in-memory state: authored here and committed as actions,
//! so undo and collaboration see the same record.

use crate::actions::GraphKind;
use crate::engine::{self, Ctx};
use ui::tilo_resource::{self, Model};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Calc,
    Convert,
    Graph2d,
    Graph3d,
}

impl Mode {
    pub fn label(self) -> String {
        let message = match self {
            Mode::Calc => "calculator-mode-calculator",
            Mode::Convert => "calculator-mode-convert",
            Mode::Graph2d => "calculator-mode-graph-2d",
            Mode::Graph3d => "calculator-mode-graph-3d",
        };
        crate::resource::CATALOG.t(message)
    }
    pub fn all() -> [Mode; 4] {
        [Mode::Calc, Mode::Graph2d, Mode::Graph3d, Mode::Convert]
    }
    /// A saved view names modes by index, so these values must not move.
    pub fn index(self) -> usize {
        match self {
            Mode::Calc => 0,
            Mode::Graph2d => 1,
            Mode::Graph3d => 2,
            Mode::Convert => 3,
        }
    }
    pub fn from_index(i: usize) -> Mode {
        match i {
            1 => Mode::Graph2d,
            2 => Mode::Graph3d,
            3 => Mode::Convert,
            _ => Mode::Calc,
        }
    }
    pub fn graph(self) -> Option<GraphKind> {
        match self {
            Mode::Calc | Mode::Convert => None,
            Mode::Graph2d => Some(GraphKind::Curve2d),
            Mode::Graph3d => Some(GraphKind::Surface3d),
        }
    }
}

/// The conversion a capture shows.
pub const CONVERT_DEMO_AMOUNT: &str = "40";

#[derive(Model, Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    /// Calculation
    #[model(label)]
    pub expr: String,
    /// Result
    pub result: String,
}

#[derive(Model, Clone, Debug, PartialEq)]
pub struct PlotFn {
    /// Formula
    #[model(label)]
    pub expr: String,
    /// Shown
    pub visible: bool,
}

/// World-space centre and pixels per unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View2d {
    pub cx: f64,
    pub cy: f64,
    pub scale: f64,
}

impl Default for View2d {
    fn default() -> Self {
        Self {
            cx: 0.0,
            cy: 0.0,
            scale: 64.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View3d {
    pub yaw: f64,
    pub pitch: f64,
    pub zoom: f64,
}

impl Default for View3d {
    fn default() -> Self {
        Self {
            yaw: 0.62,
            pitch: 0.52,
            zoom: 1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CalcModel {
    pub mode: Mode,
    pub deg: bool,
    pub expr: String,
    /// After `=`, a value key starts fresh and an operator continues.
    pub after_equals: bool,
    pub ans: f64,
    pub memory: f64,
    pub history: Vec<HistoryEntry>,
    pub funcs: Vec<PlotFn>,
    pub active_fn: usize,
    pub surfaces: Vec<PlotFn>,
    pub active_surface: usize,
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
            surfaces: Vec::new(),
            active_surface: 0,
            view2d: View2d::default(),
            view3d: View3d::default(),
        }
    }
}

/// The starter surface Graph 3D opens with; not restored state.
pub const DEFAULT_SURFACE: &str = "sin(sqrt(x^2+y^2))";

/// Newest first: the expression and its result.
pub const DEMO_HISTORY: [(&str, &str); 5] = [
    ("12*(4+3)", "84"),
    ("sqrt(2)", "1.4142135624"),
    ("5!", "120"),
    ("log(2,1024)", "10"),
    ("2^10", "1024"),
];

pub const DEMO_CURVES: [&str; 4] = ["(x^2+1)/(x-3)", "0.25*x^2-2", "sqrt(x+6)", "sin(x)"];

/// Sheets at different heights that never cross.
pub const DEMO_SURFACES: [&str; 2] = [DEFAULT_SURFACE, "sin(x)*cos(y)-3"];

impl CalcModel {
    pub fn graphs(&self, kind: GraphKind) -> &[PlotFn] {
        match kind {
            GraphKind::Curve2d => &self.funcs,
            GraphKind::Surface3d => &self.surfaces,
        }
    }

    pub fn graphs_mut(&mut self, kind: GraphKind) -> &mut Vec<PlotFn> {
        match kind {
            GraphKind::Curve2d => &mut self.funcs,
            GraphKind::Surface3d => &mut self.surfaces,
        }
    }

    pub fn active_graph(&self, kind: GraphKind) -> usize {
        match kind {
            GraphKind::Curve2d => self.active_fn,
            GraphKind::Surface3d => self.active_surface,
        }
    }

    pub fn active_graph_mut(&mut self, kind: GraphKind) -> &mut usize {
        match kind {
            GraphKind::Curve2d => &mut self.active_fn,
            GraphKind::Surface3d => &mut self.active_surface,
        }
    }

    /// Lived-in content for tests and offline demos; the live app boots empty.
    pub fn seeded() -> Self {
        let plot = |expr: &str| PlotFn {
            expr: expr.to_string(),
            visible: true,
        };
        CalcModel {
            mode: Mode::Graph2d,
            expr: "sin(0.5)+cos(0.5)".to_string(),
            ans: 1.0,
            memory: 42.0,
            history: DEMO_HISTORY
                .iter()
                .map(|(expr, result)| HistoryEntry {
                    expr: (*expr).into(),
                    result: (*result).into(),
                })
                .collect(),
            funcs: DEMO_CURVES.iter().map(|expr| plot(expr)).collect(),
            active_fn: 0,
            surfaces: DEMO_SURFACES.iter().map(|expr| plot(expr)).collect(),
            active_surface: 0,
            ..CalcModel::default()
        }
    }

    /// The parse error a capture shows.
    pub(crate) fn apply_error_demo(&mut self) {
        self.mode = Mode::Calc;
        self.expr = "sqrt(".into();
        self.after_equals = false;
    }

    pub fn ctx(&self) -> Ctx {
        Ctx {
            deg: self.deg,
            ans: self.ans,
            mem: self.memory,
            ..Default::default()
        }
    }

    /// Returns the entry so the caller can submit `commit-history`; `None` when
    /// the expression is empty or unparseable, so no action is dispatched.
    pub fn commit_equals(&mut self) -> Option<HistoryEntry> {
        let trimmed = self.expr.trim().to_string();
        if trimmed.is_empty() {
            return None;
        }
        // `=` again on a shown result would record a row like "-0.26 = -0.26".
        if self.after_equals
            && self
                .history
                .first()
                .is_some_and(|last| last.result == trimmed)
        {
            return None;
        }
        if let Ok(v) = engine::evaluate(&trimmed, &self.ctx()) {
            let result = engine::format_number(v);
            let entry = HistoryEntry {
                expr: trimmed,
                result: result.clone(),
            };
            self.history.insert(0, entry.clone());
            self.history.truncate(64);
            self.ans = v;
            self.expr = result;
            self.after_equals = true;
            Some(entry)
        } else {
            // The result line shows the error; the expression stays.
            self.after_equals = false;
            None
        }
    }
}

#[derive(Clone, Debug)]
pub enum Key {
    Digit(char),
    Dot,
    DoubleZero,
    Ins(&'static str),
    /// Wraps the whole expression, not the caret.
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
    Page(KeypadPage),
}

/// The two pages of the narrow keypad: a phone-width window shows one page at a
/// time rather than dropping keys. A wide window needs neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeypadPage {
    Primary,
    Functions,
}

impl Key {
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
            Key::Backspace => "Backspace",
            Key::Ans => "ans",
            Key::MemAdd => "M+",
            Key::MemSub => "M−",
            Key::MemRecall => "MR",
            Key::MemClear => "MC",
            Key::Page(KeypadPage::Primary) => "123",
            Key::Page(KeypadPage::Functions) => "fx",
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
                "int(" => "∫",
                "root(" => "ⁿ√",
                "abs(" => "|x|",
                "sinh(" => "sinh",
                "cosh(" => "cosh",
                "tanh(" => "tanh",
                "gamma(" => "Γ",
                "exp(" => "eˣ",
                "1/(" => "1/x",
                "pi" => "π",
                "e" => "e",
                "mod(" => "mod",
                _ => s,
            },
        }
    }

    pub fn icon(&self) -> Option<ui::icons::Icon> {
        match self {
            Key::Backspace => Some(ui::icons::Icon::Delete),
            _ => None,
        }
    }

    pub fn role(&self) -> KeyRole {
        match self {
            Key::Digit(_) | Key::Dot | Key::DoubleZero => KeyRole::Digit,
            Key::Equals => KeyRole::Equals,
            Key::Clear => KeyRole::Clear,
            // Navigation, not a function: it gets the operator's lifted face.
            Key::Backspace | Key::Ins("+" | "-" | "*" | "/") | Key::Page(_) => KeyRole::Operator,
            _ => KeyRole::Function,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum KeyRole {
    Digit,
    Operator,
    Function,
    Equals,
    Clear,
}

/// How dense the keypad is, chosen from the body width. The layouts are nested
/// supersets: every key on a smaller layout sits in the same relative place on a
/// larger one, so the digits and operators never move.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeypadLayout {
    Compact,
    Standard,
    Scientific,
}

impl KeypadLayout {
    pub fn for_width(width: f64) -> Self {
        let compact = width < 460.0; // allow-breakpoint: re-chosen from the laid-out rect on every pass
        let standard = width < 680.0; // allow-breakpoint: re-chosen from the laid-out rect on every pass
        if compact {
            KeypadLayout::Compact
        } else if standard {
            KeypadLayout::Standard
        } else {
            KeypadLayout::Scientific
        }
    }

    pub fn cols(self) -> usize {
        match self {
            KeypadLayout::Compact => 4,
            KeypadLayout::Standard => 5,
            KeypadLayout::Scientific => 7,
        }
    }

    pub fn keys(self) -> Vec<Key> {
        self.keys_on(KeypadPage::Primary)
    }

    /// Only the compact layout pages; the wider ones ignore the page.
    pub fn keys_on(self, page: KeypadPage) -> Vec<Key> {
        match (self, page) {
            (KeypadLayout::Compact, KeypadPage::Functions) => Self::compact_function_keys(),
            (KeypadLayout::Compact, KeypadPage::Primary) => Self::compact_keys(),
            (KeypadLayout::Standard, _) => Self::standard_keys(),
            (KeypadLayout::Scientific, _) => Self::scientific_keys(),
        }
    }

    pub fn pages(self) -> bool {
        self == KeypadLayout::Compact
    }

    /// 4 × 6: the essentials and the door to the function page.
    fn compact_keys() -> Vec<Key> {
        use Key::{Ans, Backspace, Clear, Digit, Dot, Equals, Ins, Page, Percent, ToggleSign};
        vec![
            Clear,
            Backspace,
            Ins("("),
            Ins(")"),
            Percent,
            ToggleSign,
            Ans,
            Page(KeypadPage::Functions),
            Digit('7'),
            Digit('8'),
            Digit('9'),
            Ins("/"),
            Digit('4'),
            Digit('5'),
            Digit('6'),
            Ins("*"),
            Digit('1'),
            Digit('2'),
            Digit('3'),
            Ins("-"),
            Digit('0'),
            Dot,
            Equals,
            Ins("+"),
        ]
    }

    /// 4 × 7: every scientific key the number page leaves out, plus `123`.
    fn compact_function_keys() -> Vec<Key> {
        use Key::{Ans, DoubleZero, Ins, Page, Wrap};
        vec![
            Page(KeypadPage::Primary),
            Ins("sin("),
            Ins("cos("),
            Ins("tan("),
            Ins("pi"),
            Ins("e"),
            Ins("asin("),
            Ins("acos("),
            Ins("atan("),
            Ins("sinh("),
            Ins("cosh("),
            Ins("tanh("),
            Ins("ln("),
            Ins("log10("),
            Ins("exp("),
            Ins("sqrt("),
            Wrap("(", ")^2"),
            Ins("^"),
            Ins("1/("),
            Ins("abs("),
            Ins("mod("),
            Ins("!"),
            Ins("root("),
            Ins("int("),
            Ins("gamma("),
            DoubleZero,
            Ans,
            Ins(","),
        ]
    }

    /// 5 × 7: essentials plus the common scientific keys.
    fn standard_keys() -> Vec<Key> {
        use Key::{
            Ans, Backspace, Clear, Digit, Dot, DoubleZero, Equals, Ins, Percent, ToggleSign, Wrap,
        };
        vec![
            Clear,
            Backspace,
            Ins("("),
            Ins(")"),
            Ins("!"),
            Ins("sin("),
            Ins("cos("),
            Ins("tan("),
            Ins("pi"),
            Ins("e"),
            Ins("ln("),
            Ins("^"),
            Ins("sqrt("),
            Percent,
            Ins("/"),
            Wrap("(", ")^2"),
            Digit('7'),
            Digit('8'),
            Digit('9'),
            Ins("*"),
            Ins("1/("),
            Digit('4'),
            Digit('5'),
            Digit('6'),
            Ins("-"),
            ToggleSign,
            Digit('1'),
            Digit('2'),
            Digit('3'),
            Ins("+"),
            Ans,
            DoubleZero,
            Digit('0'),
            Dot,
            Equals,
        ]
    }

    /// 7 × 7: the full pad, with a leading column of integration and roots.
    fn scientific_keys() -> Vec<Key> {
        use Key::{
            Ans, Backspace, Clear, Digit, Dot, DoubleZero, Equals, Ins, Percent, ToggleSign, Wrap,
        };
        vec![
            Ins("int("),
            Clear,
            Backspace,
            Ins("("),
            Ins(")"),
            Ins("mod("),
            Ins("!"),
            Ins("root("),
            Ins("sin("),
            Ins("cos("),
            Ins("tan("),
            Ins("pi"),
            Ins("e"),
            Percent,
            Ins("abs("),
            Ins("asin("),
            Ins("acos("),
            Ins("atan("),
            Ins("^"),
            Ins("sqrt("),
            Ins("/"),
            Ins("sinh("),
            Ins("ln("),
            Ins("log10("),
            Digit('7'),
            Digit('8'),
            Digit('9'),
            Ins("*"),
            Ins("cosh("),
            Ins("exp("),
            Ins("1/("),
            Digit('4'),
            Digit('5'),
            Digit('6'),
            Ins("-"),
            Ins("tanh("),
            Wrap("(", ")^2"),
            ToggleSign,
            Digit('1'),
            Digit('2'),
            Digit('3'),
            Ins("+"),
            Ins("gamma("),
            DoubleZero,
            Ans,
            Digit('0'),
            Dot,
            Ins(","),
            Equals,
        ]
    }
}

pub fn apply_key(m: &mut CalcModel, key: &Key) {
    match key {
        Key::Clear => {
            m.expr.clear();
            m.after_equals = false;
        }
        Key::Backspace => {
            m.after_equals = false;
            // A trailing `name(` goes whole.
            pop_token(&mut m.expr);
        }
        Key::Equals => {
            m.commit_equals();
        }
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
            // Operators and closers continue a shown result; value starters
            // begin fresh.
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
            } else {
                m.expr = format!("{pre}{}{post}", m.expr);
            }
            m.after_equals = false;
        }
        // The screen owns which page shows.
        Key::Page(_) => {}
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

fn start_value(m: &mut CalcModel, s: &str) {
    if m.after_equals {
        m.expr.clear();
        m.after_equals = false;
    }
    m.expr.push_str(s);
}

fn pop_token(expr: &mut String) {
    if expr.is_empty() {
        return;
    }
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
    fn key_entry_and_equals_update_expression_memory_and_history() {
        // phase equals_pushes_history_and_sets_ans
        {
            let mut m = CalcModel {
                expr: "2+3".into(),
                ..CalcModel::default()
            };
            m.commit_equals();
            assert_eq!(m.ans, 5.0);
            assert_eq!(m.history[0].result, "5");
            assert_eq!(m.expr, "5");
            assert!(m.after_equals);
            assert!(m.commit_equals().is_none());
            assert_eq!(m.history.len(), 1);
            // sin(π) is exactly 0.
            m.expr = "sin(pi)".into();
            m.after_equals = false;
            m.commit_equals();
            assert_eq!(m.history[0].result, "0");
        }

        // phase digit_after_equals_starts_fresh
        {
            let mut m = CalcModel {
                expr: "5".into(),
                after_equals: true,
                ..CalcModel::default()
            };
            apply_key(&mut m, &Key::Digit('7'));
            assert_eq!(m.expr, "7");
        }

        // phase operator_after_equals_continues
        {
            let mut m = CalcModel {
                expr: "5".into(),
                after_equals: true,
                ..CalcModel::default()
            };
            apply_key(&mut m, &Key::Ins("+"));
            assert_eq!(m.expr, "5+");
        }

        // phase error_demo_is_calc_only_and_guarantees_a_renderable_parse_error
        {
            let mut m = CalcModel::seeded();
            m.after_equals = true;
            m.apply_error_demo();

            assert_eq!(m.mode, Mode::Calc);
            assert_eq!(m.expr, "sqrt(");
            assert!(!m.after_equals);
            assert!(engine::evaluate(&m.expr, &m.ctx()).is_err());
        }

        // phase backspace_drops_function_name
        {
            let mut m = CalcModel {
                expr: "2+sin(".into(),
                ..CalcModel::default()
            };
            apply_key(&mut m, &Key::Backspace);
            assert_eq!(m.expr, "2+");
        }

        // phase memory_add_accumulates
        {
            let mut m = CalcModel {
                expr: "10".into(),
                ..CalcModel::default()
            };
            apply_key(&mut m, &Key::MemAdd);
            apply_key(&mut m, &Key::MemAdd);
            assert_eq!(m.memory, 20.0);
        }
    }

    #[test]
    fn keypad_layouts_are_dense_grids_chosen_by_width_with_essentials() {
        // phase every_keypad_layout_is_a_dense_grid
        {
            for layout in [
                KeypadLayout::Compact,
                KeypadLayout::Standard,
                KeypadLayout::Scientific,
            ] {
                assert_eq!(
                    layout.keys().len() % layout.cols(),
                    0,
                    "{layout:?} keys must tile its column count exactly"
                );
            }
        }

        // phase keypad_breakpoints_pick_expected_layout
        {
            assert_eq!(KeypadLayout::for_width(380.0), KeypadLayout::Compact);
            assert_eq!(KeypadLayout::for_width(560.0), KeypadLayout::Standard);
            assert_eq!(KeypadLayout::for_width(900.0), KeypadLayout::Scientific);
            assert!(KeypadLayout::Compact.cols() < KeypadLayout::Standard.cols());
            assert!(KeypadLayout::Standard.cols() < KeypadLayout::Scientific.cols());
        }

        // phase every_layout_keeps_the_quick_calc_essentials
        {
            for layout in [
                KeypadLayout::Compact,
                KeypadLayout::Standard,
                KeypadLayout::Scientific,
            ] {
                let keys = layout.keys();
                for d in '0'..='9' {
                    assert!(
                        keys.iter().any(|k| matches!(k, Key::Digit(c) if *c == d)),
                        "{layout:?} missing digit {d}"
                    );
                }
                for op in ["+", "-", "*", "/"] {
                    assert!(
                        keys.iter().any(|k| matches!(k, Key::Ins(s) if *s == op)),
                        "{layout:?} missing operator {op}"
                    );
                }
                assert!(
                    keys.iter().any(|k| matches!(k, Key::Equals)),
                    "{layout:?} missing ="
                );
                assert!(
                    keys.iter().any(|k| matches!(k, Key::Clear)),
                    "{layout:?} missing AC"
                );
                assert!(
                    keys.iter().any(|k| matches!(k, Key::Backspace)),
                    "{layout:?} missing backspace"
                );
            }
        }
    }
    /// The two pages together hold every key the wide pad shows.
    #[test]
    fn compact_pages_together_hold_every_scientific_key() {
        let mut reachable = std::collections::HashSet::new();
        for page in [KeypadPage::Primary, KeypadPage::Functions] {
            let keys = KeypadLayout::Compact.keys_on(page);
            assert_eq!(keys.len() % KeypadLayout::Compact.cols(), 0);
            reachable.extend(keys.iter().map(|key| format!("{key:?}")));
        }
        for key in KeypadLayout::Scientific.keys() {
            assert!(
                reachable.contains(&format!("{key:?}")),
                "the narrow pages cannot reach {key:?}"
            );
        }
    }
}
