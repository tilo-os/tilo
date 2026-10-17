//! In-memory model for the calculator. This is the render/interaction
//! mirror of the CRDT document (see `doc.rs`); state is authored here and
//! flushed into Automerge so undo/redo and live collaboration work.

use crate::actions::GraphKind;
use crate::engine::{self, Ctx};
use ui::tilo_resource::{self, Model};

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
    /// Condensed label for a narrow header where the full `label` won't fit.
    pub fn short_label(self) -> &'static str {
        match self {
            Mode::Calc => "Calc",
            Mode::Graph2d => "2D",
            Mode::Graph3d => "3D",
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
    /// The graph list this mode plots, if it is a graph mode.
    pub fn graph(self) -> Option<GraphKind> {
        match self {
            Mode::Calc => None,
            Mode::Graph2d => Some(GraphKind::Curve2d),
            Mode::Graph3d => Some(GraphKind::Surface3d),
        }
    }
}

/// One evaluated entry in the calculator history.
#[derive(Model, Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    /// Calculation
    #[model(label)]
    pub expr: String,
    /// Result
    pub result: String,
}

/// A curve (`y = f(x)`) or surface (`z = f(x, y)`) row of a graph.
#[derive(Model, Clone, Debug, PartialEq)]
pub struct PlotFn {
    /// Formula
    #[model(label)]
    pub expr: String,
    /// Shown
    pub visible: bool,
}

/// 2D viewport: world-space center + pixels-per-unit zoom.
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

/// 3D viewport: orbit angles + zoom.
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

/// The whole calculator state.
#[derive(Clone, Debug, PartialEq)]
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
    /// The Graph 3D surface list: each `PlotFn` is a `z = f(x, y)` surface,
    /// mirroring how `funcs` holds the 2D `y = f(x)` curves. Multiple surfaces
    /// render as separate, per-function-coloured sheets (visibility-toggleable).
    pub surfaces: Vec<PlotFn>,
    /// Active surface row in `surfaces` (the one the editor edits) — the 3D
    /// analogue of `active_fn`.
    pub active_surface: usize,
    pub view2d: View2d,
    pub view3d: View3d,
}

impl Default for CalcModel {
    /// The live launch state: an empty calculator in Calc mode. Nothing is
    /// pre-plotted, no history, no memory — the app boots to a blank slate the
    /// user fills in. (A lived-in demo state lives behind [`CalcModel::seeded`],
    /// used only by the headless screenshot + tests.)
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

/// The default `z = f(x, y)` surface a user gets when they first open Graph 3D
/// (a starter example to explore + edit, not restored user state).
pub const DEFAULT_SURFACE: &str = "sin(sqrt(x^2+y^2))";

/// The calculations a lived-in calculator shows, newest first: the expression
/// and the result it evaluated to.
pub const DEMO_HISTORY: [(&str, &str); 5] = [
    ("12*(4+3)", "84"),
    ("sqrt(2)", "1.4142135624"),
    ("5!", "120"),
    ("log(2,1024)", "10"),
    ("2^10", "1024"),
];

/// The curves a lived-in Graph 2D shows.
pub const DEMO_CURVES: [&str; 4] = ["(x^2+1)/(x-3)", "0.25*x^2-2", "sqrt(x+6)", "sin(x)"];

/// The surfaces a lived-in Graph 3D shows: sheets at different heights that
/// never cross, so the plot reads as several surfaces rather than a lone one.
pub const DEMO_SURFACES: [&str; 2] = [DEFAULT_SURFACE, "sin(x)*cos(y)-3"];

impl CalcModel {
    /// The rows of a graph list.
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

    /// The row of a graph list its editor edits.
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

    /// Rich starting content for tests and offline demos: a populated,
    /// lived-in calculator and graphs. The live app boots empty
    /// ([`CalcModel::default`]); a capture stages the same content through
    /// the document's actions.
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

    /// The invalid-expression state a capture shows: a truthful parse error
    /// under the expression.
    pub(crate) fn apply_error_demo(&mut self) {
        self.mode = Mode::Calc;
        self.expr = "sqrt(".into();
        self.after_equals = false;
    }

    /// Evaluation context derived from the current model.
    pub fn ctx(&self) -> Ctx {
        Ctx {
            deg: self.deg,
            ans: self.ans,
            mem: self.memory,
            ..Default::default()
        }
    }

    /// Commit `=`: evaluate, push history, update Ans. Returns the committed
    /// [`HistoryEntry`] (a clone) so the caller can submit the matching
    /// `commit-history` verb — the shared, replayable, undoable path the
    /// history list is changed through. Returns
    /// `None` when there is nothing to commit (empty or unparseable expression),
    /// so no action is dispatched.
    pub fn commit_equals(&mut self) -> Option<HistoryEntry> {
        let trimmed = self.expr.trim().to_string();
        if trimmed.is_empty() {
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
            // Leave the expression intact; the result line shows the error.
            self.after_equals = false;
            None
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
            Key::Backspace => "Backspace",
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

    /// The icon a key shows in place of its label, when it has one.
    pub fn icon(&self) -> Option<ui::icons::Icon> {
        match self {
            Key::Backspace => Some(ui::icons::Icon::Delete),
            _ => None,
        }
    }

    /// What the key is, which decides its control role at draw time.
    pub fn role(&self) -> KeyRole {
        match self {
            Key::Digit(_) | Key::Dot | Key::DoubleZero => KeyRole::Digit,
            Key::Equals => KeyRole::Equals,
            Key::Clear => KeyRole::Clear,
            Key::Backspace | Key::Ins("+" | "-" | "*" | "/") => KeyRole::Operator,
            _ => KeyRole::Function,
        }
    }
}

/// What a keycap is: the number entry pad (digits and operators) stands out
/// from the scientific functions, `=` is the one primary action and `AC`
/// discards the expression.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum KeyRole {
    Digit,
    Operator,
    Function,
    Equals,
    Clear,
}

/// How dense the on-screen keypad is, chosen from the available body area so
/// the calculator stays usable from a phone-narrow strip up to a wide window.
///
/// The layouts are strictly nested supersets — every key on a smaller layout
/// is also present (in the same relative place) on a larger one — so the pad
/// grows extra columns/rows of functions outward as the window widens without
/// the core number pad ever jumping around. A "quick calculation" user always
/// sees the digits, the four basic operators, `=`, `AC` and backspace in the same
/// spot regardless of size, so responsiveness never gets in the way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeypadLayout {
    /// Phone-narrow: a 4-column essentials pad (digits, `+ − × ÷`, `=`, a few
    /// staples). No scientific column.
    Compact,
    /// Default desktop: 5 columns — essentials plus the common scientific keys
    /// (trig, powers, roots, π/e).
    Standard,
    /// Wide window: the full 6-column scientific pad with every function key.
    Scientific,
}

impl KeypadLayout {
    /// Choose a layout from the available keypad area `width` (in logical px).
    /// The breakpoints are by width because the pad grows horizontally; the
    /// thresholds bake in the per-column width the keys need to stay tappable.
    ///
    /// * `< 460 px`  → [`Compact`](Self::Compact)
    /// * `< 680 px`  → [`Standard`](Self::Standard)
    /// * `>= 680 px` → [`Scientific`](Self::Scientific)
    pub fn for_width(width: f64) -> Self {
        if width < 460.0 {
            KeypadLayout::Compact
        } else if width < 680.0 {
            KeypadLayout::Standard
        } else {
            KeypadLayout::Scientific
        }
    }

    /// Columns in this layout's grid.
    pub fn cols(self) -> usize {
        match self {
            KeypadLayout::Compact => 4,
            KeypadLayout::Standard => 5,
            KeypadLayout::Scientific => 6,
        }
    }

    /// The row-major key list for this layout. Each layout is a dense grid of
    /// `cols()` columns.
    pub fn keys(self) -> Vec<Key> {
        match self {
            KeypadLayout::Compact => Self::compact_keys(),
            KeypadLayout::Standard => Self::standard_keys(),
            KeypadLayout::Scientific => Self::scientific_keys(),
        }
    }

    /// 4 cols × 6 rows — the everyday essentials.
    fn compact_keys() -> Vec<Key> {
        use Key::{Backspace, Clear, Digit, Dot, Equals, Ins};
        vec![
            Clear,
            Backspace,
            Ins("("),
            Ins(")"),
            Ins("sin("),
            Ins("cos("),
            Ins("^"),
            Ins("sqrt("),
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

    /// 5 cols × 7 rows — essentials + common scientific keys.
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

    /// 6 cols × 7 rows — the full scientific pad.
    fn scientific_keys() -> Vec<Key> {
        use Key::{
            Ans, Backspace, Clear, Digit, Dot, DoubleZero, Equals, Ins, Percent, ToggleSign, Wrap,
        };
        vec![
            Clear,
            Backspace,
            Ins("("),
            Ins(")"),
            Ins("mod("),
            Ins("!"),
            Ins("sin("),
            Ins("cos("),
            Ins("tan("),
            Ins("pi"),
            Ins("e"),
            Percent,
            Ins("asin("),
            Ins("acos("),
            Ins("atan("),
            Ins("^"),
            Ins("sqrt("),
            Ins("/"),
            Ins("ln("),
            Ins("log10("),
            Digit('7'),
            Digit('8'),
            Digit('9'),
            Ins("*"),
            Ins("exp("),
            Ins("1/("),
            Digit('4'),
            Digit('5'),
            Digit('6'),
            Ins("-"),
            Wrap("(", ")^2"),
            ToggleSign,
            Digit('1'),
            Digit('2'),
            Digit('3'),
            Ins("+"),
            DoubleZero,
            Ans,
            Digit('0'),
            Dot,
            Ins(","),
            Equals,
        ]
    }
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

    /// Model key handling: `=` stores the evaluated value in ans/history, a digit
    /// after `=` starts fresh while an operator continues, Backspace drops a whole
    /// function name, M+ accumulates, and the error demo yields a renderable Calc
    /// parse error. Fresh model per phase.
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

    /// Keypad layouts: the default and every breakpoint layout tile their column
    /// count exactly, widths pick the expected layout with growing column counts,
    /// and every layout keeps the quick-calc essentials.
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
            // wider layouts have at least as many columns
            assert!(KeypadLayout::Compact.cols() < KeypadLayout::Standard.cols());
            assert!(KeypadLayout::Standard.cols() < KeypadLayout::Scientific.cols());
        }

        // phase every_layout_keeps_the_quick_calc_essentials
        {
            // A "quick calculation" user must always find the digits, the four
            // basic operators and `=` regardless of breakpoint.
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
}
